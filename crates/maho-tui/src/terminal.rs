//! Port of senpi \`packages/tui/src/terminal.ts\`.
//!
//! Node's event loop is replaced by an explicit driver: input arrives through
//! [\`ProcessTerminal::feed\`] (or [\`ProcessTerminal::pump\`], which reads stdin), and every
//! senpi \`setTimeout\`/\`setInterval\` is a deadline on the injected [\`Clock\`] that
//! [\`ProcessTerminal::run_due_timers\`] fires in due order. Raw mode and io go through
//! [\`TerminalIo\`] (crossterm raw mode + rustix polling for the process terminal).

use std::cell::RefCell;
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use regex::Regex;

use crate::keys::set_kitty_protocol_active;
use crate::mux::is_multiplexer_session_in;
use crate::native_modifiers::{ModifierKey, is_native_modifier_pressed};
use crate::native_platform::{get_native_platform_helper, node_platform};
use crate::process_env::{self, Env};
use crate::process_stdio::{self, StreamWrite};
use crate::stderr_observer::observe_process_stderr_writes;
use crate::stdin_buffer::{
    Clock, StdinBuffer, StdinBufferOptions, StdinEvent, StdinInput, SystemClock,
};
use crate::tmux_cursor_query::{
    TmuxCursorQuery, TmuxExecFile, finish_tmux_cursor_query, query_tmux_cursor_position,
};

const TERMINAL_PROGRESS_KEEPALIVE_MS: u64 = 1000;
const TERMINAL_PROGRESS_ACTIVE_SEQUENCE: &str = "\x1b]9;4;3\x07";
const TERMINAL_PROGRESS_CLEAR_SEQUENCE: &str = "\x1b]9;4;0\x07";
const NATIVE_SHIFT_ENTER_SEQUENCE: &str = "\x1b[13;2u";
const DESIRED_KITTY_KEYBOARD_PROTOCOL_FLAGS: u32 = 7;
const DEAD_TERMINAL_ERROR_CODES: [&str; 3] = ["EIO", "EPIPE", "ENOTCONN"];
/// Bun's macOS tty shim can report synchronous ioctl EIO as raw positive errno 5.
const EIO_ERRNO: i32 = 5;
/// Stable across darwin and linux; ENOTCONN is excluded because its number differs.
const EPIPE_ERRNO: i32 = 32;
const KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS: u64 = 150;
const CURSOR_QUERY_TIMEOUT_MS: u64 = 750;
const STDIN_ERROR_HANDLER_GRACE_MS: u64 = 250;
const DEFAULT_ESCAPE_TIMEOUT_MS: f64 = 10.0;
const DEFAULT_SSH_ESCAPE_TIMEOUT_MS: f64 = 100.0;

static KITTY_KEYBOARD_PROTOCOL_QUERY: LazyLock<String> =
    LazyLock::new(|| format!("\x1b[>{DESIRED_KITTY_KEYBOARD_PROTOCOL_FLAGS}u\x1b[?u\x1b[c"));
