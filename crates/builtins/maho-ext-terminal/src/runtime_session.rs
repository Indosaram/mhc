use maho_pty::{PtyExit,PtySession,PtySessionOptions};
use std::sync::{Arc,Condvar,Mutex};
use std::time::Duration;
use crate::shared::MAX_SESSION_OUTPUT_CHARS;

#[derive(Debug,thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Pty(#[from] maho_pty::PtyError),
    #[error("terminal state lock poisoned")]
    Poisoned,
    #[error("terminal exit waiter panicked")]
    WaiterPanicked,
    #[error("terminal exit wait timed out")]
    WaitTimeout,
}

#[derive(Debug,PartialEq,Eq)]
pub struct DeltaRead {pub text:String,pub dropped_chars:usize}

#[derive(Default)]
struct OutputState {buffer:Vec<u16>,pending_utf8:Vec<u8>,dropped_chars:usize,consumed:usize}

impl OutputState {
    fn ingest(&mut self,chunk:&[u8]) {
        self.pending_utf8.extend_from_slice(chunk);
        let mut decoded=String::new();let mut consumed=0;
        while consumed<self.pending_utf8.len() {
            match std::str::from_utf8(&self.pending_utf8[consumed..]) {
                Ok(text)=>{decoded.push_str(text);consumed=self.pending_utf8.len();}
                Err(error)=>{
                    let end=consumed+error.valid_up_to();
                    if let Ok(text)=std::str::from_utf8(&self.pending_utf8[consumed..end]) {decoded.push_str(text);}
                    consumed=end;
                    if let Some(length)=error.error_len() {decoded.push('\u{fffd}');consumed+=length;} else {break;}
                }
            }
        }
        self.pending_utf8.drain(..consumed);
        self.buffer.extend(decoded.encode_utf16());
        let overflow=self.buffer.len().saturating_sub(MAX_SESSION_OUTPUT_CHARS);
        self.buffer.drain(..overflow);self.dropped_chars+=overflow;
    }

    fn read_delta(&mut self)->DeltaRead {
        let start=self.consumed.max(self.dropped_chars);
        let dropped=self.dropped_chars.saturating_sub(self.consumed);
        let text=String::from_utf16_lossy(&self.buffer[start-self.dropped_chars..]);
        self.consumed=self.dropped_chars+self.buffer.len();DeltaRead {text,dropped_chars:dropped}
    }
}

type ExitState=Arc<(Mutex<Option<Result<PtyExit,String>>>,Condvar)>;

pub struct TerminalRuntimeSession {
    pub command:String,
    session:PtySession,
    output:Arc<Mutex<OutputState>>,
    exit:ExitState,
    exit_thread:Option<std::thread::JoinHandle<()>>,
}

impl TerminalRuntimeSession {
    pub fn start(command:&str,options:PtySessionOptions)->Result<Self,RuntimeError> {
        let output=Arc::new(Mutex::new(OutputState::default()));let sink=Arc::clone(&output);
        let mut session=PtySession::start(options,move |chunk| {
            if let Ok(mut state)=sink.lock() {state.ingest(chunk);}
        })?;
        let waiter=session.wait_in_background()?;
        let exit:ExitState=Arc::new((Mutex::new(None),Condvar::new()));let settled=Arc::clone(&exit);
        let exit_thread=std::thread::spawn(move || {
            let result=match waiter.join() {Ok(result)=>result.map_err(|e|e.to_string()),Err(_)=>Err("terminal exit waiter panicked".to_owned())};
            let (lock,signal)=&*settled;
            if let Ok(mut state)=lock.lock() {*state=Some(result);signal.notify_all();}
        });
        Ok(Self {command:command.to_owned(),session,output,exit,exit_thread:Some(exit_thread)})
    }

