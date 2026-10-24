use crate::session_input::PtyInput;
use crate::session_threads::{spawn_reader, spawn_timeout};
use crate::signals::{kill_signal, process_group, send_signal, signal_signal};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub type PtyResult<T> = Result<T, PtyError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyError {
    message: String,
}

impl PtyError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for PtyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for PtyError {}

impl From<std::io::Error> for PtyError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct PtySessionOptions {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    pub timeout: Option<Duration>,
}

impl PtySessionOptions {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            cols: 80,
            rows: 24,
            timeout: None,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyExit {
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub timed_out: bool,
}

pub struct PtySession {
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    input: Arc<PtyInput>,
    child: Option<Box<dyn Child + Send + Sync>>,
    /// Recorded at spawn: `child` is moved into the waiter by `wait_in_background`.
    pid: Option<u32>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    reader_thread: Arc<Mutex<Option<JoinHandle<()>>>>,
    finished: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    timed_out: Arc<AtomicBool>,
    user_wrote: Arc<AtomicBool>,
    process_group: Option<i32>,
    exit: Option<PtyExit>,
}

impl PtySession {
    pub fn start(
        opts: PtySessionOptions,
        mut on_chunk: impl FnMut(&[u8]) + Send + 'static,
    ) -> PtyResult<Self> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: opts.rows,
                cols: opts.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| PtyError::new(error.to_string()))?;

