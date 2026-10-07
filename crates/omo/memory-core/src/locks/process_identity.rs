//! Operating system process liveness detection and start-time identity resolution.
//!
//! The pin (`locks/process-start-time.ts`) replaces the macOS `/bin/ps` fork with an in-process
//! `libproc` `proc_pidinfo` call and the Windows `powershell.exe` probe with `kernel32`
//! `GetProcessTimes`, because a fork per lock check filled the process table on a long-lived shared
//! host. Both replacements are FFI (`bun:ffi` `dlopen` upstream); Rust has no safe binding for
//! either in this workspace and this crate forbids `unsafe_code`, so those two platforms keep their
//! pre-existing behavior and the fork-free port is a recorded platform blocker, not a silent
//! degradation. Linux's `/proc` read is already in process and needs no FFI.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Process liveness state determined by OS-level probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessLiveness {
    Alive,
    Dead,
    Unknown,
}

fn run_command_with_timeout(mut command: Command, timeout: Duration) -> Option<Output> {
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command.spawn().ok()?;
    let start = Instant::now();
    let poll_interval = Duration::from_millis(5);

    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(poll_interval);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn read_linux_start_identity(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let command_end = stat.rfind(')')?;
    let remainder = stat.get(command_end + 2..)?.trim();
    let fields: Vec<&str> = remainder.split_whitespace().collect();
    let start_ticks = fields.get(19)?;
    Some(format!("linux-proc-start-ticks:{start_ticks}"))
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn read_bsd_start_identity(pid: u32) -> Option<String> {
    let mut cmd = Command::new("/bin/ps");
    cmd.args(["-o", "lstart=", "-p", &pid.to_string()]);
    let output = run_command_with_timeout(cmd, Duration::from_secs(2))?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(format!("ps-lstart:{collapsed}"))
}

/// Resolves an OS-level start timestamp token for distinguishing recycled PIDs.
pub fn get_process_start_identity(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        read_linux_start_identity(pid)
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    {
        read_bsd_start_identity(pid)
    }
    #[cfg(target_os = "windows")]
    {
        read_windows_start_identity(pid)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "windows"
    )))]
    {
        let _ = pid;
        None
    }
}

/// Windows keeps its pre-existing behavior: the pinned fork-free replacement is `kernel32`
/// `GetProcessTimes` behind `bun:ffi` `dlopen`, and neither this crate nor the workspace exposes a
/// safe binding for it. Identity stays unavailable, which `locks/acquire.rs` already handles by
/// comparing liveness alone (conservative: it never reclaims a live owner's lock).
#[cfg(target_os = "windows")]
fn read_windows_start_identity(pid: u32) -> Option<String> {
    let _ = pid;
    None
}

fn identity_scheme(identity: &str) -> Option<&str> {
    match identity.find(':') {
        Some(0) | None => None,
        Some(index) => Some(&identity[..index]),
    }
}

pub fn start_identities_conflict(recorded: &str, actual: &str) -> bool {
    let Some(recorded_scheme) = identity_scheme(recorded) else {
        return false;
    };
    if Some(recorded_scheme) != identity_scheme(actual) {
        return false;
    }
    recorded != actual
}

pub fn start_identities_comparable(recorded: &str, actual: &str) -> bool {
    identity_scheme(recorded) == identity_scheme(actual)
}

/// Probes whether a process with the given PID is currently alive, dead, or unknown.
pub fn get_pid_liveness(pid: u32) -> ProcessLiveness {
    if pid == 0 {
        return ProcessLiveness::Unknown;
    }

    #[cfg(unix)]
    {
        let prog = if Path::new("/bin/kill").exists() {
            "/bin/kill"
        } else {
            "kill"
        };
        let mut cmd = Command::new(prog);
        cmd.args(["-0", &pid.to_string()]);
        let output = match run_command_with_timeout(cmd, Duration::from_secs(2)) {
            Some(out) => out,
            None => return ProcessLiveness::Unknown,
        };
        if output.status.success() {
            return ProcessLiveness::Alive;
        }
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        if stderr.contains("no such process") || stderr.contains("esrch") {
            ProcessLiveness::Dead
        } else if stderr.contains("operation not permitted") || stderr.contains("eperm") {
            ProcessLiveness::Alive
        } else {
            ProcessLiveness::Unknown
        }
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("tasklist");
        cmd.args(["/FI", &format!("PID eq {pid}")]);
        let output = match run_command_with_timeout(cmd, Duration::from_secs(2)) {
            Some(out) => out,
            None => return ProcessLiveness::Unknown,
        };
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.contains(&pid.to_string()) {
                ProcessLiveness::Alive
            } else if stdout.contains("No tasks are running") || stdout.contains("INFO:") {
                ProcessLiveness::Dead
            } else {
                ProcessLiveness::Unknown
            }
        } else {
            ProcessLiveness::Unknown
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        ProcessLiveness::Unknown
    }
}

#[cfg(test)]
#[path = "process_identity_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "process_identity_conflict_tests.rs"]
mod conflict_tests;