static ERRNO_IN_MESSAGE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"errno:\s*(\d+)").expect("valid regex"));
static CURSOR_POSITION_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[\?(\d+);(\d+)(?:;(\d+))?R$").expect("valid regex"));
static KITTY_FLAGS_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[\?(\d+)u$").expect("valid regex"));
static DEVICE_ATTRIBUTES_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[\?[\d;]*c$").expect("valid regex"));
static PRIVATE_PREFIX_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[\?[\d;]*$").expect("valid regex"));
static FINAL_BYTE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\x40-\x7e]").expect("valid regex"));
static WSL_INTEROP_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/run/WSL/\d+_interop$").expect("valid regex"));
static TITLE_CONTROL_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\x{0000}-\x{001f}\x{007f}-\x{009f}]").expect("valid regex"));
static JS_DECIMAL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?$").expect("valid regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPosition {
    pub row: u64,
    pub column: u64,
    pub page: Option<u64>,
}

pub fn parse_cursor_position_response(sequence: &str) -> Option<CursorPosition> {
    let captures = CURSOR_POSITION_PATTERN.captures(sequence)?;
    let number = |i: usize| captures.get(i).and_then(|m| m.as_str().parse::<u64>().ok());
    Some(CursorPosition {
        row: number(1)?,
        column: number(2)?,
        page: match captures.get(3) {
            Some(_) => Some(number(3)?),
            None => None,
        },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardProtocolNegotiationSequence {
    KittyFlags { flags: u64 },
    DeviceAttributes,
    CursorPosition(CursorPosition),
}

pub fn parse_keyboard_protocol_negotiation_sequence(
    sequence: &str,
) -> Option<KeyboardProtocolNegotiationSequence> {
    if let Some(position) = parse_cursor_position_response(sequence) {
        return Some(KeyboardProtocolNegotiationSequence::CursorPosition(
            position,
        ));
    }
    if let Some(captures) = KITTY_FLAGS_PATTERN.captures(sequence) {
        // \`Number.parseInt\` of an all-digit run; saturate instead of losing precision.
        let flags = captures[1].parse::<u64>().unwrap_or(u64::MAX);
        return Some(KeyboardProtocolNegotiationSequence::KittyFlags { flags });
    }
    DEVICE_ATTRIBUTES_PATTERN
        .is_match(sequence)
        .then_some(KeyboardProtocolNegotiationSequence::DeviceAttributes)
}

fn is_keyboard_protocol_negotiation_sequence_prefix(sequence: &str) -> bool {
    sequence == "\x1b[" || PRIVATE_PREFIX_PATTERN.is_match(sequence)
}

pub fn is_apple_terminal_session() -> bool {
    node_platform() == "darwin"
        && process_env::var("TERM_PROGRAM").as_deref() == Some("Apple_Terminal")
}

/// Refresh terminal dimensions on POSIX by sending SIGWINCH to this process. Best-effort:
/// restricted seccomp/LSM policies may reject \`kill(2)\` (EACCES); the refresh is then skipped.
pub fn refresh_terminal_dimensions() {
    refresh_terminal_dimensions_with(node_platform(), std::process::id(), send_sigwinch);
}

fn refresh_terminal_dimensions_with(
    platform: &str,
    pid: u32,
    kill: impl FnOnce(u32) -> std::io::Result<()>,
) {
    if platform == "win32" || pid == 0 {
        return;
    }
    // Signal delivery not permitted in this environment is ignored.
    let _ = kill(pid);
}

#[cfg(unix)]
fn send_sigwinch(pid: u32) -> std::io::Result<()> {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    rustix::process::kill_process(pid, rustix::process::Signal::WINCH).map_err(std::io::Error::from)
}

#[cfg(not(unix))]
fn send_sigwinch(_pid: u32) -> std::io::Result<()> {
    Ok(())
}

pub fn normalize_native_shift_enter_input(
    data: &str,
    should_detect_native_shift_enter: bool,
    is_shift_pressed: bool,
) -> String {
    if should_detect_native_shift_enter && data == "\r" && is_shift_pressed {
        return NATIVE_SHIFT_ENTER_SEQUENCE.to_string();
    }
    data.to_string()
}

pub fn normalize_apple_terminal_input(
    data: &str,
    is_apple_terminal: bool,
    is_shift_pressed: bool,
) -> String {
    normalize_native_shift_enter_input(data, is_apple_terminal, is_shift_pressed)
}

fn js_trimmed<'a>(env: &'a Env, name: &str) -> Option<&'a str> {
    env.get(name).map(|v| v.trim())
}

fn default_socket_exists(socket_path: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        std::fs::metadata(socket_path).is_ok_and(|m| m.file_type().is_socket())
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        false
    }
}

/// \`normalizeWarpWslShiftEnterInput(data, env, platform, socketExists)\`.
pub fn normalize_warp_wsl_shift_enter_input(
    data: &str,
    env: &Env,
    platform: &str,
    socket_exists: Option<&dyn Fn(&str) -> bool>,
) -> String {
    if data != "\n" || platform != "linux" {
        return data.to_string();
    }
    let non_empty = |name: &str| js_trimmed(env, name).is_some_and(|v| !v.is_empty());
    if is_multiplexer_session_in(env)
        || non_empty("SSH_CONNECTION")
        || non_empty("SSH_CLIENT")
        || non_empty("SSH_TTY")
    {
        return data.to_string();
    }
    let is_warp = non_empty("WARP_SESSION_ID") || non_empty("WARP_TERMINAL_SESSION_UUID");
    let is_wsl = is_warp
        && js_trimmed(env, "WSL_INTEROP").is_some_and(|interop| {
            WSL_INTEROP_PATTERN.is_match(interop)
                && match socket_exists {
                    Some(check) => check(interop),
                    None => default_socket_exists(interop),
                }
        });
    if is_warp && is_wsl {
        NATIVE_SHIFT_ENTER_SEQUENCE.to_string()
    } else {
        data.to_string()
    }
}

pub fn keyboard_enhancement_enabled() -> bool {
    match process_env::var("PI_TUI_KEYBOARD_PROTOCOL") {
        None => true,
        Some(value) => !["0", "false", "no", "off"].contains(&value.to_lowercase().as_str()),
    }
}

/// JS \`Number(value)\` for environment strings.
fn js_number(value: &str) -> f64 {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return 0.0;
    }
    let radix = |prefix: &str, radix: u32| {
        trimmed
            .strip_prefix(prefix)
            .or_else(|| trimmed.strip_prefix(&prefix.to_uppercase()))
            .map(|digits| {
                if digits.is_empty() {
                    return f64::NAN;
                }
                digits
                    .chars()
                    .try_fold(0f64, |acc, c| {
                        c.to_digit(radix)
                            .map(|d| acc * f64::from(radix) + f64::from(d))
                    })
                    .unwrap_or(f64::NAN)
            })
    };
    if let Some(n) = radix("0x", 16)
        .or_else(|| radix("0o", 8))
        .or_else(|| radix("0b", 2))
    {
        return n;
    }
    match trimmed {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if JS_DECIMAL_PATTERN.is_match(trimmed) {
        trimmed.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}

/// How long to wait for the rest of an escape sequence before dispatching a lone ESC.
/// Legacy Alt+key input is ESC plus another byte, so high-latency transports wait longer.
pub fn resolve_escape_timeout_ms(env: &Env) -> f64 {
    let configured = env
        .get("PI_TUI_ESC_TIMEOUT")
        .map_or(f64::NAN, |v| js_number(v));
    if configured.is_finite() && configured > 0.0 {
        return configured;
    }
    if process_env::truthy(env, "SSH_CONNECTION") || process_env::truthy(env, "SSH_TTY") {
        return DEFAULT_SSH_ESCAPE_TIMEOUT_MS;
    }
    DEFAULT_ESCAPE_TIMEOUT_MS
}

/// Node-style error carried by terminal io failures (\`code\`, \`errno\`, \`message\`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalError {
    pub code: Option<String>,
    pub errno: Option<i32>,
    pub message: String,
}

impl TerminalError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: None,
            errno: None,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn with_code(mut self, code: &str) -> Self {
        self.code = Some(code.to_string());
        self
    }

    #[must_use]
    pub fn with_errno(mut self, errno: i32) -> Self {
        self.errno = Some(errno);
        self
    }
}

impl std::fmt::Display for TerminalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TerminalError {}

impl From<std::io::Error> for TerminalError {
    fn from(error: std::io::Error) -> Self {
        let message = error.to_string();
        let mut out = Self::new(message);
        #[cfg(unix)]
        if let Some(errno) = rustix::io::Errno::from_io_error(&error) {
            use rustix::io::Errno;
            let code = match errno {
                Errno::IO => Some("EIO"),
                Errno::PIPE => Some("EPIPE"),
                Errno::NOTCONN => Some("ENOTCONN"),
                Errno::BADF => Some("EBADF"),
                Errno::INVAL => Some("EINVAL"),
                _ => None,
            };
            out.code = code.map(str::to_string);
            out.errno = Some(-errno.raw_os_error());
        }
        out
    }
}

fn is_dead_terminal_error(error: &TerminalError) -> bool {
    if let Some(code) = &error.code {
        return DEAD_TERMINAL_ERROR_CODES.contains(&code.as_str());
    }
    if error.errno == Some(EIO_ERRNO) {
        return true;
    }
    ERRNO_IN_MESSAGE_PATTERN
        .captures(&error.message)
        .and_then(|c| c[1].parse::<i32>().ok())
        .is_some_and(|errno| errno == EIO_ERRNO || errno == EPIPE_ERRNO)
}

/// A vanished or re-backgrounded controlling terminal fails the next stdin read with EIO;
/// that is the only stdin error this module owns (Node code "EIO"/errno -5, Bun errno 5).
fn is_terminal_detach_stdin_error(error: &TerminalError) -> bool {
    error.code.as_deref() == Some("EIO")
        || error.errno == Some(EIO_ERRNO)
        || error.errno == Some(-EIO_ERRNO)
}

thread_local! {
    static STDIN_ERROR_SUBSCRIBERS: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
    static NEXT_SUBSCRIBER_ID: RefCell<u64> = const { RefCell::new(0) };
}

pub fn stdin_error_subscriber_count_for_tests() -> usize {
    STDIN_ERROR_SUBSCRIBERS.with(|s| s.borrow().len())
}

pub fn stdin_error_dispatcher_installed_for_tests() -> bool {
    stdin_error_subscriber_count_for_tests() > 0
}

/// The stdin "error" dispatch: detach errors are swallowed while a terminal subscribes; every
/// other error (and any error with no subscriber) propagates like an unlistened EventEmitter error.
pub fn dispatch_stdin_error(error: TerminalError) -> Result<(), TerminalError> {
    let subscribed = stdin_error_dispatcher_installed_for_tests();
    if !subscribed || !is_terminal_detach_stdin_error(&error) {
        return Err(error);
    }
    Ok(())
}

fn subscribe_to_stdin_errors() -> u64 {
    let id = NEXT_SUBSCRIBER_ID.with(|n| {
        let mut n = n.borrow_mut();
        *n += 1;
        *n
    });
    STDIN_ERROR_SUBSCRIBERS.with(|s| s.borrow_mut().insert(id));
    id
}

fn unsubscribe_from_stdin_errors(id: u64) {
    STDIN_ERROR_SUBSCRIBERS.with(|s| s.borrow_mut().remove(&id));
}

/// Raw terminal io behind [\`ProcessTerminal\`].
pub trait TerminalIo {
    fn write(&mut self, data: &str);
    fn is_raw(&self) -> bool;
    fn set_raw_mode(&mut self, raw: bool) -> Result<(), TerminalError>;
    fn resume(&mut self) {}
    fn pause(&mut self) {}
    /// \`(columns, rows)\` when the output is a tty.
    fn size(&self) -> Option<(u16, u16)>;
    /// Waits up to \`timeout_ms\` for stdin bytes; \`Ok(None)\` on timeout.
    fn read_input(&mut self, timeout_ms: u64) -> Result<Option<Vec<u8>>, TerminalError>;
    fn refresh_dimensions(&mut self) {
        refresh_terminal_dimensions();
    }
}

/// The process's stdin/stdout: crossterm raw mode, rustix poll/read, stdout via [\`process_stdio\`].
#[derive(Debug, Default)]
pub struct ProcessIo;

impl TerminalIo for ProcessIo {
    fn write(&mut self, data: &str) {
        process_stdio::stdout_write(data);
    }

    fn is_raw(&self) -> bool {
        crossterm::terminal::is_raw_mode_enabled().unwrap_or(false)
    }

    fn set_raw_mode(&mut self, raw: bool) -> Result<(), TerminalError> {
        let result = if raw {
            crossterm::terminal::enable_raw_mode()
        } else {
            crossterm::terminal::disable_raw_mode()
        };
        result.map_err(TerminalError::from)
    }

    fn size(&self) -> Option<(u16, u16)> {
        #[cfg(unix)]
        {
            let size = rustix::termios::tcgetwinsize(rustix::stdio::stdout()).ok()?;
            Some((size.ws_col, size.ws_row))
        }
        #[cfg(not(unix))]
        {
            crossterm::terminal::size().ok()
        }
    }

    fn read_input(&mut self, timeout_ms: u64) -> Result<Option<Vec<u8>>, TerminalError> {
        #[cfg(unix)]
        {
            use rustix::event::{PollFd, PollFlags, Timespec, poll};
            let stdin = rustix::stdio::stdin();
            let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
            let timeout = Timespec {
                tv_sec: i64::try_from(timeout_ms / 1000).unwrap_or(i64::MAX),
                tv_nsec: i64::try_from((timeout_ms % 1000) * 1_000_000).unwrap_or(0),
            };
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) => return Ok(None),
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => return Ok(None),
                Err(e) => return Err(std::io::Error::from(e).into()),
            }
            let mut buf = vec![0u8; 4096];
            match rustix::io::read(stdin, &mut buf) {
                Ok(0) => Ok(None),
                Ok(n) => {
                    buf.truncate(n);
                    Ok(Some(buf))
                }
                Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => Ok(None),
                Err(e) => Err(std::io::Error::from(e).into()),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = timeout_ms;
            Ok(None)
        }
    }
}

