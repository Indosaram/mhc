//! Spawns `sg` with streamed JSON output under record, payload, stderr and time bounds.

use std::io::Read;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use serde_json::Map;
use serde_json::Value;

use crate::abort::AbortSignal;

pub const MAX_JSON_RECORD_BYTES: usize = 1024 * 1024;
pub const MAX_STDERR_BYTES: usize = 64 * 1024;
pub const MAX_MCP_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_MATCHES: u64 = 500;
pub const DEFAULT_MATCHES: u64 = 50;
pub const DEFAULT_TIMEOUT_MS: u64 = 300_000;
pub const MAX_TIMEOUT_MS: u64 = 300_000;

const KILL_GRACE: Duration = Duration::from_millis(1_000);
const WAIT_SLICE: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SgRunnerErrorCode {
    Aborted,
    EncodingError,
    OutputParseFailed,
    OutputTooLarge,
    SgFailed,
    Timeout,
}

impl SgRunnerErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Aborted => "ABORTED",
            Self::EncodingError => "ENCODING_ERROR",
            Self::OutputParseFailed => "OUTPUT_PARSE_FAILED",
            Self::OutputTooLarge => "OUTPUT_TOO_LARGE",
            Self::SgFailed => "SG_FAILED",
            Self::Timeout => "TIMEOUT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SgTruncationReason {
    MatchLimit,
    OutputCap,
    SgOutputTruncated,
}

impl SgTruncationReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MatchLimit => "match_limit",
            Self::OutputCap => "output_cap",
            Self::SgOutputTruncated => "sg_output_truncated",
        }
    }
}

pub fn reason_value(reason: Option<SgTruncationReason>) -> Value {
    reason.map_or(Value::Null, |reason| Value::from(reason.as_str()))
}

#[derive(Debug, Clone)]
pub struct SgRunnerInput<'a> {
    pub sg_path: &'a str,
    pub args: Vec<String>,
    pub workdir: &'a str,
    /// Full replacement environment; `None` inherits the server's environment.
    pub env: Option<Vec<(String, String)>>,
    pub max_matches: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub signal: Option<&'a AbortSignal>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SgRunnerResult {
    pub records: Vec<Map<String, Value>>,
    pub truncated: bool,
    pub reason: Option<SgTruncationReason>,
    pub salvaged_records: usize,
    pub stderr: String,
    pub duration_ms: u64,
    pub at_least_matches: usize,
    pub max_payload_bytes: usize,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SgRunnerError {
    pub code: SgRunnerErrorCode,
    pub message: String,
    pub stderr: String,
    pub duration_ms: u64,
}

impl std::fmt::Display for SgRunnerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SgRunnerError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopReason {
    Abort,
    Limit,
    Timeout,
}

enum Event {
    Stdout(Vec<u8>),
    StdoutClosed,
}

struct State {
    records: Vec<Map<String, Value>>,
    serialized_records_bytes: usize,
    pending: Vec<u8>,
    malformed: bool,
    fatal_error: Option<SgRunnerErrorCode>,
    stop_reason: Option<StopReason>,
    truncation_reason: Option<SgTruncationReason>,
    max_matches: usize,
}

impl State {
    fn stop(&mut self, reason: StopReason) {
        if self.stop_reason.is_none() {
            self.stop_reason = Some(reason);
        }
    }

    fn fail_output(&mut self, code: SgRunnerErrorCode) {
        if self.fatal_error.is_none() {
            self.fatal_error = Some(code);
        }
        self.stop(StopReason::Limit);
    }

    fn parse_line(&mut self, line: &[u8]) -> bool {
        let value = line.strip_suffix(b"\r").unwrap_or(line);
        if value.is_empty() {
            return true;
        }
        if value.len() > MAX_JSON_RECORD_BYTES {
            self.fail_output(SgRunnerErrorCode::OutputTooLarge);
            return false;
        }
        let Ok(text) = std::str::from_utf8(value) else {
            self.fail_output(SgRunnerErrorCode::EncodingError);
            return false;
        };
        let record = match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(record)) => record,
            _ => {
                self.malformed = true;
                return true;
            }
        };
        let serialized_bytes = Value::Object(record.clone()).to_string().len();
        let separator = usize::from(!self.records.is_empty());
        let next_aggregate = self.serialized_records_bytes + serialized_bytes + separator;
        if next_aggregate > MAX_MCP_PAYLOAD_BYTES {
            self.truncation_reason = Some(SgTruncationReason::OutputCap);
            self.stop(StopReason::Limit);
            return false;
        }
        self.records.push(record);
        self.serialized_records_bytes = next_aggregate;
        if self.records.len() > self.max_matches {
            self.records.truncate(self.max_matches);
            self.truncation_reason = Some(SgTruncationReason::MatchLimit);
            self.stop(StopReason::Limit);
            return false;
        }
        true
    }

    fn push_stdout(&mut self, chunk: &[u8]) {
        if self.stop_reason == Some(StopReason::Limit) {
            return;
        }
        self.pending.extend_from_slice(chunk);
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=newline).collect();
            if !self.parse_line(&line[..line.len() - 1]) {
                return;
            }
        }
        if self.pending.len() > MAX_JSON_RECORD_BYTES {
            self.fail_output(SgRunnerErrorCode::OutputTooLarge);
        }
    }
}

