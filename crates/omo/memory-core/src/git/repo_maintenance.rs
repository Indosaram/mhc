use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::atomic::AtomicU64;

use std::io::Read;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitMaintenanceOptions {
    pub min_loose_objects: usize,
    pub timeout_ms: u64,
    pub executable: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitMaintenanceResult {
    Skipped { loose_objects: usize },
    IntervalSuppressed,
    Packed { loose_objects_before: usize, loose_objects_after: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitMaintenanceError {
    Aborted,
    Io(String),
    Command(String),
    TimedOut(String),
}

static CHILD_PID: AtomicU64 = AtomicU64::new(0);

impl std::fmt::Display for GitMaintenanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Aborted => write!(f, "The operation was aborted"),
            Self::Io(message) | Self::Command(message) | Self::TimedOut(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for GitMaintenanceError {}

pub fn run_memory_repo_maintenance(
    repo: &Path,
    options: &GitMaintenanceOptions,
    cancelled: &AtomicBool,
) -> Result<GitMaintenanceResult, GitMaintenanceError> {
    let loose_before = count_loose_objects(repo, options.timeout_ms, cancelled, options.executable.as_deref())?;
    if loose_before < options.min_loose_objects {
        return Ok(GitMaintenanceResult::Skipped { loose_objects: loose_before });
    }
    run_git(repo, &["maintenance", "run", "--task=loose-objects", "--quiet"], options.timeout_ms, cancelled, options.executable.as_deref())?;
    if cancelled.load(Ordering::Acquire) { return Err(GitMaintenanceError::Aborted); }
    run_git(repo, &["prune-packed", "--quiet"], options.timeout_ms, cancelled, options.executable.as_deref())?;
    let loose_after = count_loose_objects(repo, options.timeout_ms, cancelled, options.executable.as_deref())?;
    Ok(GitMaintenanceResult::Packed { loose_objects_before: loose_before, loose_objects_after: loose_after })
}

pub fn maintain_repository(
    repo: &Path,
    stamp_key: &str,
    now: u64,
    interval_ms: u64,
    options: &GitMaintenanceOptions,
    cancelled: &AtomicBool,
) -> Result<GitMaintenanceResult, GitMaintenanceError> {
    let previous = match run_git(repo, &["config", "--local", "--get", stamp_key], options.timeout_ms, cancelled, options.executable.as_deref()) {
        Ok(value) => value.trim().parse::<u64>().ok(),
        Err(GitMaintenanceError::Command(_)) => None,
        Err(error) => return Err(error),
    };
    if previous.is_some_and(|last| now.saturating_sub(last) < interval_ms) {
        return Ok(GitMaintenanceResult::IntervalSuppressed);
    }
    run_git(repo, &["config", "--local", stamp_key, &now.to_string()], options.timeout_ms, cancelled, options.executable.as_deref())?;
    run_memory_repo_maintenance(repo, options, cancelled)
}

fn count_loose_objects(repo: &Path, timeout_ms: u64, cancelled: &AtomicBool, executable: Option<&Path>) -> Result<usize, GitMaintenanceError> {
    let output = run_git(repo, &["count-objects", "-v"], timeout_ms, cancelled, executable)?;
    let count = output.lines().find_map(|line| line.strip_prefix("count: ").and_then(|value| value.parse::<usize>().ok())).unwrap_or(0);
    Ok(count)
}

fn run_git(repo: &Path, args: &[&str], timeout_ms: u64, cancelled: &AtomicBool, executable: Option<&Path>) -> Result<String, GitMaintenanceError> {
    if cancelled.load(Ordering::Acquire) { return Err(GitMaintenanceError::Aborted); }
    let mut child = Command::new(executable.unwrap_or_else(|| Path::new("git"))).args(args).current_dir(repo).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|error| GitMaintenanceError::Io(error.to_string()))?;
    CHILD_PID.store(u64::from(child.id()), Ordering::Release);
    run_child(&mut child, args, timeout_ms, cancelled)
}

#[cfg(test)]
pub fn child_pid_for_tests() -> u32 { CHILD_PID.load(Ordering::Acquire) as u32 }

pub fn run_child(child: &mut std::process::Child, args: &[&str], timeout_ms: u64, cancelled: &AtomicBool) -> Result<String, GitMaintenanceError> {
    run_child_with_start(child, args, timeout_ms, cancelled, || {})
}

pub fn run_child_with_start<F: FnOnce()>(child: &mut std::process::Child, args: &[&str], timeout_ms: u64, cancelled: &AtomicBool, on_start: F) -> Result<String, GitMaintenanceError> {
    let stdout = child.stdout.take().ok_or_else(|| GitMaintenanceError::Io("stdout pipe missing".into()))?;
    let stderr = child.stderr.take().ok_or_else(|| GitMaintenanceError::Io("stderr pipe missing".into()))?;
    let out_reader = std::thread::spawn(move || { let mut bytes=Vec::new(); let mut stream=stdout; stream.read_to_end(&mut bytes).map(|_|bytes).map_err(|error|error.to_string()) });
    let err_reader = std::thread::spawn(move || { let mut bytes=Vec::new(); let mut stream=stderr; stream.read_to_end(&mut bytes).map(|_|bytes).map_err(|error|error.to_string()) });
    on_start();
    let deadline = std::time::Instant::now().checked_add(std::time::Duration::from_millis(timeout_ms));
    let mut terminal_error = None;
    let status = loop {
        if cancelled.load(Ordering::Acquire) { terminal_error = Some(GitMaintenanceError::Aborted); break None; }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {},
            Err(error) => { terminal_error = Some(GitMaintenanceError::Io(error.to_string())); break None; }
        }
        if timeout_ms > 0 && deadline.is_none_or(|end| std::time::Instant::now() >= end) { terminal_error = Some(GitMaintenanceError::TimedOut(format!("git {} timed out", args.join(" ")))); break None; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let status = match status {
        Some(status) => status,
        None => {
            let kill_error = child.kill().err();
            let wait_error = child.wait().err();
            let out_error = out_reader.join().map_err(|_| "stdout reader panicked".to_owned()).and_then(|r| r).err();
            let err_error = err_reader.join().map_err(|_| "stderr reader panicked".to_owned()).and_then(|r| r).err();
            if let Some(error) = kill_error.or(wait_error) { return Err(GitMaintenanceError::Io(error.to_string())); }
            if let Some(error) = out_error.or(err_error) { return Err(GitMaintenanceError::Io(error)); }
            return Err(terminal_error.unwrap_or(GitMaintenanceError::Aborted));
        }
    };
    let out=out_reader.join().map_err(|_|GitMaintenanceError::Io("stdout reader panicked".into()))?.map_err(GitMaintenanceError::Io)?;
    let err=err_reader.join().map_err(|_|GitMaintenanceError::Io("stderr reader panicked".into()))?.map_err(GitMaintenanceError::Io)?;
    if !status.success() { return Err(GitMaintenanceError::Command(String::from_utf8_lossy(&err).trim().into())); }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