/// Resolution of a [\`ProcessTerminal::query_cursor_position\`] call.
#[derive(Clone, Default)]
pub struct CursorQueryTicket(Rc<RefCell<Option<Option<CursorPosition>>>>);

impl CursorQueryTicket {
    /// \`None\` while pending; \`Some(result)\` once settled.
    pub fn result(&self) -> Option<Option<CursorPosition>> {
        *self.0.borrow()
    }

    pub fn is_same(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    fn resolved(position: Option<CursorPosition>) -> Self {
        Self(Rc::new(RefCell::new(Some(position))))
    }

    fn settle(&self, position: Option<CursorPosition>) {
        *self.0.borrow_mut() = Some(position);
    }
}

pub type InputHandler = Box<dyn FnMut(&str)>;
pub type ResizeHandler = Box<dyn FnMut()>;
pub type ExternalWriteListener = Arc<dyn Fn() + Send + Sync>;
/// Returns \`Err\` to reproduce a throwing handler (the write then falls back to raw stdout).
pub type ExternalStdoutHandler = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
pub type ObserveExternalStderrWrites =
    Arc<dyn Fn(ExternalWriteListener) -> Box<dyn FnOnce() + Send>>;

/// Minimal terminal interface for the TUI.
pub trait Terminal {
    fn start(&mut self, on_input: InputHandler, on_resize: ResizeHandler);
    fn stop(&mut self) -> Result<(), TerminalError>;
    /// Drain stdin before exiting so Kitty key release events do not leak to the parent shell.
    fn drain_input(&mut self, max_ms: u64, idle_ms: u64);
    /// Optional for virtual/custom terminals that cannot answer private DECXCPR.
    fn query_cursor_position(&mut self) -> Option<CursorQueryTicket> {
        None
    }
    /// Observe external writes without changing their output policy; returns the unsubscribe.
    fn observe_external_writes(
        &mut self,
        _listener: ExternalWriteListener,
    ) -> Option<Box<dyn FnOnce()>> {
        None
    }
    fn write(&mut self, data: &str);
    fn columns(&self) -> u16;
    fn rows(&self) -> u16;
    fn kitty_protocol_active(&self) -> bool;
    fn move_by(&mut self, lines: i64);
    fn hide_cursor(&mut self);
    fn show_cursor(&mut self);
    fn clear_line(&mut self);
    fn clear_from_cursor(&mut self);
    fn clear_screen(&mut self);
    fn set_title(&mut self, title: &str);
    fn set_progress(&mut self, active: bool);
    /// Earliest pending timer deadline on the terminal clock.
    fn next_timer_deadline(&self) -> Option<u64> {
        None
    }
    /// Fires every timer whose deadline has passed.
    fn run_due_timers(&mut self) {}
}

#[derive(Default)]
pub struct ProcessTerminalOptions {
    /// Injectable out-of-band tmux cursor source.
    pub tmux_exec_file: Option<TmuxExecFile>,
    /// When set, stdout writes not issued by this terminal are hidden while started and
    /// forwarded here instead (they would desynchronize differential rendering).
    pub on_external_stdout_write: Option<ExternalStdoutHandler>,
    /// Observe actual stderr delivery when a host redirects diagnostics.
    pub observe_external_stderr_writes: Option<ObserveExternalStderrWrites>,
    pub io: Option<Box<dyn TerminalIo>>,
    pub clock: Option<Arc<dyn Clock>>,
}

struct PendingCursorQuery {
    ticket: CursorQueryTicket,
    timer_due: u64,
    issued: bool,
    tmux_pane: Option<String>,
    deadline: u64,
    tmux_second_read: Option<(u64, CursorPosition)>,
}

type Observers = Arc<Mutex<Vec<(u64, ExternalWriteListener)>>>;

fn note_external_write(observers: &Observers) {
    let listeners: Vec<ExternalWriteListener> = observers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|(_, l)| Arc::clone(l))
        .collect();
    for listener in listeners {
        listener();
    }
}