    pub fn backend(&self)->&'static str {"native"}
    pub fn exited(&self)->Result<bool,RuntimeError> {Ok(self.exit.0.lock().map_err(|_|RuntimeError::Poisoned)?.is_some())}
    pub fn exit_result(&self)->Result<Option<PtyExit>,RuntimeError> {
        let state=self.exit.0.lock().map_err(|_|RuntimeError::Poisoned)?;
        state.as_ref().map(|result|result.clone().map_err(|error|RuntimeError::Pty(std::io::Error::other(error).into()))).transpose()
    }
    pub fn wait(&self,timeout:Duration)->Result<PtyExit,RuntimeError> {
        let (lock,signal)=&*self.exit;let state=lock.lock().map_err(|_|RuntimeError::Poisoned)?;
        let (state,_)=signal.wait_timeout_while(state,timeout,|state|state.is_none()).map_err(|_|RuntimeError::Poisoned)?;
        state.as_ref().ok_or(RuntimeError::WaitTimeout)?.clone().map_err(|error|RuntimeError::Pty(std::io::Error::other(error).into()))
    }
    pub fn total_chars(&self)->Result<usize,RuntimeError> {let state=self.output.lock().map_err(|_|RuntimeError::Poisoned)?;Ok(state.dropped_chars+state.buffer.len())}
    pub fn read_delta(&self)->Result<DeltaRead,RuntimeError> {Ok(self.output.lock().map_err(|_|RuntimeError::Poisoned)?.read_delta())}
    pub fn full_output(&self)->Result<String,RuntimeError> {Ok(String::from_utf16_lossy(&self.output.lock().map_err(|_|RuntimeError::Poisoned)?.buffer))}
    pub fn write(&mut self,bytes:&[u8])->Result<(),RuntimeError> {self.session.write(bytes)?;Ok(())}
    pub fn resize(&self,cols:u16,rows:u16)->Result<(),RuntimeError> {self.session.resize(cols,rows)?;Ok(())}
    pub fn kill(&mut self)->Result<(),RuntimeError> {self.session.kill()?;Ok(())}
    pub fn dispose(mut self)->Result<(),RuntimeError> {
        if !self.exited()? {self.kill()?;}
        if let Some(thread)=self.exit_thread.take() {thread.join().map_err(|_|RuntimeError::WaiterPanicked)?;}
        Ok(())
    }
}

impl Drop for TerminalRuntimeSession {
    fn drop(&mut self) {
        match self.exited() {
            Ok(false)=>{if let Err(error)=self.session.kill() {eprintln!("terminal cleanup failed: {error}");}}
            Ok(true)=>{},Err(error)=>eprintln!("terminal cleanup state failed: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_utf8_and_delta_cursor() {
        let mut state=OutputState::default();state.ingest(&[0xe2,0x82]);assert!(state.read_delta().text.is_empty());
        state.ingest(&[0xac,b'\n']);assert_eq!(state.read_delta(),DeltaRead {text:"\u{20ac}\n".to_owned(),dropped_chars:0});assert!(state.read_delta().text.is_empty());
    }

    #[test]
    fn retained_output_cap_reports_dropped_units() {
        let mut state=OutputState::default();state.ingest(&vec![b'x';MAX_SESSION_OUTPUT_CHARS+10]);
        let delta=state.read_delta();assert_eq!(delta.dropped_chars,10);assert_eq!(delta.text.len(),MAX_SESSION_OUTPUT_CHARS);assert!(state.read_delta().text.is_empty());
    }

    #[test]
    fn real_pty_output_exit_and_delta_are_observable()->Result<(),RuntimeError> {
        let runtime=TerminalRuntimeSession::start("printf ready",PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'ready\\n'").timeout(Duration::from_secs(5)))?;
        assert_eq!(runtime.wait(Duration::from_secs(10))?.exit_code,Some(0));assert!(runtime.exited()?);
        assert_eq!(runtime.full_output()?,"ready\r\n");assert_eq!(runtime.read_delta()?.text,"ready\r\n");assert!(runtime.read_delta()?.text.is_empty());runtime.dispose()
    }
}
