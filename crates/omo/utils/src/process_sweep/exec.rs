use std::process::Command;

use super::process_table::{ProcessInfo, parse_posix_process_table, parse_windows_process_table};

/// Signals sweep targets; the sweeper calls `terminate`, waits, then `kill`s survivors.
pub trait ProcessKiller {
    fn is_alive(&self, pid: u32) -> bool;
    fn kill(&self, pid: u32) -> Result<(), String>;
    fn terminate(&self, pid: u32) -> Result<(), String>;
}

pub type CodegraphProcessKiller = dyn ProcessKiller;

pub fn enumerate_processes(platform: &str) -> Result<Vec<ProcessInfo>, String> {
    if platform == "win32" {
        let script = [
            "Get-CimInstance Win32_Process",
            "Select-Object ProcessId,ParentProcessId,CommandLine",
            "ConvertTo-Json -Compress -Depth 2",
        ]
        .join(" | ");
        return run_for_stdout("powershell.exe", &["-NoProfile", "-Command", &script])
            .map(|stdout| parse_windows_process_table(&stdout));
    }
    run_for_stdout("ps", &["-eo", "pid=,ppid=,command="])
        .map(|stdout| parse_posix_process_table(&stdout))
}

pub fn enumerate_codegraph_processes(platform: &str) -> Result<Vec<ProcessInfo>, String> {
    enumerate_processes(platform)
}

pub fn create_default_process_killer(platform: &str) -> Box<dyn ProcessKiller + Send + Sync> {
    if platform == "win32" {
        Box::new(WindowsKiller)
    } else {
        Box::new(PosixKiller)
    }
}

pub fn create_default_codegraph_process_killer(
    platform: &str,
) -> Box<dyn ProcessKiller + Send + Sync> {
    create_default_process_killer(platform)
}

pub fn default_is_process_alive(pid: u32) -> bool {
    crate::process_tree::is_process_alive(pid)
}

struct PosixKiller;

impl ProcessKiller for PosixKiller {
    fn is_alive(&self, pid: u32) -> bool {
        default_is_process_alive(pid)
    }

    fn kill(&self, pid: u32) -> Result<(), String> {
        signal_pid(pid, Signal::Kill)
    }

    fn terminate(&self, pid: u32) -> Result<(), String> {
        signal_pid(pid, Signal::Term)
    }
}

struct WindowsKiller;

impl ProcessKiller for WindowsKiller {
    fn is_alive(&self, pid: u32) -> bool {
        default_is_process_alive(pid)
    }

    fn kill(&self, pid: u32) -> Result<(), String> {
        run_for_stdout("taskkill.exe", &["/PID", &pid.to_string(), "/T", "/F"]).map(drop)
    }

    fn terminate(&self, pid: u32) -> Result<(), String> {
        run_for_stdout("taskkill.exe", &["/PID", &pid.to_string(), "/T"]).map(drop)
    }
}

#[derive(Clone, Copy)]
enum Signal {
    Kill,
    Term,
}

#[cfg(unix)]
fn signal_pid(pid: u32, signal: Signal) -> Result<(), String> {
    let raw = match signal {
        Signal::Kill => libc::SIGKILL,
        Signal::Term => libc::SIGTERM,
    };
    let target = libc::pid_t::try_from(pid).map_err(|_| format!("pid {pid} out of range"))?;
    // SAFETY: kill(2) only reads its integer arguments.
    if unsafe { libc::kill(target, raw) } == 0 {
        return Ok(());
    }
    Err(std::io::Error::last_os_error().to_string())
}

#[cfg(not(unix))]
fn signal_pid(pid: u32, signal: Signal) -> Result<(), String> {
    let mut args = vec!["/PID".to_string(), pid.to_string(), "/T".to_string()];
    if matches!(signal, Signal::Kill) {
        args.push("/F".to_string());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_for_stdout("taskkill.exe", &refs).map(drop)
}

fn run_for_stdout(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{program} exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