/// Real terminal on the process stdin/stdout.
pub struct ProcessTerminal {
    io: Box<dyn TerminalIo>,
    clock: Arc<dyn Clock>,
    was_raw: bool,
    tmux_exec_file: Option<TmuxExecFile>,
    on_external_stdout_write: Option<ExternalStdoutHandler>,
    original_stdout_write: Option<StreamWrite>,
    guard_write: Option<StreamWrite>,
    raw_stdout_write: Option<StreamWrite>,
    observe_external_stderr_writes: ObserveExternalStderrWrites,
    stop_external_stderr_observation: Option<Box<dyn FnOnce() + Send>>,
    external_write_observers: Observers,
    next_observer_id: u64,
    keyboard_negotiation_settled: bool,
    cursor_query_timed_out: bool,
    cursor_query: Option<PendingCursorQuery>,
    forwarding_external_write: Arc<AtomicBool>,
    input_handler: Option<InputHandler>,
    resize_handler: Option<ResizeHandler>,
    kitty_protocol_active: bool,
    modify_other_keys_active: bool,
    keyboard_protocol_pushed: bool,
    keyboard_protocol_negotiation_buffer: String,
    discarding_private_response: bool,
    keyboard_protocol_buffer_flush_due: Option<u64>,
    stdin_buffer: Option<StdinBuffer>,
    stdin_data_listening: bool,
    stdin_error_subscription: Option<u64>,
    stdin_error_cleanup_due: Option<u64>,
    progress_due: Option<u64>,
    last_size: Option<(u16, u16)>,
    write_log_path: String,
}

fn resolve_write_log_path() -> String {
    let env = process_env::var("PI_TUI_WRITE_LOG").unwrap_or_default();
    if env.is_empty() {
        return String::new();
    }
    if Path::new(&env).is_dir() {
        let ts = jiff::Zoned::now().strftime("%Y-%m-%d_%H-%M-%S").to_string();
        return Path::new(&env)
            .join(format!("tui-{ts}-{}.log", std::process::id()))
            .to_string_lossy()
            .into_owned();
    }
    env
}

impl Default for ProcessTerminal {
    fn default() -> Self {
        Self::new(ProcessTerminalOptions::default())
    }
}

impl ProcessTerminal {
    pub fn new(options: ProcessTerminalOptions) -> Self {
        let default_observe: ObserveExternalStderrWrites = Arc::new(observe_process_stderr_writes);
        Self {
            io: options.io.unwrap_or_else(|| Box::new(ProcessIo)),
            clock: options
                .clock
                .unwrap_or_else(|| Arc::new(SystemClock::default())),
            was_raw: false,
            tmux_exec_file: options.tmux_exec_file,
            on_external_stdout_write: options.on_external_stdout_write,
            original_stdout_write: None,
            guard_write: None,
            raw_stdout_write: None,
            observe_external_stderr_writes: options
                .observe_external_stderr_writes
                .unwrap_or(default_observe),
            stop_external_stderr_observation: None,
            external_write_observers: Arc::new(Mutex::new(Vec::new())),
            next_observer_id: 0,
            keyboard_negotiation_settled: false,
            cursor_query_timed_out: false,
            cursor_query: None,
            forwarding_external_write: Arc::new(AtomicBool::new(false)),
            input_handler: None,
            resize_handler: None,
            kitty_protocol_active: false,
            modify_other_keys_active: false,
            keyboard_protocol_pushed: false,
            keyboard_protocol_negotiation_buffer: String::new(),
            discarding_private_response: false,
            keyboard_protocol_buffer_flush_due: None,
            stdin_buffer: None,
            stdin_data_listening: false,
            stdin_error_subscription: None,
            stdin_error_cleanup_due: None,
            progress_due: None,
            last_size: None,
            write_log_path: resolve_write_log_path(),
        }
    }

