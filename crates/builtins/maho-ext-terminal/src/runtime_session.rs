use maho_pty::{PtyExit,PtySession,PtySessionOptions};
use std::sync::{Arc,Condvar,Mutex};
use std::sync::atomic::{AtomicBool,Ordering};
use crate::pipe_fallback::{PipeFallbackSession,should_use_pipe_fallback};
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
    #[error("Cannot create command monitor: monitor registry is disposed.")]
    RegistryDisposed,
}

#[derive(Debug,PartialEq,Eq)]
pub struct DeltaRead {pub text:String,pub dropped_chars:usize}

#[derive(Default)]
struct OutputState {buffer:std::collections::VecDeque<u16>,pending_utf8:Vec<u8>,dropped_chars:usize,consumed:usize}

/// `String::from_utf16_lossy` over the retained ring without making it contiguous.
fn lossy_utf16(units:impl Iterator<Item=u16>)->String {char::decode_utf16(units).map(|unit|unit.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()}

pub struct OutputObserverGuard {observers:Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<String>>>>,sender:tokio::sync::mpsc::UnboundedSender<String>}
impl OutputObserverGuard {
    pub fn dispose(self)->bool {let Ok(mut observers)=self.observers.lock() else {return false;};let before=observers.len();observers.retain(|candidate|!candidate.same_channel(&self.sender));observers.len()!=before}
}

impl OutputState {
    fn ingest(&mut self,chunk:&[u8])->String {
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
        decoded
    }

    fn read_delta(&mut self)->DeltaRead {
        let start=self.consumed.max(self.dropped_chars);
        let dropped=self.dropped_chars.saturating_sub(self.consumed);
        let text=lossy_utf16(self.buffer.range(start-self.dropped_chars..).copied());
        self.consumed=self.dropped_chars+self.buffer.len();DeltaRead {text,dropped_chars:dropped}
    }

    /// Retained output from the first line start within the last `units` units, plus whether it is the
    /// whole buffer. The cut sits right after a `'\n'` so line-local sanitizing is unchanged; an OSC
    /// sequence (the only escape that can span lines) open across the cut falls back to the whole buffer.
    fn tail(&self,units:usize)->(String,bool) {
        let buffer=&self.buffer;
        let cut=buffer.len().checked_sub(units).and_then(|start|buffer.range(start..).position(|unit|*unit==u16::from(b'\n')).map(|offset|start+offset+1));
        let cut=cut.filter(|&cut|buffer.range(..cut).rposition(|unit|*unit==0x07||*unit==0x1b).is_none_or(|index|buffer.get(index+1)!=Some(&u16::from(b']'))));
        match cut {Some(cut)=>(lossy_utf16(buffer.range(cut..).copied()),false),None=>(lossy_utf16(buffer.iter().copied()),true)}
    }
}

type ExitState=Arc<(Mutex<Option<Result<PtyExit,String>>>,Condvar)>;

fn parser_snapshot(parser:&mut vt100::Parser)->Option<TerminalScreenSnapshot> {
    let screen=parser.screen_mut();let (rows,cols)=screen.size();
    let visible_grid=screen.rows(0,cols).collect();let cursor=screen.cursor_position();
    screen.set_scrollback(usize::MAX);let count=screen.scrollback();let mut scrollback=Vec::with_capacity(count);
    for offset in (1..=count).rev(){screen.set_scrollback(offset);scrollback.push(screen.rows(0,cols).next().unwrap_or_default());}
    screen.set_scrollback(0);Some(TerminalScreenSnapshot {cols,rows,visible_grid,scrollback,cursor})
}

#[derive(Debug,PartialEq,Eq)]
pub struct TerminalScreenSnapshot {
    pub cols:u16,
    pub rows:u16,
    pub visible_grid:Vec<String>,
    pub scrollback:Vec<String>,
    pub cursor:(u16,u16),
}

enum TerminalSessionBackend {Native(PtySession),PipeFallback(PipeFallbackSession)}

pub struct TerminalRuntimeSession {
    pub command:String,
    session:TerminalSessionBackend,
    kill_requested:Arc<AtomicBool>,
    output:Arc<Mutex<OutputState>>,
    exit:ExitState,
    exit_thread:Option<std::thread::JoinHandle<()>>,
    exit_signal:tokio::sync::watch::Receiver<Option<Result<PtyExit,String>>>,
    observers:Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<String>>>>,
    screen:Arc<Mutex<Option<vt100::Parser>>>,
    final_screen:Arc<Mutex<Option<TerminalScreenSnapshot>>>,
    scrollback:usize,
}

impl TerminalRuntimeSession {
    pub fn start(command:&str,options:PtySessionOptions)->Result<Self,RuntimeError> {
        Self::start_with_scrollback(command,options,crate::shared::DEFAULT_SCROLLBACK)
    }
    pub fn start_with_scrollback(command:&str,options:PtySessionOptions,scrollback:usize)->Result<Self,RuntimeError> {
        let output=Arc::new(Mutex::new(OutputState::default()));let sink=Arc::clone(&output);
        let observers:Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<String>>>>=Arc::new(Mutex::new(vec![]));
        let listeners=observers.clone();
        let screen=Arc::new(Mutex::new(Some(vt100::Parser::new(options.rows,options.cols,scrollback))));
        let projection=screen.clone();
        let on_chunk=move |chunk:&[u8]| {
            if let Ok(mut state)=sink.lock() {
                let decoded=crate::screen_text::sanitize_string(&state.ingest(chunk));
                if let Ok(mut screen)=projection.lock() && let Some(screen)=screen.as_mut() {screen.process(decoded.as_bytes());}
                if !decoded.is_empty() && let Ok(mut listeners)=listeners.lock() {
                    let mut decoded=decoded;let last=listeners.len();let mut index=0;
                    listeners.retain(|listener| {index+=1;let text=if index==last {std::mem::take(&mut decoded)} else {decoded.clone()};listener.send(text).is_ok()});
                }
            }
        };
        let (session,waiter)=if should_use_pipe_fallback(std::env::var("SENPI_PTY_FORCE_PIPE").ok().as_deref()) {
            let mut session=PipeFallbackSession::start(&options,on_chunk)?;let waiter=session.wait_in_background()?;(TerminalSessionBackend::PipeFallback(session),waiter)
        } else {
            let mut session=PtySession::start(options,on_chunk)?;let waiter=session.wait_in_background()?;(TerminalSessionBackend::Native(session),waiter)
        };
        let kill_requested=Arc::new(AtomicBool::new(false));let exit_kill_requested=Arc::clone(&kill_requested);
        let final_screen=Arc::new(Mutex::new(None));
        let exit:ExitState=Arc::new((Mutex::new(None),Condvar::new()));let settled=Arc::clone(&exit);
        let (exit_sender,exit_signal)=tokio::sync::watch::channel(None);
        let release_signal=exit_signal.clone();let release_screen=screen.clone();let released_snapshot=final_screen.clone();
        let exit_thread=std::thread::spawn(move || {
            // session-exit.ts `normalizeTerminalExit`: any requested kill (SIGTERM included) marks the exit cancelled.
            let result=match waiter.join() {Ok(result)=>result.map(|mut exit|{exit.cancelled|=exit_kill_requested.load(Ordering::SeqCst);exit}).map_err(|e|e.to_string()),Err(_)=>Err("terminal exit waiter panicked".to_owned())};
            if let Ok(mut guard)=release_screen.lock() && let Some(mut parser)=guard.take() {
                let mut snapshot=parser_snapshot(&mut parser).unwrap_or(TerminalScreenSnapshot {cols:0,rows:0,visible_grid:vec![],scrollback:vec![],cursor:(0,0)});snapshot.scrollback.clear();
                if let Ok(mut final_guard)=released_snapshot.lock(){*final_guard=Some(snapshot);}
            }
            let (lock,signal)=&*settled;
            if let Ok(mut state)=lock.lock() {*state=Some(result.clone());signal.notify_all();}
            exit_sender.send_replace(Some(result));
        });
        Ok(Self {command:command.to_owned(),session,kill_requested,output,exit,exit_thread:Some(exit_thread),exit_signal,observers,screen,final_screen,scrollback})
    }

    pub fn backend(&self)->&'static str {match self.session {TerminalSessionBackend::Native(_)=>"native",TerminalSessionBackend::PipeFallback(_)=>"pipe-fallback"}}
    pub fn subscribe_exit(&self)->tokio::sync::watch::Receiver<Option<Result<PtyExit,String>>> {self.exit_signal.clone()}
    pub fn subscribe_output(&self)->Result<(String,tokio::sync::mpsc::UnboundedReceiver<String>),RuntimeError> {
        let state=self.output.lock().map_err(|_|RuntimeError::Poisoned)?;
        let (sender,receiver)=tokio::sync::mpsc::unbounded_channel();
        self.observers.lock().map_err(|_|RuntimeError::Poisoned)?.push(sender);
        Ok((lossy_utf16(state.buffer.iter().copied()),receiver))
    }
    pub fn subscribe_output_guarded(&self)->Result<(String,tokio::sync::mpsc::UnboundedReceiver<String>,OutputObserverGuard),RuntimeError> {
        let state=self.output.lock().map_err(|_|RuntimeError::Poisoned)?;
        let (sender,receiver)=tokio::sync::mpsc::unbounded_channel();
        self.observers.lock().map_err(|_|RuntimeError::Poisoned)?.push(sender.clone());
        Ok((lossy_utf16(state.buffer.iter().copied()),receiver,OutputObserverGuard {observers:self.observers.clone(),sender}))
    }
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
    pub fn tail_output(&self,units:usize)->Result<(String,bool),RuntimeError> {Ok(self.output.lock().map_err(|_|RuntimeError::Poisoned)?.tail(units))}
    pub fn full_output(&self)->Result<String,RuntimeError> {Ok(lossy_utf16(self.output.lock().map_err(|_|RuntimeError::Poisoned)?.buffer.iter().copied()))}
    pub fn write(&mut self,bytes:&[u8])->Result<(),RuntimeError> {match &mut self.session {TerminalSessionBackend::Native(session)=>session.write(bytes)?,TerminalSessionBackend::PipeFallback(session)=>session.write(bytes)?}Ok(())}
    pub fn snapshot(&self)->Result<TerminalScreenSnapshot,RuntimeError> {
        let mut parser=self.screen.lock().map_err(|_|RuntimeError::Poisoned)?;
        if let Some(parser)=parser.as_mut(){return parser_snapshot(parser).ok_or(RuntimeError::Poisoned);}
        Ok(self.final_screen.lock().map_err(|_|RuntimeError::Poisoned)?.clone().unwrap_or(TerminalScreenSnapshot {cols:0,rows:0,visible_grid:vec![],scrollback:vec![],cursor:(0,0)}))
    }
    pub fn resize(&self,cols:u16,rows:u16)->Result<(),RuntimeError> {
        if self.exited()? {return Ok(());}
        // The PTY resize is best-effort and meaningless once the process is gone: upstream's
        // `resizeScreen` only touches the screen and its `bash_resize` returns early when
        // `runtime.exited`. Skipping it for a closed session keeps the screen projection resizable
        // instead of failing with "pty session is closed".
        // The pipe fallback rejects the resize with a note but the screen projection still resizes.
        let session_resize=if self.exited()? {Ok(())} else {match &self.session {TerminalSessionBackend::Native(session)=>session.resize(cols,rows),TerminalSessionBackend::PipeFallback(session)=>session.resize(cols,rows)}};
        let retained=lossy_utf16(self.output.lock().map_err(|_|RuntimeError::Poisoned)?.buffer.iter().copied());
        let replay_limit=crate::screen_text::normalize_replay_history_length(
            usize::from(cols),usize::from(rows),self.scrollback);
        let retained=if retained.encode_utf16().count()>replay_limit {crate::screen_text::sanitized_tail(&retained,replay_limit)} else {retained};
        let mut screen=self.screen.lock().map_err(|_|RuntimeError::Poisoned)?;
        if let Some(screen)=screen.as_mut(){
            if retained.is_empty() {
                screen.screen_mut().set_size(rows,cols);
            } else {
                let mut rebuilt=vt100::Parser::new(rows,cols,self.scrollback);
                rebuilt.process(retained.as_bytes());
                *screen=rebuilt;
            }
        }
        session_resize?;Ok(())
    }
    /// session.ts `stop()`: the default SIGTERM (registry-session.ts `DEFAULT_SIGNAL`), letting the
    /// process tree clean up before a caller escalates to `kill`.
    pub fn stop(&mut self)->Result<(),RuntimeError> {self.signal("SIGTERM")}
    pub fn kill(&mut self)->Result<(),RuntimeError> {self.signal("SIGKILL")}
    fn signal(&mut self,signal:&str)->Result<(),RuntimeError> {
        self.kill_requested.store(true,Ordering::SeqCst);
        let result=match &mut self.session {TerminalSessionBackend::Native(session) if signal=="SIGKILL"=>session.kill(),TerminalSessionBackend::Native(session)=>session.signal(signal),TerminalSessionBackend::PipeFallback(session)=>session.kill(signal)};
        // A session that exited between the state check and the signal is already stopped.
        match result {Ok(())=>Ok(()),Err(error) if error.to_string()=="pty session is closed"||error.to_string().contains("No such process") =>Ok(()),Err(_) if self.exited()? =>Ok(()),Err(error)=>Err(error.into())}
    }
    pub fn dispose(mut self)->Result<(),RuntimeError> {
        if !self.exited()? {self.kill()?;}
        // Only join a settled waiter: a descendant still holding the PTY keeps the reader (and so the
        // waiter) blocked, and disposal must not hang on it (upstream dispose never waits).
        if self.exited()? && let Some(thread)=self.exit_thread.take() {thread.join().map_err(|_|RuntimeError::WaiterPanicked)?;}
        Ok(())
    }
}

impl Drop for TerminalRuntimeSession {
    fn drop(&mut self) {
        match self.exited() {
            Ok(false)=>{if let Err(error)=self.kill() {maho_ext_api::diagnostic(&format!("terminal cleanup failed: {error}"));}}
            Ok(true)=>{},Err(error)=>maho_ext_api::diagnostic(&format!("terminal cleanup state failed: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn resize_preserves_screen_history_even_when_log_buffer_is_empty()->Result<(),RuntimeError> {
        let mut runtime=TerminalRuntimeSession::start("resize",PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; read start; printf 'screen-kept'; read end"))?;
        let (_,mut output)=runtime.subscribe_output()?;runtime.write(b"begin\n")?;
        tokio::time::timeout(Duration::from_secs(5),async {let mut observed=String::new();while !observed.contains("screen-kept") {observed.push_str(&output.recv().await.unwrap());}}).await.unwrap();
        runtime.output.lock().unwrap().buffer.clear();runtime.resize(100,30)?;
        assert!(runtime.snapshot()?.visible_grid.iter().any(|line|line.contains("screen-kept")));runtime.dispose()
    }
    #[test]
    fn snapshot_exposes_scrollback_and_restores_visible_view()->Result<(),RuntimeError> {
        let runtime=TerminalRuntimeSession::start("screen",PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'one\r\ntwo\r\nthree'").size(20,2))?;
        runtime.wait(Duration::from_secs(5))?;let snapshot=runtime.snapshot()?;
        assert_eq!(snapshot.scrollback,["one"]);assert_eq!(snapshot.visible_grid,["two","three"]);assert_eq!(runtime.snapshot()?,snapshot);runtime.dispose()
    }

    #[test]
    fn resize_updates_geometry_and_retains_prior_screen_content()->Result<(),RuntimeError> {
        let runtime=TerminalRuntimeSession::start("screen",PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'one\r\ntwo\r\nthree'").size(20,2))?;
        runtime.wait(Duration::from_secs(5))?;
        let before=runtime.snapshot()?;runtime.resize(40,6)?;let after=runtime.snapshot()?;
        assert_eq!(after,before);assert!(runtime.full_output()?.contains("one"));
        runtime.dispose()
    }

    #[test]
    fn resize_preserves_operation_history_at_the_new_geometry()->Result<(),RuntimeError> {
        let runtime=TerminalRuntimeSession::start("wrap",PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'").size(20,10))?;
        runtime.wait(Duration::from_secs(5))?;
        let before=runtime.snapshot()?;assert_eq!(before.cols,20);assert!(before.visible_grid.iter().filter(|line|line.contains('A')).count()>=3);
        runtime.resize(60,10)?;let after=runtime.snapshot()?;
        assert_eq!(after.cols,60);assert!(after.visible_grid.iter().any(|line|line.contains('A')),"resized screen: {after:?}");
        runtime.dispose()
    }

    #[test]
    fn exited_session_retains_final_screen_without_scrollback()->Result<(),RuntimeError> {
        let runtime=TerminalRuntimeSession::start("screen",PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'one\\r\\ntwo\\r\\nthree'").size(20,2))?;
        runtime.wait(Duration::from_secs(5))?;let snapshot=runtime.snapshot()?;
        assert!(snapshot.scrollback.is_empty());assert!(snapshot.visible_grid.iter().any(|line|line.contains("three")));
        assert_eq!(runtime.snapshot()?,snapshot);runtime.dispose()
    }

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

    #[test]
    fn tail_cuts_at_line_start_unless_osc_spans_the_cut() {
        let mut state=OutputState::default();state.ingest(b"aaaa\nbbbb\ncccc");
        assert_eq!(state.tail(7),("cccc".to_owned(),false));assert_eq!(state.tail(100),("aaaa\nbbbb\ncccc".to_owned(),true));
        let mut open=OutputState::default();open.ingest(b"x\n\x1b]0;t\ncccc\x07");
        assert!(open.tail(6).1);
    }

    #[tokio::test]
    async fn atomic_subscription_covers_history_and_future_without_cursor_interference()->Result<(),RuntimeError> {
        let mut runtime=TerminalRuntimeSession::start("read input",PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'ready\\n'; read value; printf '%s\\n' \"$value\"").timeout(Duration::from_secs(5)))?;
        let (mut output,mut receiver)=runtime.subscribe_output()?;
        tokio::time::timeout(Duration::from_secs(5),async {while !output.contains("ready\r\n") {output.push_str(&receiver.recv().await.expect("output sender"));}}).await.map_err(|_|RuntimeError::WaitTimeout)?;
        runtime.write(b"next\n")?;
        tokio::time::timeout(Duration::from_secs(5),async {while !output.contains("next\r\n") {output.push_str(&receiver.recv().await.expect("output sender"));}}).await.map_err(|_|RuntimeError::WaitTimeout)?;
        runtime.wait(Duration::from_secs(5))?;
        assert_eq!(output,"ready\r\nnext\r\n");assert_eq!(runtime.read_delta()?.text,output);
        runtime.dispose()
    }

    #[tokio::test]
    async fn disposed_output_observer_stops_receiving_after_history_drain()->Result<(),RuntimeError> {
        let mut runtime=TerminalRuntimeSession::start("read input",PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'ready\\n'; read value; printf '%s\\n' \"$value\"").timeout(Duration::from_secs(5)))?;
        let (mut output,mut receiver,guard)=runtime.subscribe_output_guarded()?;
        tokio::time::timeout(Duration::from_secs(5),async {while !output.contains("ready\r\n") {output.push_str(&receiver.recv().await.expect("output sender"));}}).await.map_err(|_|RuntimeError::WaitTimeout)?;
        while receiver.try_recv().is_ok() {}
        assert!(guard.dispose());
        runtime.write(b"next\n")?;runtime.wait(Duration::from_secs(5))?;
        assert!(receiver.try_recv().is_err());runtime.dispose()
    }
}
