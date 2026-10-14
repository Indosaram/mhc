//! Port of senpi `packages/pty/src/pipe-fallback.ts`: a plain child-process pipe backend used
//! when `SENPI_PTY_FORCE_PIPE` is set (memory reflection/facts children set it).
use maho_pty::{PtyError,PtyExit,PtyResult,PtySessionOptions};
use std::io::{Read,Write};
use std::process::{Child,ChildStdin,Command,Stdio};
use std::sync::atomic::{AtomicBool,Ordering};
use std::sync::mpsc::{RecvTimeoutError,Sender};
use std::sync::{Arc,Mutex};
use std::thread::JoinHandle;

pub const PIPE_FALLBACK_NOTE:&str="Running with child_process pipe fallback because no PTY backend is active; terminal screen state and resize are unavailable.";

pub fn is_pipe_fallback_forced(value:Option<&str>)->bool {matches!(value,Some("1"|"true"))}

pub fn should_use_pipe_fallback(value:Option<&str>)->bool {is_pipe_fallback_forced(value)}

/// `terminateChildTree`: signal the child's whole process group on POSIX (the child leads its own
/// group, and grandchildren inheriting the pipes would otherwise keep them open), falling back to
/// a direct kill when the group is already gone.
#[cfg(unix)]
pub fn terminate_child_tree(pid:u32,signal:&str)->PtyResult<&'static str> {
    use nix::sys::signal::{Signal,kill,killpg};
    use std::str::FromStr;
    let name=signal.to_ascii_uppercase();let name=if name.starts_with("SIG") {name} else {format!("SIG{name}")};
    let signal=Signal::from_str(&name).map_err(|error|PtyError::from(std::io::Error::from(error)))?;
    let pid=nix::unistd::Pid::from_raw(i32::try_from(pid).map_err(|error|PtyError::from(std::io::Error::other(error)))?);
    if killpg(pid,signal).is_ok() {return Ok("group");}
    match kill(pid,signal) {Ok(())|Err(nix::errno::Errno::ESRCH)=>Ok("direct"),Err(error)=>Err(PtyError::from(std::io::Error::from(error)))}
}