        let mut command = CommandBuilder::new(&opts.command);
        command.args(&opts.args);
        if let Some(cwd) = &opts.cwd {
            command.cwd(cwd);
        }
        for (key, value) in &opts.env {
            command.env(key, value);
        }

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| PtyError::new(error.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| PtyError::new(error.to_string()))?;
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| PtyError::new(error.to_string()))?;
        let pid = child.process_id();
        let killer = child.clone_killer();
        let timeout_killer = child.clone_killer();
        #[cfg(unix)]
        let process_group = process_group(&*pair.master)
            .filter(|process_group| *process_group != unsafe { libc::getpgrp() })
            .or_else(|| child.process_id().map(|pid| pid as i32));
        #[cfg(not(unix))]
        let process_group = process_group(&*pair.master).or_else(|| child.process_id().map(|pid| pid as i32));
        let finished = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let timed_out = Arc::new(AtomicBool::new(false));
        let input = Arc::new(PtyInput::new(writer));
        let user_wrote = Arc::new(AtomicBool::new(false));

        // On Windows, portable-pty's ConPTY is created with PSEUDOCONSOLE_INHERIT_CURSOR, so it
        // emits a DSR cursor-position query (ESC[6n) at startup and withholds the child's output
        // until a terminal answers it. A raw byte pipe has no terminal, so the reader answers the
        // startup handshake itself (see answer_conpty_cursor_query); the window closes on the first
        // consumer write so later DSR queries pass through to the consumer.
        #[cfg(windows)]
        let reader_input = Arc::clone(&input);
        #[cfg(windows)]
        let reader_user_wrote = Arc::clone(&user_wrote);
        #[cfg(windows)]
        let mut dsr_tail: Vec<u8> = Vec::new();
        #[cfg(windows)]
        let mut dsr_answered_initial = false;
        #[cfg(windows)]
        let mut conpty_filter = ConPtyFilter::new();
        let reader_thread = spawn_reader(reader, move |chunk| {
            #[cfg(windows)]
            answer_conpty_cursor_query(
                chunk,
                &mut dsr_tail,
                &mut dsr_answered_initial,
                &reader_input,
                &reader_user_wrote,
            );
            #[cfg(windows)]
            {
                let filtered = if chunk.is_empty() {
                    conpty_filter.flush()
                } else {
                    conpty_filter.process(chunk)
                };
                if !filtered.is_empty() {
                    on_chunk(&filtered);
                }
            }
            #[cfg(not(windows))]
            if !chunk.is_empty() {
                on_chunk(chunk);
            }
        });

        if let Some(timeout) = opts.timeout {
            spawn_timeout(
                timeout,
                Arc::clone(&finished),
                Arc::clone(&cancelled),
                Arc::clone(&timed_out),
                process_group,
                timeout_killer,
            );
        }

        Ok(Self {
            master: Arc::new(Mutex::new(Some(pair.master))),
            input,
            child: Some(child),
            pid,
            killer,
            reader_thread: Arc::new(Mutex::new(Some(reader_thread))),
            finished,
            cancelled,
            timed_out,
            user_wrote,
            process_group,
            exit: None,
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn write(&mut self, bytes: &[u8]) -> PtyResult<()> {
        if self.finished.load(Ordering::SeqCst) {
            return Err(PtyError::new("pty session is closed"));
        }
        // The first consumer write closes the ConPTY cursor-query auto-answer window.
        self.user_wrote.store(true, Ordering::SeqCst);
        self.input.send(bytes)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> PtyResult<()> {
        self.master
            .lock()
            .map_err(|_| PtyError::new("pty master lock poisoned"))?
            .as_ref()
            .ok_or_else(|| PtyError::new("pty session is closed"))?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| PtyError::new(error.to_string()))
    }

    pub fn signal(&mut self, signal: &str) -> PtyResult<()> {
        self.signal_number(signal_signal(signal)?)
    }

    pub fn kill(&mut self) -> PtyResult<()> {
        self.cancelled.store(true, Ordering::SeqCst);
        match self.signal_number(kill_signal()) {
            Ok(()) => Ok(()),
            Err(error) if error.to_string().contains("No such process") => Ok(()),
            #[cfg(windows)]
            Err(error)
                if error.to_string().contains("os error 6")
                    || error.to_string().contains("os error 5")
                    || error.to_string().contains("os error 2")
                    || error.to_string().contains("Access is denied") =>
            {
                Ok(())
            }
            Err(_) => match self.killer.kill().map_err(PtyError::from) {
                Ok(()) => Ok(()),
                Err(error) if error.to_string().contains("No such process") => Ok(()),
                #[cfg(windows)]
                Err(error)
                    if error.to_string().contains("os error 6")
                        || error.to_string().contains("os error 5")
                        || error.to_string().contains("os error 2")
                        || error.to_string().contains("Access is denied") =>
                {
                    Ok(())
                }
                Err(error) => Err(error),
            },
        }
    }

    pub fn wait(&mut self) -> PtyResult<PtyExit> {
        if let Some(exit) = &self.exit {
            return Ok(exit.clone());
        }
        let mut child = self
            .child
            .take()
            .ok_or_else(|| PtyError::new("pty session is closed"))?;
        let status = child.wait()?;
        self.finished.store(true, Ordering::SeqCst);
        drain_output(&self.input, &self.master, &self.reader_thread);
        let exit = PtyExit {
            exit_code: Some(status.exit_code() as i32),
            cancelled: self.cancelled.load(Ordering::SeqCst) || self.timed_out.load(Ordering::SeqCst),
            timed_out: self.timed_out.load(Ordering::SeqCst),
        };
        self.exit = Some(exit.clone());
        Ok(exit)
    }

    pub fn wait_in_background(&mut self) -> PtyResult<JoinHandle<PtyResult<PtyExit>>> {
        if let Some(exit) = &self.exit {
            let exit = exit.clone();
            return Ok(std::thread::spawn(move || Ok(exit)));
        }
        let mut child = self
            .child
            .take()
            .ok_or_else(|| PtyError::new("pty session is closed"))?;
        let finished = Arc::clone(&self.finished);
        let cancelled = Arc::clone(&self.cancelled);
        let timed_out = Arc::clone(&self.timed_out);
        let input = Arc::clone(&self.input);
        let master = Arc::clone(&self.master);
        let reader_thread = Arc::clone(&self.reader_thread);

        Ok(std::thread::spawn(move || {
            let status = child.wait()?;
            finished.store(true, Ordering::SeqCst);
            drain_output(&input, &master, &reader_thread);
            Ok(PtyExit {
                exit_code: Some(status.exit_code() as i32),
                cancelled: cancelled.load(Ordering::SeqCst) || timed_out.load(Ordering::SeqCst),
                timed_out: timed_out.load(Ordering::SeqCst),
            })
        }))
    }

    fn signal_number(&mut self, signal: i32) -> PtyResult<()> {
        if self.finished.load(Ordering::SeqCst) {
            return Err(PtyError::new("pty session is closed"));
        }
        send_signal(self.process_group, signal, &mut *self.killer)
    }
}

fn drain_output(
    input: &PtyInput,
    master: &Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    reader_thread: &Arc<Mutex<Option<JoinHandle<()>>>>,
) {
    // POSIX reaches EOF/EIO when the child closes the slave. Drain before dropping
    // portable-pty's writer: its Drop writes newline/EOT, which must not be echoed
    // into the raw output stream after the child has exited.
    #[cfg(unix)]
    {
        let reader_thread = reader_thread.lock().ok().and_then(|mut guard| guard.take());
        if let Some(reader_thread) = reader_thread {
            let _ = reader_thread.join();
        }
    }
    input.close();
    // Drop the PTY master before joining the reader thread. On Windows the ConPTY output pipe
    // never reaches EOF just because the child exited; only closing the pseudo console unblocks the
    // pending read. On POSIX closing the master here is harmless early cleanup.
    if let Ok(mut guard) = master.lock() {
        guard.take();
    }
    let reader_thread = reader_thread.lock().ok().and_then(|mut guard| guard.take());
    if let Some(reader_thread) = reader_thread {
        let _ = reader_thread.join();
    }
}

/// Answer ConPTY's startup DSR cursor-position query so the child's withheld output is released.
///
/// portable-pty 0.9's ConPTY is created with PSEUDOCONSOLE_INHERIT_CURSOR, which makes it emit
/// `ESC[6n` at startup and hold back the child's output until a terminal replies with a
/// cursor-position report. senpi-pty is a raw byte pipe with no terminal attached, so it answers the
/// startup handshake on the consumer's behalf. The reply window is bounded: the very first query is
/// always answered (so a consumer that writes immediately cannot race past the handshake), and
/// further queries are answered only until the first consumer write — afterwards DSR queries pass
/// through untouched so a consumer that is itself a terminal owns the exchange.
#[cfg(windows)]
fn answer_conpty_cursor_query(
    chunk: &[u8],
    tail: &mut Vec<u8>,
    answered_initial: &mut bool,
    input: &PtyInput,
    user_wrote: &Arc<AtomicBool>,
) {
    const DSR_CURSOR_QUERY: &[u8] = b"\x1b[6n";
    const CPR_RESPONSE: &[u8] = b"\x1b[1;1R";

    // Prepend the few bytes carried from the previous read so a query split across reads is still
    // detected, then keep the trailing bytes that could start the next boundary-straddling match.
    let mut window = std::mem::take(tail);
    window.extend_from_slice(chunk);
    let seen = window
        .windows(DSR_CURSOR_QUERY.len())
        .any(|candidate| candidate == DSR_CURSOR_QUERY);
    let keep = DSR_CURSOR_QUERY.len() - 1;
    *tail = if window.len() > keep {
        window[window.len() - keep..].to_vec()
    } else {
        window
    };

    if !seen || (*answered_initial && user_wrote.load(Ordering::SeqCst)) {
        return;
    }
    *answered_initial = true;
    let _ = input.send(CPR_RESPONSE);
}

#[cfg(windows)]
struct ConPtyFilter {
    in_preamble: bool,
    buffer: Vec<u8>,
}

#[cfg(windows)]
impl ConPtyFilter {
    fn new() -> Self {
        Self {
            in_preamble: true,
            buffer: Vec::new(),
        }
    }

    fn process(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.buffer.len() {
            if self.buffer[i] == 0x1b {
                let rem = &self.buffer[i..];
                if let Some(consumed) = self.match_escape(rem) {
                    if consumed == 0 {
                        break;
                    }
                    i += consumed;
                    continue;
                }
                if is_partial_conpty_escape(rem, self.in_preamble) {
                    break;
                }
                self.in_preamble = false;
                out.push(self.buffer[i]);
                i += 1;
            } else {
                self.in_preamble = false;
                out.push(self.buffer[i]);
                i += 1;
            }
        }
        self.buffer.drain(..i);
        out
    }

    fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buffer)
    }

    fn match_escape(&self, rem: &[u8]) -> Option<usize> {
        if rem.starts_with(b"\x1b[6n") {
            return Some(4);
        }
        if rem.starts_with(b"\x1b[?9001h") || rem.starts_with(b"\x1b[?9001l") {
            return Some(8);
        }
        if rem.starts_with(b"\x1b[?1004h") || rem.starts_with(b"\x1b[?1004l") {
            return Some(8);
        }
        if self.in_preamble {
            if rem.starts_with(b"\x1b[m") {
                return Some(3);
            }
            if rem.starts_with(b"\x1b[?25h") {
                return Some(6);
            }
            if rem.starts_with(b"\x1b]0;") {
                let mut idx = 4;
                while idx < rem.len() {
                    if rem[idx] == 0x07 {
                        return Some(idx + 1);
                    }
                    if rem[idx] == 0x1b && idx + 1 < rem.len() && rem[idx + 1] == b'\\' {
                        return Some(idx + 2);
                    }
                    idx += 1;
                }
                return Some(0);
            }
        }
        None
    }
}

#[cfg(windows)]
fn is_partial_conpty_escape(rem: &[u8], in_preamble: bool) -> bool {
    const PREFIXES: &[&[u8]] = &[
        b"\x1b[6n",
        b"\x1b[?9001h",
        b"\x1b[?9001l",
        b"\x1b[?1004h",
        b"\x1b[?1004l",
    ];
    for p in PREFIXES {
        if rem.len() < p.len() && p.starts_with(rem) {
            return true;
        }
    }
    if in_preamble {
        const PREAMBLE_PREFIXES: &[&[u8]] = &[b"\x1b[m", b"\x1b[?25h", b"\x1b]0;"];
        for p in PREAMBLE_PREFIXES {
            if rem.len() < p.len() && p.starts_with(rem) {
                return true;
            }
        }
    }
    false
}