struct StderrCapture {
    kept: Vec<u8>,
    has_sg_error_diagnostic: bool,
}

fn capture_stderr(mut stream: impl Read) -> StderrCapture {
    let mut capture = StderrCapture {
        kept: Vec::new(),
        has_sg_error_diagnostic: false,
    };
    let mut line_prefix = String::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let chunk = &buffer[..read];
        if !capture.has_sg_error_diagnostic {
            for byte in chunk {
                if *byte == b'\n' || *byte == b'\r' {
                    line_prefix.clear();
                } else if line_prefix.chars().count() < 80 {
                    line_prefix.push(char::from(*byte));
                    if is_sg_error_prefix(&line_prefix) {
                        capture.has_sg_error_diagnostic = true;
                    }
                }
            }
        }
        let remaining = MAX_STDERR_BYTES.saturating_sub(capture.kept.len());
        capture
            .kept
            .extend_from_slice(&chunk[..remaining.min(chunk.len())]);
    }
    capture
}

fn is_sg_error_prefix(prefix: &str) -> bool {
    let rest = prefix.trim_start_matches(['\t', ' ']);
    if let Some(after) = rest.strip_prefix("ERROR") {
        return after
            .chars()
            .next()
            .is_none_or(|next| !(next.is_ascii_alphanumeric() || next == '_'));
    }
    rest.starts_with("error:")
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Drops a trailing incomplete UTF-8 sequence (a chunk cut mid-character) before lossy decoding.
pub fn decode_stderr(bytes: &[u8]) -> String {
    let len = bytes.len();
    let mut start = len;
    while start > 0 && (bytes[start - 1] & 0xc0) == 0x80 && len - (start - 1) <= 3 {
        start -= 1;
    }
    let complete = if start > 0 {
        let lead = bytes[start - 1];
        let width = match lead {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 1,
        };
        if width > len - (start - 1) {
            &bytes[..start - 1]
        } else {
            bytes
        }
    } else {
        bytes
    };
    let text = String::from_utf8_lossy(complete);
    truncate_utf8(&text, MAX_STDERR_BYTES).to_owned()
}

fn elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Ok(pid) = libc::pid_t::try_from(child.id()) {
            // SAFETY: kill(2) on our own child's pid has no memory-safety preconditions.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
            return;
        }
    }
    let _ = child.kill();
}

fn wait_with_grace(
    child: &mut Child,
    state: &mut State,
    signal: Option<&AbortSignal>,
    deadline: Instant,
) -> Option<i32> {
    let mut term_sent_at: Option<Instant> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) => {}
            Err(_) => return None,
        }
        if state.stop_reason.is_none() {
            if signal.is_some_and(AbortSignal::is_aborted) {
                state.stop(StopReason::Abort);
            } else if Instant::now() >= deadline {
                state.stop(StopReason::Timeout);
            }
        }
        if state.stop_reason.is_some() {
            match term_sent_at {
                None => {
                    terminate(child);
                    term_sent_at = Some(Instant::now());
                }
                Some(at) if at.elapsed() >= KILL_GRACE => {
                    let _ = child.kill();
                }
                Some(_) => {}
            }
        }
        thread::sleep(WAIT_SLICE);
    }
}

fn runner_error(
    code: SgRunnerErrorCode,
    message: impl Into<String>,
    stderr: String,
    duration_ms: u64,
) -> SgRunnerError {
    SgRunnerError {
        code,
        message: message.into(),
        stderr,
        duration_ms,
    }
}