    pub fn modify_other_keys_active(&self) -> bool {
        self.modify_other_keys_active
    }

    fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    fn settle_cursor_query(&mut self, position: Option<CursorPosition>) {
        if let Some(pending) = self.cursor_query.take() {
            pending.ticket.settle(position);
        }
    }

    fn issue_cursor_query(&mut self) {
        let settled = self.keyboard_negotiation_settled;
        let Some(pending) = self.cursor_query.as_mut() else {
            return;
        };
        if !settled || pending.issued {
            return;
        }
        pending.issued = true;
        let pane = pending.tmux_pane.clone();
        let deadline = pending.deadline;
        self.raw_write("\x1b[?6n");
        let Some(pane) = pane else {
            return;
        };
        match query_tmux_cursor_position(
            &pane,
            deadline,
            self.tmux_exec_file.as_ref(),
            self.clock.as_ref(),
        ) {
            TmuxCursorQuery::Done(position) => self.finish_tmux_query(position),
            TmuxCursorQuery::SecondReadAt { due, first } => {
                if let Some(pending) = self.cursor_query.as_mut() {
                    pending.tmux_second_read = Some((due, first));
                }
            }
        }
    }

    fn finish_tmux_query(&mut self, position: Option<CursorPosition>) {
        let Some(deadline) = self.cursor_query.as_ref().map(|p| p.deadline) else {
            return;
        };
        if self.now() >= deadline {
            self.cursor_query_timed_out = true;
        }
        let result = if self.cursor_query_timed_out {
            None
        } else {
            position
        };
        self.settle_cursor_query(result);
    }

    fn raw_write(&mut self, data: &str) {
        if let Some(raw) = &self.raw_stdout_write {
            raw(data);
            return;
        }
        self.io.write(data);
    }

    fn install_external_stderr_observer(&mut self) {
        let empty = self
            .external_write_observers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty();
        if self.stop_external_stderr_observation.is_some() || empty {
            return;
        }
        let observers = Arc::clone(&self.external_write_observers);
        let listener: ExternalWriteListener = Arc::new(move || note_external_write(&observers));
        self.stop_external_stderr_observation =
            Some((self.observe_external_stderr_writes)(listener));
    }

    fn install_external_stdout_guard(&mut self) {
        let no_observers = self
            .external_write_observers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty();
        if (self.on_external_stdout_write.is_none() && no_observers)
            || self.original_stdout_write.is_some()
        {
            return;
        }
        let original = process_stdio::stdout_writer();
        let raw = Arc::clone(&original);
        self.original_stdout_write = Some(original);
        self.raw_stdout_write = Some(Arc::clone(&raw));
        let handler = self.on_external_stdout_write.clone();
        let observers = Arc::clone(&self.external_write_observers);
        let forwarding = Arc::clone(&self.forwarding_external_write);
        let guard: StreamWrite = Arc::new(move |text: &str| {
            let Some(handler) = handler
                .as_ref()
                .filter(|_| !forwarding.load(Ordering::SeqCst))
            else {
                note_external_write(&observers);
                raw(text);
                return;
            };
            forwarding.store(true, Ordering::SeqCst);
            if handler(text).is_err() {
                note_external_write(&observers);
                raw(text);
            }
            forwarding.store(false, Ordering::SeqCst);
        });
        self.guard_write = Some(Arc::clone(&guard));
        process_stdio::set_stdout_writer(guard);
    }

    fn remove_external_stdout_guard(&mut self) {
        let Some(original) = self.original_stdout_write.take() else {
            return;
        };
        process_stdio::set_stdout_writer(original);
        self.guard_write = None;
        self.raw_stdout_write = None;
    }

    /// Stdout writer installed while the guard is active (\`process.stdout.write\` identity).
    pub fn is_stdout_guard_installed(&self) -> bool {
        self.guard_write
            .as_ref()
            .is_some_and(|guard| Arc::ptr_eq(guard, &process_stdio::stdout_writer()))
    }

    fn setup_stdin_buffer(&mut self) {
        let escape_timeout = resolve_escape_timeout_ms(&process_env::current());
        let options = StdinBufferOptions {
            timeout: None,
            // setTimeout truncates fractional delays.
            escape_timeout: Some(escape_timeout as u64),
        };
        self.stdin_buffer = Some(StdinBuffer::with_clock(options, Arc::clone(&self.clock)));
    }

    pub(crate) fn query_and_enable_kitty_protocol(&mut self) {
        self.setup_stdin_buffer();
        self.stdin_data_listening = true;
        if !keyboard_enhancement_enabled() {
            return;
        }
        if process_env::var("TMUX").is_some() || process_env::var("TMUX_PANE").is_some() {
            self.enable_modify_other_keys();
        }
        self.keyboard_protocol_pushed = true;
        self.clear_keyboard_protocol_negotiation_buffer();
        let query = KITTY_KEYBOARD_PROTOCOL_QUERY.clone();
        self.raw_write(&query);
    }

    #[cfg(test)]
    pub(crate) fn set_input_handler(&mut self, handler: InputHandler) {
        self.input_handler = Some(handler);
    }

    #[cfg(test)]
    pub(crate) fn set_was_raw(&mut self, was_raw: bool) {
        self.was_raw = was_raw;
    }

