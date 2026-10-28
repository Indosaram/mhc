//! Running the `ast-grep` program with bounded time and output (spec §5).

use crate::error::{
    ABORTED, ENCODING_ERROR, OUTPUT_PARSE_FAILED, OUTPUT_TOO_LARGE, Phase, SG_FAILED, TIMEOUT,
    ToolError,
};
use serde_json::{Map, Value, json};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::time::Instant;

pub const MAX_LINE_BYTES: usize = 1024 * 1024;
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_STDERR_BYTES: usize = 64 * 1024;
const KILL_GRACE: Duration = Duration::from_secs(1);

/// Cancellation signal of one call.
#[derive(Clone, Debug)]
pub struct AbortSignal(watch::Receiver<bool>);

/// The side that raises an [`AbortSignal`].
#[derive(Clone, Debug)]
pub struct AbortHandle(watch::Sender<bool>);

/// A fresh, unaborted signal pair.
pub fn abort_channel() -> (AbortHandle, AbortSignal) {
    let (tx, rx) = watch::channel(false);
    (AbortHandle(tx), AbortSignal(rx))
}

impl AbortHandle {
    pub fn abort(&self) {
        self.0.send_replace(true);
    }
}

impl AbortSignal {
    pub fn is_aborted(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once aborted (immediately if already so).
    pub async fn aborted(&mut self) {
        if self.0.wait_for(|aborted| *aborted).await.is_err() {
            // The handle was dropped without aborting: never resolves.
            std::future::pending::<()>().await;
        }
    }
}

/// Why collection stopped before the end of the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Truncation {
    MatchLimit,
    OutputCap,
    SgOutputTruncated,
}

impl Truncation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MatchLimit => "match_limit",
            Self::OutputCap => "output_cap",
            Self::SgOutputTruncated => "sg_output_truncated",
        }
    }
}

/// What one preview/search process produced.
#[derive(Debug)]
pub struct Collected {
    pub records: Vec<Map<String, Value>>,
    pub truncation: Option<Truncation>,
    pub salvaged: usize,
    pub exit_code: i32,
    pub stderr: String,
}

/// What a plain (apply) process produced.
#[derive(Debug)]
pub struct Finished {
    pub exit_code: i32,
    pub stderr: String,
}

/// Limits and context of one process run.
#[derive(Clone, Debug)]
pub struct RunSpec<'a> {
    pub program: &'a Path,
    pub args: &'a [String],
    pub workdir: &'a Path,
    pub deadline: Instant,
    pub phase: Phase,
    pub max_records: usize,
}

fn spawn(spec: &RunSpec<'_>) -> Result<Child, ToolError> {
    let mut command = Command::new(spec.program);
    command
        .args(spec.args)
        .current_dir(spec.workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command.spawn().map_err(|error| {
        ToolError::new(
            SG_FAILED,
            spec.phase,
            format!("Could not start {}: {error}", spec.program.display()),
        )
    })
}

/// Polite terminate, then a hard kill about one second later.
pub async fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            let _ = Command::new("kill")
                .args(["-TERM", "--", &format!("-{pid}")])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        if tokio::time::timeout(KILL_GRACE, child.wait()).await.is_ok() {
            return;
        }
        if let Some(pid) = child.id() {
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{pid}")])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        let _ = child.kill().await;
    }
    #[cfg(not(unix))]
    {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(KILL_GRACE, child.wait()).await;
        let _ = child.kill().await;
    }
}

async fn read_stderr(mut stream: impl AsyncRead + Unpin) -> String {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = MAX_STDERR_BYTES.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    }
    match String::from_utf8(kept) {
        Ok(text) => text,
        Err(error) => {
            // A cut in the middle of a character drops that partial character; any other
            // invalid byte is replaced.
            let cut_at_end = error.utf8_error().error_len().is_none();
            let valid = error.utf8_error().valid_up_to();
            let bytes = error.into_bytes();
            if cut_at_end {
                String::from_utf8_lossy(&bytes[..valid]).into_owned()
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            }
        }
    }
}

enum Line {
    Eof,
    Line(Vec<u8>),
    TooLarge,
}

/// One `\n`-terminated line, at most [`MAX_LINE_BYTES`] long.
async fn read_line(reader: &mut (impl AsyncBufRead + Unpin)) -> std::io::Result<Line> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if line.is_empty() { Line::Eof } else { Line::Line(line) });
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(at) => {
                line.extend_from_slice(&available[..at]);
                reader.consume(at + 1);
                return Ok(if line.len() > MAX_LINE_BYTES { Line::TooLarge } else { Line::Line(line) });
            }
            None => {
                let n = available.len();
                line.extend_from_slice(available);
                reader.consume(n);
                if line.len() > MAX_LINE_BYTES {
                    return Ok(Line::TooLarge);
                }
            }
        }
    }
}

fn stop_error(
    phase: Phase,
    timed_out: bool,
) -> ToolError {
    if timed_out {
        ToolError::new(TIMEOUT, phase, "The ast-grep call ran out of time.")
    } else {
        ToolError::new(ABORTED, phase, "The ast-grep call was aborted.")
    }
}