pub fn spawn_sg_runner(input: SgRunnerInput<'_>) -> Result<SgRunnerResult, SgRunnerError> {
    let started_at = Instant::now();
    if input.signal.is_some_and(AbortSignal::is_aborted) {
        return Err(runner_error(
            SgRunnerErrorCode::Aborted,
            "ast-grep request was aborted",
            String::new(),
            0,
        ));
    }
    let max_matches =
        usize::try_from(input.max_matches.unwrap_or(DEFAULT_MATCHES)).unwrap_or(usize::MAX);
    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));

    let mut command = Command::new(input.sg_path);
    command
        .args(&input.args)
        .current_dir(Path::new(input.workdir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(env) = &input.env {
        command.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
    }
    let mut child = command.spawn().map_err(|error| {
        runner_error(
            SgRunnerErrorCode::SgFailed,
            error.to_string(),
            String::new(),
            elapsed_ms(started_at),
        )
    })?;

    let (sender, receiver) = mpsc::channel::<Event>();
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let stdout_thread = thread::spawn(move || {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if sender.send(Event::Stdout(buffer[..read].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = sender.send(Event::StdoutClosed);
    });
    let stderr = child.stderr.take().expect("stderr is piped");
    let stderr_thread = thread::spawn(move || capture_stderr(stderr));

    let mut state = State {
        records: Vec::new(),
        serialized_records_bytes: 2,
        pending: Vec::new(),
        malformed: false,
        fatal_error: None,
        stop_reason: None,
        truncation_reason: None,
        max_matches,
    };

    loop {
        if state.stop_reason.is_some() {
            break;
        }
        if input.signal.is_some_and(AbortSignal::is_aborted) {
            state.stop(StopReason::Abort);
            break;
        }
        if started_at.elapsed() >= timeout {
            state.stop(StopReason::Timeout);
            break;
        }
        match receiver.recv_timeout(WAIT_SLICE) {
            Ok(Event::Stdout(chunk)) => state.push_stdout(&chunk),
            Ok(Event::StdoutClosed) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }

    drop(receiver);
    let exit_code = wait_with_grace(&mut child, &mut state, input.signal, started_at + timeout);
    if state.stop_reason.is_none() {
        let _ = stdout_thread.join();
    }
    let capture = stderr_thread.join().unwrap_or(StderrCapture {
        kept: Vec::new(),
        has_sg_error_diagnostic: false,
    });
    let stderr = decode_stderr(&capture.kept);
    let duration = elapsed_ms(started_at);

    match state.stop_reason {
        Some(StopReason::Abort) => {
            return Err(runner_error(
                SgRunnerErrorCode::Aborted,
                "ast-grep request was aborted",
                stderr,
                duration,
            ));
        }
        Some(StopReason::Timeout) => {
            return Err(runner_error(
                SgRunnerErrorCode::Timeout,
                "ast-grep request timed out",
                stderr,
                duration,
            ));
        }
        _ => {}
    }
    const UNSAFE_OUTPUT: &str = "ast-grep output could not be read safely";
    if let Some(code) = state.fatal_error {
        return Err(runner_error(code, UNSAFE_OUTPUT, stderr, duration));
    }
    if state.stop_reason != Some(StopReason::Limit) && !state.pending.is_empty() {
        let pending = std::mem::take(&mut state.pending);
        state.parse_line(&pending);
    }
    if let Some(code) = state.fatal_error {
        return Err(runner_error(code, UNSAFE_OUTPUT, stderr, duration));
    }
    let failed_exit = exit_code != Some(0) && exit_code != Some(1);
    let diagnosed_exit_one = exit_code == Some(1) && capture.has_sg_error_diagnostic;
    if state.stop_reason.is_none() && (failed_exit || diagnosed_exit_one) {
        let code_text = exit_code.map_or_else(|| "unknown".to_owned(), |code| code.to_string());
        return Err(runner_error(
            SgRunnerErrorCode::SgFailed,
            format!("ast-grep exited with code {code_text}"),
            stderr,
            duration,
        ));
    }
    if state.malformed && state.records.is_empty() {
        return Err(runner_error(
            SgRunnerErrorCode::OutputParseFailed,
            "ast-grep produced no parseable JSON records",
            stderr,
            duration,
        ));
    }
    let limited = state.stop_reason == Some(StopReason::Limit) && state.truncation_reason.is_some();
    let salvaged = state.malformed && !state.records.is_empty();
    let count = state.records.len();
    Ok(SgRunnerResult {
        truncated: limited || salvaged,
        reason: if limited {
            state.truncation_reason
        } else if salvaged {
            Some(SgTruncationReason::SgOutputTruncated)
        } else {
            None
        },
        salvaged_records: if salvaged { count } else { 0 },
        stderr,
        duration_ms: duration,
        at_least_matches: if limited { count + 1 } else { count },
        max_payload_bytes: MAX_MCP_PAYLOAD_BYTES,
        exit_code,
        records: state.records,
    })
}