    /// Feeds stdin data (the \`process.stdin\` "data" event).
    pub fn feed<'a>(&mut self, data: impl Into<StdinInput<'a>>) {
        if !self.stdin_data_listening {
            return;
        }
        let Some(buffer) = self.stdin_buffer.as_mut() else {
            return;
        };
        let events = buffer.process(data);
        self.handle_stdin_events(events);
    }

    fn handle_stdin_events(&mut self, events: Vec<StdinEvent>) {
        for event in events {
            match event {
                StdinEvent::Data(sequence) => self.handle_stdin_sequence(&sequence),
                StdinEvent::Paste(content) => {
                    if let Some(handler) = self.input_handler.as_mut() {
                        handler(&format!("\x1b[200~{content}\x1b[201~"));
                    }
                }
            }
        }
    }

    fn handle_stdin_sequence(&mut self, sequence: &str) {
        if self.discarding_private_response {
            if sequence.starts_with('\x1b') {
                self.discarding_private_response = false;
            } else {
                if FINAL_BYTE_PATTERN.is_match(sequence) {
                    self.discarding_private_response = false;
                }
                return;
            }
        }
        match self.read_keyboard_protocol_negotiation_sequence(sequence) {
            NegotiationRead::Pending => {
                // Wait briefly for the rest of a split Kitty response.
                self.schedule_keyboard_protocol_negotiation_buffer_flush();
            }
            NegotiationRead::Sequence(negotiation) => {
                self.handle_keyboard_protocol_negotiation_sequence(negotiation)
            }
            NegotiationRead::None => self.forward_input_sequence(sequence),
        }
    }

    fn handle_keyboard_protocol_negotiation_sequence(
        &mut self,
        negotiation: KeyboardProtocolNegotiationSequence,
    ) {
        self.clear_keyboard_protocol_negotiation_buffer();
        match negotiation {
            KeyboardProtocolNegotiationSequence::CursorPosition(position) => {
                if self
                    .cursor_query
                    .as_ref()
                    .is_some_and(|q| q.issued && q.tmux_pane.is_none())
                {
                    self.settle_cursor_query(Some(position));
                }
                return;
            }
            KeyboardProtocolNegotiationSequence::KittyFlags { flags } => {
                self.keyboard_negotiation_settled = true;
                self.issue_cursor_query();
                if flags != 0 {
                    self.disable_modify_other_keys();
                    if !self.kitty_protocol_active {
                        self.kitty_protocol_active = true;
                        set_kitty_protocol_active(true);
                    }
                } else {
                    self.enable_modify_other_keys();
                }
                return;
            }
            KeyboardProtocolNegotiationSequence::DeviceAttributes => {
                self.keyboard_negotiation_settled = true;
                self.issue_cursor_query();
            }
        }
        if !self.kitty_protocol_active {
            self.enable_modify_other_keys();
        }
    }

    fn read_keyboard_protocol_negotiation_sequence(&mut self, sequence: &str) -> NegotiationRead {
        if !self.keyboard_protocol_negotiation_buffer.is_empty() {
            let buffered = format!("{}{sequence}", self.keyboard_protocol_negotiation_buffer);
            if let Some(negotiation) = parse_keyboard_protocol_negotiation_sequence(&buffered) {
                self.clear_keyboard_protocol_negotiation_buffer();
                return NegotiationRead::Sequence(negotiation);
            }
            if is_keyboard_protocol_negotiation_sequence_prefix(&buffered) {
                self.set_keyboard_protocol_negotiation_buffer(buffered);
                return NegotiationRead::Pending;
            }
            self.flush_keyboard_protocol_negotiation_buffer_as_input(false);
        }
        if let Some(negotiation) = parse_keyboard_protocol_negotiation_sequence(sequence) {
            return NegotiationRead::Sequence(negotiation);
        }
        if is_keyboard_protocol_negotiation_sequence_prefix(sequence) {
            self.set_keyboard_protocol_negotiation_buffer(sequence.to_string());
            return NegotiationRead::Pending;
        }
        NegotiationRead::None
    }

    fn set_keyboard_protocol_negotiation_buffer(&mut self, sequence: String) {
        self.keyboard_protocol_buffer_flush_due = None;
        self.keyboard_protocol_negotiation_buffer = sequence;
    }

    fn clear_keyboard_protocol_negotiation_buffer(&mut self) {
        self.keyboard_protocol_buffer_flush_due = None;
        self.keyboard_protocol_negotiation_buffer.clear();
    }

    fn flush_keyboard_protocol_negotiation_buffer_as_input(&mut self, discard_tail: bool) {
        if self.keyboard_protocol_negotiation_buffer.is_empty() {
            return;
        }
        let sequence = std::mem::take(&mut self.keyboard_protocol_negotiation_buffer);
        self.clear_keyboard_protocol_negotiation_buffer();
        if PRIVATE_PREFIX_PATTERN.is_match(&sequence) {
            self.discarding_private_response = discard_tail;
            return;
        }
        self.forward_input_sequence(&sequence);
    }

    fn schedule_keyboard_protocol_negotiation_buffer_flush(&mut self) {
        if self.keyboard_protocol_negotiation_buffer.is_empty()
            || self.keyboard_protocol_buffer_flush_due.is_some()
        {
            return;
        }
        self.keyboard_protocol_buffer_flush_due =
            Some(self.now() + KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS);
    }

    fn forward_input_sequence(&mut self, sequence: &str) {
        if self.input_handler.is_none() {
            return;
        }
        let should_detect =
            sequence == "\r" && (is_apple_terminal_session() || node_platform() == "win32");
        let shift = should_detect && is_native_modifier_pressed(ModifierKey::Shift);
        let input = normalize_native_shift_enter_input(sequence, should_detect, shift);
        if let Some(handler) = self.input_handler.as_mut() {
            handler(&input);
        }
    }

    fn enable_modify_other_keys(&mut self) {
        if self.kitty_protocol_active || self.modify_other_keys_active {
            return;
        }
        self.raw_write("\x1b[>4;2m");
        self.modify_other_keys_active = true;
    }

    fn disable_modify_other_keys(&mut self) {
        if !self.modify_other_keys_active {
            return;
        }
        self.raw_write("\x1b[>4;0m");
        self.modify_other_keys_active = false;
    }

    /// On Windows, ENABLE_VIRTUAL_TERMINAL_INPUT makes the console send VT sequences for
    /// modified keys (e.g. \`\x1b[Z\` for Shift+Tab).
    fn enable_windows_vt_input(&mut self) {
        if node_platform() != "win32" {
            return;
        }
        if let Some(helper) = get_native_platform_helper() {
            let _ = helper.enable_virtual_terminal_input();
        }
    }

    fn disable_keyboard_protocols(&mut self) {
        let should_disable_kitty_protocol =
            self.keyboard_protocol_pushed || self.kitty_protocol_active;
        self.clear_keyboard_protocol_negotiation_buffer();
        if should_disable_kitty_protocol {
            self.raw_write("\x1b[<u");
            self.keyboard_protocol_pushed = false;
            self.kitty_protocol_active = false;
            set_kitty_protocol_active(false);
        }
        self.disable_modify_other_keys();
    }

    fn schedule_stdin_error_handler_cleanup(&mut self) {
        if self.stdin_error_subscription.is_none() || self.stdin_error_cleanup_due.is_some() {
            return;
        }
        // Keep the guard armed briefly past stop(): a late PTY failure racing the exit path
        // must not crash the process after the TUI tore down.
        self.stdin_error_cleanup_due = Some(self.now() + STDIN_ERROR_HANDLER_GRACE_MS);
    }

    fn clear_progress_interval(&mut self) -> bool {
        self.progress_due.take().is_some()
    }

    /// Reads stdin for up to \`max_wait_ms\` (bounded by the next timer), feeds it, fires due
    /// timers and reports resizes: one turn of senpi's event loop.
    pub fn pump(&mut self, max_wait_ms: u64) -> Result<(), TerminalError> {
        let now = self.now();
        let wait = self
            .next_timer_deadline()
            .map_or(max_wait_ms, |due| due.saturating_sub(now).min(max_wait_ms));
        match self.io.read_input(wait) {
            Ok(Some(bytes)) => self.feed(bytes.as_slice()),
            Ok(None) => {}
            Err(error) => dispatch_stdin_error(error)?,
        }
        self.run_due_timers();
        self.check_resize();
        Ok(())
    }

    /// The \`process.stdout\` "resize" event: compares the tty size with the last one seen.
    pub fn check_resize(&mut self) {
        let size = self.io.size();
        if size == self.last_size {
            return;
        }
        self.last_size = size;
        if let Some(handler) = self.resize_handler.as_mut() {
            handler();
        }
    }

    fn earliest_due_timer(&self) -> Option<(u64, DueTimer)> {
        let query = self.cursor_query.as_ref();
        [
            self.stdin_buffer
                .as_ref()
                .and_then(StdinBuffer::deadline)
                .map(|d| (d, DueTimer::StdinBuffer)),
            self.keyboard_protocol_buffer_flush_due
                .map(|d| (d, DueTimer::NegotiationFlush)),
            query
                .and_then(|q| q.tmux_second_read)
                .map(|(d, _)| (d, DueTimer::TmuxSecondRead)),
            query.map(|q| (q.timer_due, DueTimer::CursorTimeout)),
            self.progress_due.map(|d| (d, DueTimer::Progress)),
            self.stdin_error_cleanup_due
                .map(|d| (d, DueTimer::StdinErrorCleanup)),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(due, _)| *due)
    }

    fn fire(&mut self, timer: DueTimer) {
        match timer {
            DueTimer::StdinBuffer => {
                if let Some(buffer) = self.stdin_buffer.as_mut() {
                    let events = buffer.fire_timer();
                    self.handle_stdin_events(events);
                }
            }
            DueTimer::NegotiationFlush => {
                self.keyboard_protocol_buffer_flush_due = None;
                self.flush_keyboard_protocol_negotiation_buffer_as_input(true);
            }
            DueTimer::TmuxSecondRead => {
                let Some(pending) = self.cursor_query.as_mut() else {
                    return;
                };
                let Some((_, first)) = pending.tmux_second_read.take() else {
                    return;
                };
                let pane = pending.tmux_pane.clone().unwrap_or_default();
                let deadline = pending.deadline;
                let position = finish_tmux_cursor_query(
                    &pane,
                    deadline,
                    self.tmux_exec_file.as_ref(),
                    self.clock.as_ref(),
                    first,
                );
                self.finish_tmux_query(position);
            }
            DueTimer::CursorTimeout => {
                // CPR has no request id: after a timeout a late reply cannot safely be
                // associated with a newer query. Stay fail-closed until the next start.
                self.cursor_query_timed_out = true;
                self.settle_cursor_query(None);
            }
            DueTimer::Progress => {
                self.progress_due = Some(self.now() + TERMINAL_PROGRESS_KEEPALIVE_MS);
                self.raw_write(TERMINAL_PROGRESS_ACTIVE_SEQUENCE);
            }
            DueTimer::StdinErrorCleanup => {
                self.stdin_error_cleanup_due = None;
                if let Some(id) = self.stdin_error_subscription.take() {
                    unsubscribe_from_stdin_errors(id);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DueTimer {
    StdinBuffer,
    NegotiationFlush,
    TmuxSecondRead,
    CursorTimeout,
    Progress,
    StdinErrorCleanup,
}

enum NegotiationRead {
    Sequence(KeyboardProtocolNegotiationSequence),
    Pending,
    None,
}

impl Drop for ProcessTerminal {
    fn drop(&mut self) {
        if let Some(id) = self.stdin_error_subscription.take() {
            unsubscribe_from_stdin_errors(id);
        }
        self.remove_external_stdout_guard();
    }
}

impl Terminal for ProcessTerminal {
    fn start(&mut self, on_input: InputHandler, on_resize: ResizeHandler) {
        self.install_external_stdout_guard();
        self.install_external_stderr_observer();
        self.keyboard_negotiation_settled = !keyboard_enhancement_enabled();
        self.cursor_query_timed_out = false;
        self.discarding_private_response = false;
        self.input_handler = Some(on_input);
        self.resize_handler = Some(on_resize);

        self.was_raw = self.io.is_raw();
        // senpi ignores setRawMode's absence; a failing raw-mode switch leaves cooked input.
        let _ = self.io.set_raw_mode(true);
        self.io.resume();

        // A vanished controlling PTY fails the next stdin read with EIO; keep it from killing
        // the process. Swallow only, so input resumes if this pgrp regains the foreground.
        self.stdin_error_cleanup_due = None;
        if self.stdin_error_subscription.is_none() {
            self.stdin_error_subscription = Some(subscribe_to_stdin_errors());
        }

        self.raw_write("\x1b[?2004h");
        self.last_size = self.io.size();
        // Dimensions may be stale after suspend/resume (SIGWINCH is lost while stopped).
        self.io.refresh_dimensions();
        self.enable_windows_vt_input();
        self.query_and_enable_kitty_protocol();
    }

    fn stop(&mut self) -> Result<(), TerminalError> {
        self.settle_cursor_query(None);
        if let Some(stop) = self.stop_external_stderr_observation.take() {
            stop();
        }
        if self.clear_progress_interval() {
            self.raw_write(TERMINAL_PROGRESS_CLEAR_SEQUENCE);
        }
        self.raw_write("\x1b[?2004l");
        self.disable_keyboard_protocols();
        if let Some(mut buffer) = self.stdin_buffer.take() {
            buffer.destroy();
        }
        self.stdin_data_listening = false;
        self.input_handler = None;
        self.resize_handler = None;
        // Pause stdin so buffered input (e.g. Ctrl+D) is not re-read after raw mode ends.
        self.io.pause();
        let was_raw = self.was_raw;
        let restored = self.io.set_raw_mode(was_raw);
        if let Err(error) = restored
            && !is_dead_terminal_error(&error)
        {
            return Err(error);
        }
        self.remove_external_stdout_guard();
        self.schedule_stdin_error_handler_cleanup();
        Ok(())
    }

    fn drain_input(&mut self, max_ms: u64, idle_ms: u64) {
        self.disable_keyboard_protocols();
        let previous_handler = self.input_handler.take();
        let mut last_data_time = self.now();
        let end_time = self.now() + max_ms;
        loop {
            let now = self.now();
            if end_time <= now || now.saturating_sub(last_data_time) >= idle_ms {
                break;
            }
            match self.io.read_input(idle_ms.min(end_time - now)) {
                Ok(Some(bytes)) => {
                    last_data_time = self.now();
                    self.feed(bytes.as_slice());
                }
                Ok(None) => {}
                // A detached terminal has nothing left to drain.
                Err(_) => break,
            }
        }
        self.input_handler = previous_handler;
    }

    fn query_cursor_position(&mut self) -> Option<CursorQueryTicket> {
        if let Some(pending) = &self.cursor_query {
            return Some(pending.ticket.clone());
        }
        if self.input_handler.is_none() || self.cursor_query_timed_out {
            return Some(CursorQueryTicket::resolved(None));
        }
        let ticket = CursorQueryTicket::default();
        let now = self.now();
        self.cursor_query = Some(PendingCursorQuery {
            ticket: ticket.clone(),
            timer_due: now + CURSOR_QUERY_TIMEOUT_MS,
            issued: false,
            tmux_pane: process_env::var("TMUX_PANE"),
            deadline: now + CURSOR_QUERY_TIMEOUT_MS,
            tmux_second_read: None,
        });
        self.issue_cursor_query();
        Some(ticket)
    }

    fn observe_external_writes(
        &mut self,
        listener: ExternalWriteListener,
    ) -> Option<Box<dyn FnOnce()>> {
        self.next_observer_id += 1;
        let id = self.next_observer_id;
        self.external_write_observers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((id, listener));
        if self.input_handler.is_some() {
            self.install_external_stdout_guard();
            self.install_external_stderr_observer();
        }
        let observers = Arc::clone(&self.external_write_observers);
        Some(Box::new(move || {
            observers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|(existing, _)| *existing != id);
        }))
    }

    fn write(&mut self, data: &str) {
        self.raw_write(data);
        if !self.write_log_path.is_empty()
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.write_log_path)
        {
            // Logging errors are ignored.
            let _ = file.write_all(data.as_bytes());
        }
    }

    fn columns(&self) -> u16 {
        dimension(self.io.size().map(|s| s.0), "COLUMNS", 80)
    }

    fn rows(&self) -> u16 {
        dimension(self.io.size().map(|s| s.1), "LINES", 24)
    }

    fn kitty_protocol_active(&self) -> bool {
        self.kitty_protocol_active
    }

    fn move_by(&mut self, lines: i64) {
        match lines.cmp(&0) {
            std::cmp::Ordering::Greater => self.raw_write(&format!("\x1b[{lines}B")),
            std::cmp::Ordering::Less => self.raw_write(&format!("\x1b[{}A", lines.unsigned_abs())),
            std::cmp::Ordering::Equal => {}
        }
    }

    fn hide_cursor(&mut self) {
        self.raw_write("\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.raw_write("\x1b[?25h");
    }

    fn clear_line(&mut self) {
        self.raw_write("\x1b[K");
    }

    fn clear_from_cursor(&mut self) {
        self.raw_write("\x1b[J");
    }

    fn clear_screen(&mut self) {
        self.raw_write("\x1b[2J\x1b[H");
    }

    fn set_title(&mut self, title: &str) {
        // Control characters are stripped so a title cannot terminate the OSC early.
        let sanitized = TITLE_CONTROL_PATTERN.replace_all(title, "");
        self.raw_write(&format!("\x1b]0;{sanitized}\x07"));
    }

    fn set_progress(&mut self, active: bool) {
        if active {
            self.raw_write(TERMINAL_PROGRESS_ACTIVE_SEQUENCE);
            if self.progress_due.is_none() {
                self.progress_due = Some(self.now() + TERMINAL_PROGRESS_KEEPALIVE_MS);
            }
        } else {
            self.clear_progress_interval();
            self.raw_write(TERMINAL_PROGRESS_CLEAR_SEQUENCE);
        }
    }

    fn next_timer_deadline(&self) -> Option<u64> {
        self.earliest_due_timer().map(|(due, _)| due)
    }

    fn run_due_timers(&mut self) {
        while let Some((due, timer)) = self.earliest_due_timer() {
            if due > self.now() {
                break;
            }
            self.fire(timer);
        }
    }
}

/// \`stdout.columns || Number(env) || fallback\`.
fn dimension(tty: Option<u16>, env_name: &str, fallback: u16) -> u16 {
    if let Some(value) = tty.filter(|v| *v > 0) {
        return value;
    }
    let from_env = process_env::var(env_name).map_or(f64::NAN, |v| js_number(&v));
    if from_env.is_finite() && from_env >= 1.0 && from_env <= f64::from(u16::MAX) {
        // Truncation mirrors the integer column count the renderer consumes.
        return from_env as u16;
    }
    fallback
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
