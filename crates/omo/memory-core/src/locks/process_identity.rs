//! Operating system process liveness detection and start-time identity resolution.

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
    let remainder = stat.get(command_end + 2)?.trim();
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
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    {
        let _ = pid;
        None
    }
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