#[cfg(not(unix))]
pub fn terminate_child_tree(pid:u32,_signal:&str)->PtyResult<&'static str> {
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    let mut killer=Command::new("taskkill.exe");killer.args(["/pid",&pid.to_string(),"/T","/F"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    killer.creation_flags(CREATE_NO_WINDOW);
    killer.spawn()?;Ok("group")
}

#[cfg(windows)]
const CREATE_NO_WINDOW:u32=0x0800_0000;

pub struct PipeFallbackSession {
    pid:u32,
    child:Option<Child>,
    stdin:Mutex<Option<ChildStdin>>,
    readers:Vec<JoinHandle<()>>,
    finished:Arc<AtomicBool>,
    kill_requested:Arc<AtomicBool>,
    timed_out:Arc<AtomicBool>,
    timeout_cancel:Option<Sender<()>>,
}

impl PipeFallbackSession {
    pub fn start(options:&PtySessionOptions,on_chunk:impl FnMut(&[u8])+Send+'static)->PtyResult<Self> {
        if options.timeout.is_some_and(|timeout|timeout.is_zero()) {return Err(PtyError::from(std::io::Error::other("Invalid timeoutMs: must be a finite positive number")));}
        let mut command=Command::new(&options.command);
        command.args(&options.args).envs(options.env.iter().cloned()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(cwd)=&options.cwd {command.current_dir(cwd);}
        // Upstream detaches on POSIX so the child leads its own group and no tty-reading program races
        // the TUI's stdin; its own process group is the safe std equivalent available here.
        #[cfg(unix)]
        {use std::os::unix::process::CommandExt;command.process_group(0);}
        #[cfg(windows)]
        {use std::os::windows::process::CommandExt;command.creation_flags(CREATE_NO_WINDOW);}
        let mut child=command.spawn().map_err(|error|PtyError::from(std::io::Error::new(error.kind(),format!("Failed to spawn pipe fallback command \"{}\": {error}",options.command))))?;
        let pid=child.id();
        let sink=Arc::new(Mutex::new(on_chunk));
        let mut readers=Vec::new();
        let streams:[Option<Box<dyn Read+Send>>;2]=[child.stdout.take().map(|stream|Box::new(stream) as Box<dyn Read+Send>),child.stderr.take().map(|stream|Box::new(stream) as Box<dyn Read+Send>)];
        for mut stream in streams.into_iter().flatten() {
            let sink=Arc::clone(&sink);
            readers.push(std::thread::spawn(move || {
                let mut buffer=[0_u8;8192];
                while let Ok(count)=stream.read(&mut buffer) {if count==0 {break;}if let Ok(mut sink)=sink.lock() {(*sink)(&buffer[..count]);}}
            }));
        }
        let finished=Arc::new(AtomicBool::new(false));let timed_out=Arc::new(AtomicBool::new(false));
        let timeout_cancel=options.timeout.map(|timeout| {
            let (sender,receiver)=std::sync::mpsc::channel::<()>();let finished=Arc::clone(&finished);let timed_out=Arc::clone(&timed_out);
            std::thread::spawn(move || {
                if receiver.recv_timeout(timeout)==Err(RecvTimeoutError::Timeout) && !finished.load(Ordering::SeqCst) {
                    timed_out.store(true,Ordering::SeqCst);
                    if let Err(error)=terminate_child_tree(pid,"SIGTERM") {maho_ext_api::diagnostic(&format!("pipe fallback timeout kill failed: {error}"));}
                }
            });
            sender
        });
        Ok(Self {pid,stdin:Mutex::new(child.stdin.take()),child:Some(child),readers,finished,kill_requested:Arc::new(AtomicBool::new(false)),timed_out,timeout_cancel})
    }

    pub fn write(&self,bytes:&[u8])->PtyResult<()> {
        if self.finished.load(Ordering::SeqCst) {return Err(PtyError::from(std::io::Error::other("Cannot write to pipe fallback stdin: session has exited.")));}
        let mut stdin=self.stdin.lock().map_err(|_|PtyError::from(std::io::Error::other("pipe fallback stdin lock poisoned")))?;
        let stdin=stdin.as_mut().ok_or_else(||PtyError::from(std::io::Error::other("Cannot write to pipe fallback stdin: child stdin is unavailable.")))?;
        stdin.write_all(bytes)?;stdin.flush()?;Ok(())
    }

    pub fn resize(&self,cols:u16,rows:u16)->PtyResult<()> {
        Err(PtyError::from(std::io::Error::other(format!("Cannot resize pipe fallback session to {cols}x{rows}: child_process pipes are not a PTY; session is still valid."))))
    }

    pub fn kill(&self,signal:&str)->PtyResult<()> {
        if self.finished.load(Ordering::SeqCst) {return Ok(());}
        self.kill_requested.store(true,Ordering::SeqCst);
        terminate_child_tree(self.pid,signal).map(|_|())
    }

    /// Settles like upstream's `close` event: after the child exits AND its stdio pipes are drained.
    pub fn wait_in_background(&mut self)->PtyResult<JoinHandle<PtyResult<PtyExit>>> {
        let mut child=self.child.take().ok_or_else(||PtyError::from(std::io::Error::other("pipe fallback session is already being waited on")))?;
        let readers=std::mem::take(&mut self.readers);let finished=Arc::clone(&self.finished);let kill_requested=Arc::clone(&self.kill_requested);let timed_out=Arc::clone(&self.timed_out);let timeout_cancel=self.timeout_cancel.take();
        Ok(std::thread::spawn(move || {
            let status=child.wait()?;
            for reader in readers {let _=reader.join();}
            finished.store(true,Ordering::SeqCst);drop(timeout_cancel);
            let timed_out=timed_out.load(Ordering::SeqCst);
            Ok(PtyExit {exit_code:status.code(),cancelled:kill_requested.load(Ordering::SeqCst)||timed_out,timed_out})
        }))
    }
}

#[cfg(all(test,unix))]
mod tests {
    use super::*;
    #[test]
    fn forced_flag_values() {assert!(should_use_pipe_fallback(Some("1")));assert!(should_use_pipe_fallback(Some("true")));assert!(!should_use_pipe_fallback(Some("0")));assert!(!should_use_pipe_fallback(None));}
    #[test]
    fn pipes_stdin_and_output_then_settles_after_exit()->PtyResult<()> {
        let (sender,receiver)=std::sync::mpsc::channel::<Vec<u8>>();
        let mut session=PipeFallbackSession::start(&PtySessionOptions::new("/bin/sh").arg("-c").arg("read value; printf 'got:%s' \"$value\"; printf 'err' >&2"),move |chunk|{let _=sender.send(chunk.to_vec());})?;
        session.write(b"hi\n")?;assert!(session.resize(10,10).is_err());
        let exit=session.wait_in_background()?.join().map_err(|_|PtyError::from(std::io::Error::other("waiter panicked")))??;
        assert_eq!(exit,PtyExit {exit_code:Some(0),cancelled:false,timed_out:false});
        let output=String::from_utf8(receiver.try_iter().flatten().collect()).map_err(std::io::Error::other)?;
        assert!(output.contains("got:hi"));assert!(output.contains("err"));assert!(session.write(b"late").is_err());Ok(())
    }
}
