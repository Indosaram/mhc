use crate::pipe_fallback::{PipeFallbackSession, should_use_pipe_fallback};
use crate::shared::MAX_SESSION_OUTPUT_CHARS;
use maho_pty::{PtyExit, PtySession, PtySessionOptions};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
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

#[derive(Debug, PartialEq, Eq)]
pub struct DeltaRead {
    pub text: String,
    pub dropped_chars: usize,
}

#[derive(Default)]
struct OutputState {
    buffer: std::collections::VecDeque<u16>,
    pending_utf8: Vec<u8>,
    dropped_chars: usize,
    consumed: usize,
}

/// `String::from_utf16_lossy` over the retained ring without making it contiguous.
fn lossy_utf16(units: impl Iterator<Item = u16>) -> String {
    char::decode_utf16(units)
        .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

pub struct OutputObserverGuard {
    observers: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<String>>>>,
    sender: tokio::sync::mpsc::UnboundedSender<String>,
}
impl OutputObserverGuard {
    pub fn dispose(self) -> bool {
        let Ok(mut observers) = self.observers.lock() else {
            return false;
        };
        let before = observers.len();
        observers.retain(|candidate| !candidate.same_channel(&self.sender));
        observers.len() != before
    }
}

impl OutputState {
    #[cfg(test)]
    fn ingest(&mut self, chunk: &[u8]) -> String {
        let decoded = self.decode(chunk);
        self.append(&decoded);
        decoded
    }

    /// `TextDecoder.decode(chunk, { stream: true })`: a split UTF-8 sequence waits for its tail.
    fn decode(&mut self, chunk: &[u8]) -> String {
        self.pending_utf8.extend_from_slice(chunk);
        let mut decoded = String::new();
        let mut consumed = 0;
        while consumed < self.pending_utf8.len() {
            match std::str::from_utf8(&self.pending_utf8[consumed..]) {
                Ok(text) => {
                    decoded.push_str(text);
                    consumed = self.pending_utf8.len();
                }
                Err(error) => {
                    let end = consumed + error.valid_up_to();
                    if let Ok(text) = std::str::from_utf8(&self.pending_utf8[consumed..end]) {
                        decoded.push_str(text);
                    }
                    consumed = end;
                    if let Some(length) = error.error_len() {
                        decoded.push('\u{fffd}');
                        consumed += length;
                    } else {
                        break;
                    }
                }
            }
        }
        self.pending_utf8.drain(..consumed);
        decoded
    }

    /// `TextDecoder.decode()` at end of stream: an unfinished sequence becomes U+FFFD.
    fn decode_end(&mut self) -> String {
        let tail = String::from_utf8_lossy(&self.pending_utf8).into_owned();
        self.pending_utf8.clear();
        tail
    }

    fn append(&mut self, text: &str) {
        self.buffer.extend(text.encode_utf16());
        let overflow = self.buffer.len().saturating_sub(MAX_SESSION_OUTPUT_CHARS);
        self.buffer.drain(..overflow);
        self.dropped_chars += overflow;
    }

    fn read_delta(&mut self) -> DeltaRead {
        let start = self.consumed.max(self.dropped_chars);
        let dropped = self.dropped_chars.saturating_sub(self.consumed);
        let text = lossy_utf16(self.buffer.range(start - self.dropped_chars..).copied());
        self.consumed = self.dropped_chars + self.buffer.len();
        DeltaRead {
            text,
            dropped_chars: dropped,
        }
    }

    /// Retained output from the first line start within the last `units` units, plus whether it is the
    /// whole buffer. The cut sits right after a `'\n'` so line-local sanitizing is unchanged; an OSC
    /// sequence (the only escape that can span lines) open across the cut falls back to the whole buffer.
    fn tail(&self, units: usize) -> (String, bool) {
        let buffer = &self.buffer;
        let cut = buffer.len().checked_sub(units).and_then(|start| {
            buffer
                .range(start..)
                .position(|unit| *unit == u16::from(b'\n'))
                .map(|offset| start + offset + 1)
        });
        let cut = cut.filter(|&cut| {
            buffer
                .range(..cut)
                .rposition(|unit| *unit == 0x07 || *unit == 0x1b)
                .is_none_or(|index| buffer.get(index + 1) != Some(&u16::from(b']')))
        });
        match cut {
            Some(cut) => (lossy_utf16(buffer.range(cut..).copied()), false),
            None => (lossy_utf16(buffer.iter().copied()), true),
        }
    }
}

type ExitState = Arc<(Mutex<Option<Result<PtyExit, String>>>, Condvar)>;
type OutputObservers = Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<String>>>>;

/// runtime-session.ts `ingest`: retain the text, feed the screen and hand it to the output listeners.
fn deliver(
    state: &mut OutputState,
    text: String,
    screen: &Mutex<Option<crate::screen::TerminalScreen>>,
    listeners: &OutputObservers,
) {
    if text.is_empty() {
        return;
    }
    state.append(&text);
    let decoded = crate::screen_text::sanitize_string(&text);
    if let Ok(mut screen) = screen.lock()
        && let Some(screen) = screen.as_mut()
    {
        screen.feed(&decoded);
    }
    if !decoded.is_empty()
        && let Ok(mut listeners) = listeners.lock()
    {
        let mut decoded = decoded;
        let last = listeners.len();
        let mut index = 0;
        listeners.retain(|listener| {
            index += 1;
            let text = if index == last {
                std::mem::take(&mut decoded)
            } else {
                decoded.clone()
            };
            listener.send(text).is_ok()
        });
    }
}

/// How long decoded output may sit in the redactor before held ordinary text is published (upstream `setTimeout(.., 50)`).
const REDACTION_IDLE_FLUSH: Duration = Duration::from_millis(50);

/// `redactSecretsInOutput`: the session's `SecretOutputRedactor` plus the idle-flush timer state. `pushes` counts
/// every push so the flusher restarts its 50 ms window on new output; `done` ends the flusher (exit or dispose).
#[derive(Default)]
struct RedactionState {
    redactor: maho_tools::secret_output_redactor::SecretOutputRedactor,
    pushes: u64,
    pending: bool,
    done: bool,
}
type Redaction = Arc<(Mutex<RedactionState>, Condvar)>;

/// The idle timer: 50 ms after the last push, publish what `flushIdle` releases. Lock order is output, then
/// redaction, the same order the data path takes.
fn spawn_redaction_flusher(
    redaction: Redaction,
    output: Arc<Mutex<OutputState>>,
    screen: Arc<Mutex<Option<crate::screen::TerminalScreen>>>,
    listeners: OutputObservers,
) {
    std::thread::spawn(move || {
        loop {
            let pushes = {
                let (lock, signal) = &*redaction;
                let guard = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let guard = signal
                    .wait_while(guard, |state| !state.pending && !state.done)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if guard.done {
                    return;
                }
                let pushes = guard.pushes;
                let (guard, _) = signal
                    .wait_timeout_while(guard, REDACTION_IDLE_FLUSH, |state| {
                        !state.done && state.pushes == pushes
                    })
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if guard.done {
                    return;
                }
                if guard.pushes != pushes {
                    continue;
                }
                pushes
            };
            let Ok(mut state) = output.lock() else {
                return;
            };
            let text = {
                let mut guard = redaction
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if guard.done {
                    return;
                }
                if guard.pushes != pushes {
                    continue;
                }
                guard.pending = false;
                guard.redactor.flush_idle()
            };
            deliver(&mut state, text, &screen, &listeners);
        }
    });
}

/// The PTY data path (`onData`): decode, pass through the redactor when it is on (arming the idle
/// timer), then retain, project and publish what it releases.
fn ingest_chunk(
    state: &mut OutputState,
    chunk: &[u8],
    redaction: Option<&Redaction>,
    screen: &Mutex<Option<crate::screen::TerminalScreen>>,
    listeners: &OutputObservers,
) {
    let decoded = state.decode(chunk);
    let text = match redaction {
        Some(redaction) => {
            let mut guard = redaction
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let text = guard.redactor.push(&decoded);
            guard.pushes += 1;
            guard.pending = true;
            drop(guard);
            redaction.1.notify_all();
            text
        }
        None => decoded,
    };
    deliver(state, text, screen, listeners);
}

/// `onExit`: stop the idle timer and publish what the redactor still holds (`push(decode()) + finish()`).
fn finish_redaction(
    state: &mut OutputState,
    redaction: &Redaction,
    screen: &Mutex<Option<crate::screen::TerminalScreen>>,
    listeners: &OutputObservers,
) {
    let tail = state.decode_end();
    let text = {
        let mut guard = redaction
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.done = true;
        let pushed = guard.redactor.push(&tail);
        pushed + &guard.redactor.finish()
    };
    redaction.1.notify_all();
    deliver(state, text, screen, listeners);
}

fn stop_redaction(redaction: Option<&Redaction>) {
    if let Some(redaction) = redaction {
        redaction
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .done = true;
        redaction.1.notify_all();
    }
}

// Both notification surfaces share one linearization point, including poisoned cleanup.
fn settle_exit(
    exit: &ExitState,
    sender: &tokio::sync::watch::Sender<Option<Result<PtyExit, String>>>,
    result: Result<PtyExit, String>,
) {
    let mut state = exit
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.is_some() {
        return;
    }
    *state = Some(result.clone());
    sender.send_replace(Some(result));
    exit.1.notify_all();
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalScreenSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub visible_grid: Vec<String>,
    pub scrollback: Vec<String>,
    pub cursor: (u16, u16),
}

enum TerminalSessionBackend {
    Native(PtySession),
    PipeFallback(PipeFallbackSession),
}

pub struct TerminalRuntimeSession {
    pub command: String,
    session: TerminalSessionBackend,
    kill_requested: Arc<AtomicBool>,
    output: Arc<Mutex<OutputState>>,
    exit: ExitState,
    exit_thread: Option<std::thread::JoinHandle<()>>,
    exit_signal: tokio::sync::watch::Receiver<Option<Result<PtyExit, String>>>,
    exit_sender: tokio::sync::watch::Sender<Option<Result<PtyExit, String>>>,
    observers: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<String>>>>,
    screen: Arc<Mutex<Option<crate::screen::TerminalScreen>>>,
    final_screen: Arc<Mutex<Option<TerminalScreenSnapshot>>>,
    redaction: Option<Redaction>,
}

impl TerminalRuntimeSession {
    pub fn start(command: &str, options: PtySessionOptions) -> Result<Self, RuntimeError> {
        Self::start_with_scrollback(command, options, crate::shared::DEFAULT_SCROLLBACK)
    }
    pub fn start_with_scrollback(
        command: &str,
        options: PtySessionOptions,
        scrollback: usize,
    ) -> Result<Self, RuntimeError> {
        Self::start_with_runtime_options(command, options, scrollback, false)
    }
    /// `new TerminalRuntimeSession(command, { ...options, scrollback, redactSecretsInOutput })`: with redaction on,
    /// decoded PTY text goes through a `SecretOutputRedactor` before the buffer, the screen and the output listeners
    /// see it; held text is published after 50 ms idle and the rest on exit.
    pub fn start_with_runtime_options(
        command: &str,
        options: PtySessionOptions,
        scrollback: usize,
        redact_secrets_in_output: bool,
    ) -> Result<Self, RuntimeError> {
        let output = Arc::new(Mutex::new(OutputState::default()));
        let sink = Arc::clone(&output);
        let observers: OutputObservers = Arc::new(Mutex::new(vec![]));
        let listeners = observers.clone();
        let screen = Arc::new(Mutex::new(Some(crate::screen::TerminalScreen::new(
            options.cols,
            options.rows,
            scrollback,
        ))));
        let projection = screen.clone();
        let redaction: Option<Redaction> = redact_secrets_in_output
            .then(|| Arc::new((Mutex::new(RedactionState::default()), Condvar::new())));
        if let Some(redaction) = &redaction {
            spawn_redaction_flusher(
                redaction.clone(),
                output.clone(),
                screen.clone(),
                observers.clone(),
            );
        }
        let chunk_redaction = redaction.clone();
        let on_chunk = move |chunk: &[u8]| {
            if let Ok(mut state) = sink.lock() {
                ingest_chunk(
                    &mut state,
                    chunk,
                    chunk_redaction.as_ref(),
                    &projection,
                    &listeners,
                );
            }
        };
        let (session, waiter) =
            if should_use_pipe_fallback(std::env::var("SENPI_PTY_FORCE_PIPE").ok().as_deref()) {
                let mut session = PipeFallbackSession::start(&options, on_chunk)?;
                let waiter = session.wait_in_background()?;
                (TerminalSessionBackend::PipeFallback(session), waiter)
            } else {
                let mut session = PtySession::start(options, on_chunk)?;
                let waiter = session.wait_in_background()?;
                (TerminalSessionBackend::Native(session), waiter)
            };
        let kill_requested = Arc::new(AtomicBool::new(false));
        let exit_kill_requested = Arc::clone(&kill_requested);
        let final_screen = Arc::new(Mutex::new(None));
        let exit: ExitState = Arc::new((Mutex::new(None), Condvar::new()));
        let settled = Arc::clone(&exit);
        let (exit_sender, exit_signal) = tokio::sync::watch::channel(None);
        let completion_sender = exit_sender.clone();
        let release_screen = screen.clone();
        let released_snapshot = final_screen.clone();
        let exit_redaction = redaction.clone();
        let exit_output = output.clone();
        let exit_listeners = observers.clone();
        let exit_thread = std::thread::spawn(move || {
            // session-exit.ts `normalizeTerminalExit`: any requested kill (SIGTERM included) marks the exit cancelled.
            let result = match waiter.join() {
                Ok(Ok(mut exit)) => {
                    exit.cancelled |= exit_kill_requested.load(Ordering::SeqCst);
                    Ok(exit)
                }
                Ok(Err(_)) if exit_kill_requested.load(Ordering::SeqCst) => Ok(PtyExit {
                    exit_code: None,
                    cancelled: true,
                    timed_out: false,
                }),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) if exit_kill_requested.load(Ordering::SeqCst) => Ok(PtyExit {
                    exit_code: None,
                    cancelled: true,
                    timed_out: false,
                }),
                Err(_) => Err("terminal exit waiter panicked".to_owned()),
            };
            // onExit: stop the idle timer and publish what the redactor still holds (`push(decode()) + finish()`).
            if let Some(redaction) = &exit_redaction
                && let Ok(mut state) = exit_output.lock()
            {
                finish_redaction(&mut state, redaction, &release_screen, &exit_listeners);
            }
            if let Ok(mut guard) = release_screen.lock()
                && let Some(mut parser) = guard.take()
            {
                let mut snapshot = parser.snapshot();
                snapshot.scrollback.clear();
                if let Ok(mut final_guard) = released_snapshot.lock() {
                    *final_guard = Some(snapshot);
                }
            }
            settle_exit(&settled, &completion_sender, result);
        });
        Ok(Self {
            command: command.to_owned(),
            session,
            kill_requested,
            output,
            exit,
            exit_thread: Some(exit_thread),
            exit_signal,
            exit_sender,
            observers,
            screen,
            final_screen,
            redaction,
        })
    }

    /// The pid of the PTY's process (the session's group leader on Unix).
    pub fn pid(&self) -> Option<u32> {
        match &self.session {
            TerminalSessionBackend::Native(session) => session.pid(),
            TerminalSessionBackend::PipeFallback(session) => Some(session.pid()),
        }
    }

    pub fn backend(&self) -> &'static str {
        match self.session {
            TerminalSessionBackend::Native(_) => "native",
            TerminalSessionBackend::PipeFallback(_) => "pipe-fallback",
        }
    }
    pub fn subscribe_exit(&self) -> tokio::sync::watch::Receiver<Option<Result<PtyExit, String>>> {
        self.exit_signal.clone()
    }
    pub fn subscribe_output(
        &self,
    ) -> Result<(String, tokio::sync::mpsc::UnboundedReceiver<String>), RuntimeError> {
        let state = self.output.lock().map_err(|_| RuntimeError::Poisoned)?;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        self.observers
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .push(sender);
        Ok((lossy_utf16(state.buffer.iter().copied()), receiver))
    }
    pub fn subscribe_output_guarded(
        &self,
    ) -> Result<
        (
            String,
            tokio::sync::mpsc::UnboundedReceiver<String>,
            OutputObserverGuard,
        ),
        RuntimeError,
    > {
        let state = self.output.lock().map_err(|_| RuntimeError::Poisoned)?;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        self.observers
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .push(sender.clone());
        Ok((
            lossy_utf16(state.buffer.iter().copied()),
            receiver,
            OutputObserverGuard {
                observers: self.observers.clone(),
                sender,
            },
        ))
    }
    pub fn exited(&self) -> Result<bool, RuntimeError> {
        Ok(self
            .exit
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some())
    }
    pub fn exit_result(&self) -> Result<Option<PtyExit>, RuntimeError> {
        let state = self
            .exit
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .as_ref()
            .map(|result| {
                result
                    .clone()
                    .map_err(|error| RuntimeError::Pty(std::io::Error::other(error).into()))
            })
            .transpose()
    }
    pub fn wait(&self, timeout: Duration) -> Result<PtyExit, RuntimeError> {
        let (lock, signal) = &*self.exit;
        let state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (state, _) = signal
            .wait_timeout_while(state, timeout, |state| state.is_none())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .as_ref()
            .ok_or(RuntimeError::WaitTimeout)?
            .clone()
            .map_err(|error| RuntimeError::Pty(std::io::Error::other(error).into()))
    }
    pub fn total_chars(&self) -> Result<usize, RuntimeError> {
        let state = self.output.lock().map_err(|_| RuntimeError::Poisoned)?;
        Ok(state.dropped_chars + state.buffer.len())
    }
    pub fn read_delta(&self) -> Result<DeltaRead, RuntimeError> {
        Ok(self
            .output
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .read_delta())
    }
    pub fn tail_output(&self, units: usize) -> Result<(String, bool), RuntimeError> {
        Ok(self
            .output
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .tail(units))
    }
    pub fn full_output(&self) -> Result<String, RuntimeError> {
        Ok(lossy_utf16(
            self.output
                .lock()
                .map_err(|_| RuntimeError::Poisoned)?
                .buffer
                .iter()
                .copied(),
        ))
    }
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), RuntimeError> {
        match &mut self.session {
            TerminalSessionBackend::Native(session) => session.write(bytes)?,
            TerminalSessionBackend::PipeFallback(session) => session.write(bytes)?,
        }
        Ok(())
    }
    pub fn snapshot(&self) -> Result<TerminalScreenSnapshot, RuntimeError> {
        let mut parser = self.screen.lock().map_err(|_| RuntimeError::Poisoned)?;
        if let Some(parser) = parser.as_mut() {
            return Ok(parser.snapshot());
        }
        Ok(self
            .final_screen
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .clone()
            .unwrap_or(TerminalScreenSnapshot {
                cols: 0,
                rows: 0,
                visible_grid: vec![],
                scrollback: vec![],
                cursor: (0, 0),
            }))
    }
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), RuntimeError> {
        // The PTY resize is best-effort and meaningless once the process is gone. The pipe fallback
        // rejects the resize with a note but the screen projection still resizes.
        let session_resize = if self.exited()? {
            Ok(())
        } else {
            match &self.session {
                TerminalSessionBackend::Native(session) => session.resize(cols, rows),
                TerminalSessionBackend::PipeFallback(session) => session.resize(cols, rows),
            }
        };
        // runtime-session.ts `resizeScreen`: the screen replays its own operation history at the new
        // geometry (pi-pty screen.ts), independent of the log buffer and its read cursor.
        if let Some(screen) = self
            .screen
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .as_mut()
        {
            screen.resize(cols, rows);
        }
        session_resize?;
        Ok(())
    }
    /// session.ts `stop()`: the default SIGTERM (registry-session.ts `DEFAULT_SIGNAL`), letting the
    /// process tree clean up before a caller escalates to `kill`.
    pub fn stop(&mut self) -> Result<(), RuntimeError> {
        self.signal("SIGTERM")
    }
    pub fn kill(&mut self) -> Result<(), RuntimeError> {
        self.signal("SIGKILL")
    }
    fn signal(&mut self, signal: &str) -> Result<(), RuntimeError> {
        self.kill_requested.store(true, Ordering::SeqCst);
        let result = match &mut self.session {
            TerminalSessionBackend::Native(session) if signal == "SIGKILL" => session.kill(),
            TerminalSessionBackend::Native(session) => session.signal(signal),
            TerminalSessionBackend::PipeFallback(session) => session.kill(signal),
        };
        // A session that exited between the state check and the signal is already stopped.
        match result {
            Ok(()) => Ok(()),
            Err(error)
                if error.to_string() == "pty session is closed"
                    || error.to_string().contains("No such process")
                    || (cfg!(windows)
                        && (error.to_string().contains("Access is denied")
                            || error.to_string().contains("os error 5")
                            || error.to_string().contains("os error 6")
                            || error.to_string().contains("os error 2")
                            || error.to_string().contains("os error 258"))) =>
            {
                Ok(())
            }
            Err(_) if self.exited()? => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn dispose(mut self) -> Result<(), RuntimeError> {
        let cleanup = if !self.exited()? { self.kill() } else { Ok(()) };
        // Only join a settled waiter: a descendant still holding the PTY keeps the reader (and so the
        // waiter) blocked, and disposal must not hang on it (upstream dispose never waits).
        self.settle_cancelled();
        cleanup?;
        if self
            .exit_thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
            && let Some(thread) = self.exit_thread.take()
        {
            thread.join().map_err(|_| RuntimeError::WaiterPanicked)?;
        }
        Ok(())
    }
    pub(crate) fn settle_cancelled(&self) {
        settle_exit(
            &self.exit,
            &self.exit_sender,
            Ok(PtyExit {
                exit_code: None,
                cancelled: true,
                timed_out: false,
            }),
        );
    }
}

impl Drop for TerminalRuntimeSession {
    fn drop(&mut self) {
        // dispose(): clear the idle timer so the flusher thread ends with the session.
        stop_redaction(self.redaction.as_ref());
        match self.exited() {
            Ok(false) => {
                if let Err(error) = self.kill() {
                    maho_ext_api::diagnostic(&format!("terminal cleanup failed: {error}"));
                }
            }
            Ok(true) => {}
            Err(error) => {
                maho_ext_api::diagnostic(&format!("terminal cleanup state failed: {error}"))
            }
        }
        self.settle_cancelled();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn poisoned_exit_settles_both_notification_surfaces_once() {
        let mut runtime =
            TerminalRuntimeSession::start("read", crate::shared::test_sh_options("read value"))
                .unwrap();
        let mut exit = runtime.subscribe_exit();
        let state = runtime.exit.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = state.0.lock().unwrap();
                panic!("poison exit state");
            })
            .join()
            .is_err()
        );
        runtime.write(b"done\n").unwrap();
        let settled = tokio::time::timeout(Duration::from_secs(5), exit.wait_for(Option::is_some))
            .await
            .unwrap()
            .unwrap()
            .clone()
            .unwrap()
            .unwrap();
        assert_eq!(settled.exit_code, Some(0));
        assert_eq!(runtime.wait(Duration::ZERO).unwrap(), settled);
        runtime.settle_cancelled();
        assert!(!exit.has_changed().unwrap());
        assert_eq!(runtime.exit_result().unwrap(), Some(settled));
        runtime.dispose().unwrap();
    }
    #[tokio::test]
    async fn disposal_releases_subscribed_waiters_once() {
        let runtime =
            TerminalRuntimeSession::start("read", crate::shared::test_sh_options("read value"))
                .unwrap();
        let mut exit = runtime.subscribe_exit();
        runtime.dispose().unwrap();
        let settled = tokio::time::timeout(Duration::from_secs(5), exit.wait_for(Option::is_some))
            .await
            .unwrap()
            .unwrap()
            .clone()
            .unwrap()
            .unwrap();
        assert!(settled.cancelled);
        assert!(!exit.has_changed().unwrap_or(false));
    }
    #[tokio::test]
    async fn cancellation_cannot_be_overwritten_by_late_reaping() {
        let exit: ExitState = Arc::new((Mutex::new(None), Condvar::new()));
        let (sender, mut receiver) = tokio::sync::watch::channel(None);
        let cancelled = Ok(PtyExit {
            exit_code: None,
            cancelled: true,
            timed_out: false,
        });
        settle_exit(&exit, &sender, cancelled.clone());
        tokio::time::timeout(Duration::from_secs(5), receiver.wait_for(Option::is_some))
            .await
            .unwrap()
            .unwrap();
        settle_exit(
            &exit,
            &sender,
            Ok(PtyExit {
                exit_code: Some(0),
                cancelled: false,
                timed_out: false,
            }),
        );
        assert_eq!(*receiver.borrow(), Some(cancelled));
        assert!(!receiver.has_changed().unwrap());
    }
    #[tokio::test]
    async fn resize_preserves_screen_history_even_when_log_buffer_is_empty()
    -> Result<(), RuntimeError> {
        let mut runtime = TerminalRuntimeSession::start(
            "resize",
            crate::shared::test_sh_options(
                "stty -echo; read start; printf 'screen-kept'; read end",
            ),
        )?;
        let (_, mut output) = runtime.subscribe_output()?;
        runtime.write(b"begin\n")?;
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut observed = String::new();
            while !observed.contains("screen-kept") {
                observed.push_str(&output.recv().await.unwrap());
            }
        })
        .await
        .unwrap();
        runtime.output.lock().unwrap().buffer.clear();
        runtime.resize(100, 30)?;
        assert!(
            runtime
                .snapshot()?
                .visible_grid
                .iter()
                .any(|line| line.contains("screen-kept"))
        );
        runtime.dispose()
    }
    #[test]
    fn snapshot_exposes_scrollback_and_restores_visible_view() -> Result<(), RuntimeError> {
        let runtime = TerminalRuntimeSession::start(
            "screen",
            crate::shared::test_sh_options("stty -echo; printf 'one\r\ntwo\r\nthree'").size(20, 2),
        )?;
        runtime.wait(Duration::from_secs(5))?;
        let snapshot = runtime.snapshot()?;
        assert!(snapshot.scrollback.is_empty());
        assert_eq!(snapshot.visible_grid, ["two", "three"]);
        assert_eq!(runtime.snapshot()?, snapshot);
        runtime.dispose()
    }

    #[test]
    fn resize_updates_geometry_and_retains_prior_screen_content() -> Result<(), RuntimeError> {
        let runtime = TerminalRuntimeSession::start(
            "screen",
            crate::shared::test_sh_options("stty -echo; printf 'one\r\ntwo\r\nthree'").size(20, 2),
        )?;
        runtime.wait(Duration::from_secs(5))?;
        let before = runtime.snapshot()?;
        runtime.resize(40, 6)?;
        let after = runtime.snapshot()?;
        assert_eq!(after, before);
        assert!(runtime.full_output()?.contains("one"));
        runtime.dispose()
    }

    #[test]
    fn resize_preserves_operation_history_at_the_new_geometry() -> Result<(), RuntimeError> {
        let runtime = TerminalRuntimeSession::start(
            "wrap",
            crate::shared::test_sh_options(
                "stty -echo; printf 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'",
            )
            .size(20, 10),
        )?;
        runtime.wait(Duration::from_secs(5))?;
        let before = runtime.snapshot()?;
        assert_eq!(before.cols, 20);
        assert!(
            before
                .visible_grid
                .iter()
                .filter(|line| line.contains('A'))
                .count()
                >= 3
        );
        runtime.resize(60, 10)?;
        let after = runtime.snapshot()?;
        assert_eq!(after, before);
        runtime.dispose()
    }

    #[test]
    fn exited_session_retains_final_screen_without_scrollback() -> Result<(), RuntimeError> {
        let runtime = TerminalRuntimeSession::start(
            "screen",
            crate::shared::test_sh_options("stty -echo; printf 'one\\r\\ntwo\\r\\nthree'")
                .size(20, 2),
        )?;
        runtime.wait(Duration::from_secs(5))?;
        let snapshot = runtime.snapshot()?;
        assert!(snapshot.scrollback.is_empty());
        assert!(
            snapshot
                .visible_grid
                .iter()
                .any(|line| line.contains("three"))
        );
        assert_eq!(runtime.snapshot()?, snapshot);
        runtime.dispose()
    }

    #[test]
    fn redaction_off_keeps_secret_output_verbatim() -> Result<(), RuntimeError> {
        let runtime = TerminalRuntimeSession::start_with_runtime_options(
            "printf",
            crate::shared::test_sh_options("stty -echo; printf 'API_KEY=synthetic123\\n'")
                .timeout(Duration::from_secs(5)),
            crate::shared::DEFAULT_SCROLLBACK,
            false,
        )?;
        runtime.wait(Duration::from_secs(10))?;
        assert!(runtime.full_output()?.contains("API_KEY=synthetic123"));
        runtime.dispose()
    }

    #[test]
    fn redaction_hides_a_secret_in_retained_and_observed_output() -> Result<(), RuntimeError> {
        let runtime = TerminalRuntimeSession::start_with_runtime_options(
            "printf",
            crate::shared::test_sh_options(
                "stty -echo; read go; printf 'API_KEY=synthetic123 done\\n'",
            ),
            crate::shared::DEFAULT_SCROLLBACK,
            true,
        )?;
        let (history, mut observed) = runtime.subscribe_output()?;
        assert!(history.is_empty());
        let mut writer = runtime;
        writer.write(b"go\n")?;
        writer.wait(Duration::from_secs(10))?;
        let output = writer.full_output()?;
        assert!(!output.contains("synthetic123"), "{output:?}");
        assert!(output.contains("API_KEY=<redacted> done"), "{output:?}");
        let mut seen = String::new();
        while let Ok(text) = observed.try_recv() {
            seen.push_str(&text);
        }
        assert!(!seen.contains("synthetic123"), "{seen:?}");
        assert!(seen.contains("<redacted>"), "{seen:?}");
        writer.dispose()
    }

    #[tokio::test]
    async fn redaction_publishes_an_idle_prompt_without_waiting_for_exit()
    -> Result<(), RuntimeError> {
        let runtime = TerminalRuntimeSession::start_with_runtime_options(
            "prompt",
            crate::shared::test_sh_options("stty -echo; printf 'Enter name'; read value"),
            crate::shared::DEFAULT_SCROLLBACK,
            true,
        )?;
        let (history, mut observed) = runtime.subscribe_output()?;
        let seen = tokio::time::timeout(Duration::from_secs(5), async {
            let mut seen = history;
            while !seen.contains("Enter name") {
                let Some(text) = observed.recv().await else {
                    break;
                };
                seen.push_str(&text);
            }
            seen
        })
        .await
        .expect("idle prompt published");
        assert!(seen.contains("Enter name"), "{seen:?}");
        assert!(!runtime.exited()?);
        runtime.dispose()
    }

    #[test]
    fn split_utf8_and_delta_cursor() {
        let mut state = OutputState::default();
        state.ingest(&[0xe2, 0x82]);
        assert!(state.read_delta().text.is_empty());
        state.ingest(&[0xac, b'\n']);
        assert_eq!(
            state.read_delta(),
            DeltaRead {
                text: "\u{20ac}\n".to_owned(),
                dropped_chars: 0
            }
        );
        assert!(state.read_delta().text.is_empty());
    }

    #[test]
    fn retained_output_cap_reports_dropped_units() {
        let mut state = OutputState::default();
        state.ingest(&vec![b'x'; MAX_SESSION_OUTPUT_CHARS + 10]);
        let delta = state.read_delta();
        assert_eq!(delta.dropped_chars, 10);
        assert_eq!(delta.text.len(), MAX_SESSION_OUTPUT_CHARS);
        assert!(state.read_delta().text.is_empty());
    }

    #[test]
    fn real_pty_output_exit_and_delta_are_observable() -> Result<(), RuntimeError> {
        let runtime = TerminalRuntimeSession::start(
            "printf ready",
            crate::shared::test_sh_options("stty -echo; printf 'ready\\n'")
                .timeout(Duration::from_secs(5)),
        )?;
        assert_eq!(runtime.wait(Duration::from_secs(10))?.exit_code, Some(0));
        assert!(runtime.exited()?);
        assert_eq!(runtime.full_output()?, "ready\r\n");
        assert_eq!(runtime.read_delta()?.text, "ready\r\n");
        assert!(runtime.read_delta()?.text.is_empty());
        runtime.dispose()
    }

    #[test]
    fn tail_cuts_at_line_start_unless_osc_spans_the_cut() {
        let mut state = OutputState::default();
        state.ingest(b"aaaa\nbbbb\ncccc");
        assert_eq!(state.tail(7), ("cccc".to_owned(), false));
        assert_eq!(state.tail(100), ("aaaa\nbbbb\ncccc".to_owned(), true));
        let mut open = OutputState::default();
        open.ingest(b"x\n\x1b]0;t\ncccc\x07");
        assert!(open.tail(6).1);
    }

    #[tokio::test]
    async fn atomic_subscription_covers_history_and_future_without_cursor_interference()
    -> Result<(), RuntimeError> {
        let mut runtime = TerminalRuntimeSession::start(
            "read input",
            crate::shared::test_sh_options(
                "stty -echo; printf 'ready\\n'; read value; printf '%s\\n' \"$value\"",
            )
            .timeout(Duration::from_secs(5)),
        )?;
        let (mut output, mut receiver) = runtime.subscribe_output()?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !output.contains("ready\r\n") {
                output.push_str(&receiver.recv().await.expect("output sender"));
            }
        })
        .await
        .map_err(|_| RuntimeError::WaitTimeout)?;
        runtime.write(b"next\n")?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !output.contains("next\r\n") {
                output.push_str(&receiver.recv().await.expect("output sender"));
            }
        })
        .await
        .map_err(|_| RuntimeError::WaitTimeout)?;
        runtime.wait(Duration::from_secs(5))?;
        assert_eq!(output, "ready\r\nnext\r\n");
        assert_eq!(runtime.read_delta()?.text, output);
        runtime.dispose()
    }

    #[tokio::test]
    async fn disposed_output_observer_stops_receiving_after_history_drain()
    -> Result<(), RuntimeError> {
        let mut runtime = TerminalRuntimeSession::start(
            "read input",
            crate::shared::test_sh_options(
                "stty -echo; printf 'ready\\n'; read value; printf '%s\\n' \"$value\"",
            )
            .timeout(Duration::from_secs(5)),
        )?;
        let (mut output, mut receiver, guard) = runtime.subscribe_output_guarded()?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !output.contains("ready\r\n") {
                output.push_str(&receiver.recv().await.expect("output sender"));
            }
        })
        .await
        .map_err(|_| RuntimeError::WaitTimeout)?;
        while receiver.try_recv().is_ok() {}
        assert!(guard.dispose());
        runtime.write(b"next\n")?;
        runtime.wait(Duration::from_secs(5))?;
        assert!(receiver.try_recv().is_err());
        runtime.dispose()
    }

    /// The runtime's output pipeline without a PTY (upstream `vi.mock("@earendil-works/pi-pty")`):
    /// `emit` is one `onData` chunk, `idle` is the 50 ms idle timer firing (fake timers), and
    /// `finish` is `onExit`.
    struct SyntheticRuntime {
        state: OutputState,
        redaction: Option<Redaction>,
        screen: Mutex<Option<crate::screen::TerminalScreen>>,
        listeners: OutputObservers,
        observed: tokio::sync::mpsc::UnboundedReceiver<String>,
    }
    impl SyntheticRuntime {
        fn new(redact: bool) -> Self {
            let (sender, observed) = tokio::sync::mpsc::unbounded_channel();
            Self {
                state: OutputState::default(),
                redaction: redact
                    .then(|| Arc::new((Mutex::new(RedactionState::default()), Condvar::new()))),
                screen: Mutex::new(Some(crate::screen::TerminalScreen::new(120, 40, 0))),
                listeners: Arc::new(Mutex::new(vec![sender])),
                observed,
            }
        }
        fn emit(&mut self, text: &str) {
            ingest_chunk(
                &mut self.state,
                text.as_bytes(),
                self.redaction.as_ref(),
                &self.screen,
                &self.listeners,
            );
        }
        /// The idle timer fires: publish what `flushIdle` releases (only when output is pending).
        fn idle(&mut self) {
            let Some(redaction) = &self.redaction else {
                return;
            };
            let text = {
                let mut guard = redaction.0.lock().unwrap();
                if !guard.pending || guard.done {
                    return;
                }
                guard.pending = false;
                guard.redactor.flush_idle()
            };
            deliver(&mut self.state, text, &self.screen, &self.listeners);
        }
        fn finish(&mut self) {
            if let Some(redaction) = self.redaction.clone() {
                finish_redaction(&mut self.state, &redaction, &self.screen, &self.listeners);
            }
        }
        /// No idle timer remains armed (upstream `vi.getTimerCount() === 0`).
        fn timer_idle(&self) -> bool {
            self.redaction.as_ref().is_none_or(|redaction| {
                let guard = redaction.0.lock().unwrap();
                guard.done || !guard.pending
            })
        }
        fn full_output(&self) -> String {
            lossy_utf16(self.state.buffer.iter().copied())
        }
        fn observed(&mut self) -> String {
            let mut seen = String::new();
            while let Ok(text) = self.observed.try_recv() {
                seen.push_str(&text);
            }
            seen
        }
        fn screen_text(&self) -> String {
            let mut screen = self.screen.lock().unwrap();
            screen
                .as_mut()
                .unwrap()
                .snapshot()
                .visible_grid
                .join("\n")
                .trim_end()
                .to_owned()
        }
    }

    /// senpi `bash-redaction-fixtures.ts` `secretOutputCases`: synthetic credentials assembled at runtime.
    fn secret_output_cases() -> Vec<(String, String, String)> {
        // base64url without padding of '{"alg":"HS256"}' and '{"sub":"test"}'.
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.signature123".to_owned();
        let mut cases: Vec<(String, String, String)> = [
            "gh|p_",
            "gh|o_",
            "gh|s_",
            "github|_pat_",
            "xo|x|a|-",
            "xo|x|b|-",
            "xo|x|p|-",
            "xo|x|o|-",
            "xo|x|r|-",
            "xo|x|s|-",
            "s|k-",
            "s|k-ant-",
            "s|k-proj-",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, parts)| {
            (
                format!("prefix {index}"),
                format!("{}{}", parts.replace('|', ""), "synthetic123".repeat(4)),
                "<redacted>".to_owned(),
            )
        })
        .collect();
        cases.push((
            "AWS".into(),
            format!("AK{}{}", "IA", "A1".repeat(8)),
            "<redacted>".into(),
        ));
        cases.push((
            "Google".into(),
            format!("AI{}{}", "za", "a".repeat(35)),
            "<redacted>".into(),
        ));
        cases.push(("JWT".into(), jwt, "<redacted>".into()));
        cases.push((
            "PEM".into(),
            // base64 of "synthetic key material".
            "-----BEGIN RSA PRIVATE KEY-----\nc3ludGhldGljIGtleSBtYXRlcmlhbA==\n-----END RSA PRIVATE KEY-----".into(),
            "<redacted>".into(),
        ));
        cases.push((
            "Bearer".into(),
            "Bearer synthetic-value123".into(),
            "Bearer <redacted>".into(),
        ));
        for key in [
            "API_KEY",
            "ACCESS_TOKEN",
            "CLIENT_SECRET",
            "PASSWORD",
            "DB_PASSWORD_FILE",
            "HTTP_AUTHORIZATION",
        ] {
            cases.push((
                key.into(),
                format!("{key}=synthetic-value123"),
                format!("{key}=<redacted>"),
            ));
        }
        cases.push((
            "quoted assignment".into(),
            "DB_PASSWORD=\"synthetic multi word value\"".into(),
            "DB_PASSWORD=\"<redacted>\"".into(),
        ));
        cases
    }

    // Port of test/suite/terminal-output-redaction.test.ts (senpi v2026.10.27-2).

    #[test]
    fn keeps_a_pem_opener_and_body_private_across_idle_pauses() {
        let (_, input, _) = secret_output_cases()
            .into_iter()
            .find(|(name, _, _)| name == "PEM")
            .unwrap();
        let mut runtime = SyntheticRuntime::new(true);
        for character in input.chars() {
            runtime.emit(&character.to_string());
            runtime.idle();
            assert!(["", "<redacted>"].contains(&runtime.full_output().as_str()));
        }
        runtime.finish();
        assert_eq!(runtime.full_output(), "<redacted>");
        assert!(runtime.timer_idle());
    }

    /// senpi#4358: fragments below a full public prefix remain ordinary output.
    #[test]
    fn flushes_idle_prompt_text_and_complete_controls() {
        for input in [
            "Enter name",
            "\x1b[10;5H",
            "Enter name\x1b[10;5H",
            "I",
            "e.g.",
            "git",
            "sk",
            "AI",
            "s",
            "g",
            "x",
            "A",
            "AK",
            "e",
            "C",
            "D",
            "eye",
        ] {
            let mut runtime = SyntheticRuntime::new(true);
            runtime.emit(input);
            runtime.idle();
            assert_eq!(runtime.observed(), input, "{input:?}");
            assert_eq!(runtime.state.read_delta().text, input, "{input:?}");
            runtime.emit(" next\n");
            assert_eq!(
                runtime.full_output(),
                format!("{input} next\n"),
                "{input:?}"
            );
        }
    }

    #[test]
    fn preserves_ordinary_words_continued_across_repeated_idle_pauses() {
        let mut runtime = SyntheticRuntime::new(true);
        for part in ["going to s", "l", "eep now"] {
            runtime.emit(part);
            runtime.idle();
        }
        assert_eq!(runtime.full_output(), "going to sleep now");
        runtime.finish();
        assert_eq!(runtime.full_output(), "going to sleep now");
    }

    #[test]
    fn re_evaluates_secrets_split_at_every_idle_boundary_and_suppresses_their_completion() {
        for (name, input, _) in secret_output_cases().into_iter().filter(|(name, _, _)| {
            name.starts_with("prefix") || ["AWS", "Google", "JWT"].contains(&name.as_str())
        }) {
            let minimum_length = if name.starts_with("prefix") {
                if input.starts_with("sk-") {
                    3
                } else {
                    input.find("synthetic").unwrap()
                }
            } else if name == "JWT" {
                3
            } else {
                4
            };
            for split in 1..input.len() {
                let mut runtime = SyntheticRuntime::new(true);
                runtime.emit(&input[..split]);
                runtime.idle();
                let released = if split < minimum_length {
                    &input[..split]
                } else {
                    ""
                };
                assert_eq!(runtime.full_output(), released, "{name} at {split}");
                runtime.idle();
                assert_eq!(runtime.full_output(), released, "{name} at {split}");
                runtime.emit(&format!("{} done\n", &input[split..]));
                assert_eq!(
                    runtime.full_output(),
                    format!("{released}<redacted> done\n"),
                    "{name} at {split}"
                );
                assert!(!runtime.full_output().contains(&input));
            }
        }
    }

    #[test]
    fn withholds_a_split_bearer_body_from_observers_retained_output_and_screen() {
        let mut runtime = SyntheticRuntime::new(true);
        let mut observed = String::new();
        for part in ["Bearer ", "abcdef", "1-ghijklmnop"] {
            runtime.emit(part);
            runtime.idle();
            observed.push_str(&runtime.observed());
            assert_eq!(runtime.full_output(), "Bearer ");
            assert_eq!(observed, "Bearer ");
            assert_eq!(runtime.screen_text(), "Bearer");
        }
        assert_eq!(runtime.state.read_delta().text, "Bearer ");
        runtime.finish();
        observed.push_str(&runtime.observed());
        assert_eq!(runtime.state.read_delta().text, "<redacted>");
        assert_eq!(runtime.full_output(), "Bearer <redacted>");
        assert_eq!(observed, "Bearer <redacted>");
        assert_eq!(runtime.screen_text(), "Bearer <redacted>");
    }

    #[test]
    fn protects_observers_delta_retained_output_and_screen() {
        for enabled in [false, true] {
            let mut runtime = SyntheticRuntime::new(enabled);
            let fixtures = secret_output_cases();
            let input = fixtures
                .iter()
                .map(|(_, input, _)| input.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let expected = if enabled {
                fixtures
                    .iter()
                    .map(|(_, _, expected)| expected.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                input.clone()
            };
            let mut observed = String::new();
            for character in input.chars() {
                runtime.emit(&character.to_string());
                observed.push_str(&runtime.observed());
                if enabled {
                    assert!(expected.starts_with(&observed), "{observed:?}");
                }
            }
            runtime.finish();
            observed.push_str(&runtime.observed());
            assert_eq!(runtime.full_output(), expected);
            assert_eq!(runtime.state.read_delta().text, expected);
            assert_eq!(runtime.state.read_delta().text, "");
            assert_eq!(observed, expected);
        }
    }
}