/// Run a `--json=stream` process and collect its records.
pub async fn collect(
    spec: &RunSpec<'_>,
    abort: &mut AbortSignal,
) -> Result<Collected, ToolError> {
    let mut child = spawn(spec)?;
    let stdout = child.stdout.take();
    let stderr_task = child.stderr.take().map(|s| tokio::spawn(read_stderr(s)));
    let mut reader = tokio::io::BufReader::new(stdout.ok_or_else(|| {
        ToolError::new(SG_FAILED, spec.phase, "The program has no stdout pipe.")
    })?);

    enum Stop {
        Done,
        Cut(Truncation),
        Fail(ToolError),
        Timeout,
        Abort,
    }

    let mut records: Vec<Map<String, Value>> = Vec::new();
    let mut payload = 0usize;
    let mut garbage = false;

    let stop = loop {
        let next = tokio::select! {
            line = read_line(&mut reader) => Some(line),
            () = tokio::time::sleep_until(spec.deadline) => None,
            () = abort.aborted() => {
                break Stop::Abort;
            }
        };
        let Some(line) = next else { break Stop::Timeout };
        let line = match line {
            Ok(Line::Eof) => break Stop::Done,
            Ok(Line::TooLarge) => {
                break Stop::Fail(ToolError::new(
                    OUTPUT_TOO_LARGE,
                    spec.phase,
                    "The program printed a line longer than 1 MiB.",
                ));
            }
            Ok(Line::Line(bytes)) => bytes,
            Err(error) => {
                break Stop::Fail(ToolError::new(
                    SG_FAILED,
                    spec.phase,
                    format!("Reading the program output failed: {error}"),
                ));
            }
        };
        let text = match String::from_utf8(line) {
            Ok(text) => text,
            Err(_) => {
                break Stop::Fail(ToolError::new(
                    ENCODING_ERROR,
                    spec.phase,
                    "The program output is not valid UTF-8.",
                ));
            }
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(Value::Object(record)) => {
                let size = serde_json::to_vec(&record).map_or(0, |bytes| bytes.len());
                if records.len() >= spec.max_records {
                    break Stop::Cut(Truncation::MatchLimit);
                }
                if payload + size > MAX_PAYLOAD_BYTES {
                    break Stop::Cut(Truncation::OutputCap);
                }
                payload += size;
                records.push(record);
            }
            _ => garbage = true,
        }
    };

    let early = !matches!(stop, Stop::Done);
    if early {
        terminate(&mut child).await;
    }
    match stop {
        Stop::Fail(error) => return Err(error),
        Stop::Timeout => return Err(stop_error(spec.phase, true)),
        Stop::Abort => return Err(stop_error(spec.phase, false)),
        Stop::Done | Stop::Cut(_) => {}
    }

    let exit_code = if early {
        // The child was terminated by us: its status is not an outcome.
        0
    } else {
        let status = tokio::select! {
            status = child.wait() => status,
            () = tokio::time::sleep_until(spec.deadline) => {
                terminate(&mut child).await;
                return Err(stop_error(spec.phase, true));
            }
            () = abort.aborted() => {
                terminate(&mut child).await;
                return Err(stop_error(spec.phase, false));
            }
        };
        status
            .map_err(|error| ToolError::new(SG_FAILED, spec.phase, format!("Waiting for the program failed: {error}")))?
            .code()
            .unwrap_or(-1)
    };
    let stderr = match stderr_task {
        Some(task) => tokio::time::timeout(KILL_GRACE, task).await.ok().and_then(Result::ok).unwrap_or_default(),
        None => String::new(),
    };

    let mut truncation = match stop {
        Stop::Cut(reason) => Some(reason),
        _ => None,
    };
    let mut salvaged = 0;
    if garbage && truncation.is_none() {
        if records.is_empty() {
            return Err(ToolError::new(
                OUTPUT_PARSE_FAILED,
                spec.phase,
                "The program output contained no JSON records.",
            )
            .with_details(json!({ "stderr": stderr })));
        }
        truncation = Some(Truncation::SgOutputTruncated);
        salvaged = records.len();
    }
    Ok(Collected {
        records,
        truncation,
        salvaged,
        exit_code,
        stderr,
    })
}

/// Run a process to the end, ignoring its stdout (the `--update-all` pass).
pub async fn finish(spec: &RunSpec<'_>, abort: &mut AbortSignal) -> Result<Finished, ToolError> {
    let mut child = spawn(spec)?;
    let stdout_task = child.stdout.take().map(|mut s| {
        tokio::spawn(async move {
            let mut sink = tokio::io::sink();
            let _ = tokio::io::copy(&mut s, &mut sink).await;
        })
    });
    let stderr_task = child.stderr.take().map(|s| tokio::spawn(read_stderr(s)));
    let status = tokio::select! {
        status = child.wait() => status,
        () = tokio::time::sleep_until(spec.deadline) => {
            terminate(&mut child).await;
            return Err(stop_error(spec.phase, true));
        }
        () = abort.aborted() => {
            terminate(&mut child).await;
            return Err(stop_error(spec.phase, false));
        }
    };
    let exit_code = status
        .map_err(|error| ToolError::new(SG_FAILED, spec.phase, format!("Waiting for the program failed: {error}")))?
        .code()
        .unwrap_or(-1);
    if let Some(task) = stdout_task {
        let _ = tokio::time::timeout(KILL_GRACE, task).await;
    }
    let stderr = match stderr_task {
        Some(task) => tokio::time::timeout(KILL_GRACE, task).await.ok().and_then(Result::ok).unwrap_or_default(),
        None => String::new(),
    };
    Ok(Finished { exit_code, stderr })
}
