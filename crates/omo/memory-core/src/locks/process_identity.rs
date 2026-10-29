//! Operating system process liveness detection and start-time identity resolution.
//!
//! The pin (`locks/process-start-time.ts`) replaces the macOS `/bin/ps` fork with an in-process
//! `libproc` `proc_pidinfo` call and the Windows `powershell.exe` probe with `kernel32`
//! `GetProcessTimes`, because a fork per lock check filled the process table on a long-lived shared
//! host. Both replacements are FFI (`bun:ffi` `dlopen` upstream); Rust has no safe binding for
//! either in this workspace. This crate denies `unsafe_code` and opts in narrowly: unix liveness
//! uses `libc::kill(pid, 0)` and macOS start identity uses `proc_pidinfo`, each in a small
//! `#[allow(unsafe_code)]` helper; Windows start identity uses `GetProcessTimes` the same way.
//! Linux's `/proc` read is already in process and needs no FFI.

#[cfg(any(target_os = "macos", target_os = "freebsd", windows))]
use std::process::{Command, Output, Stdio};
#[cfg(any(target_os = "macos", target_os = "freebsd", windows))]
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

#[cfg(any(target_os = "macos", target_os = "freebsd", windows))]
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

/// macOS start identity via `proc_pidinfo(PROC_PIDTBSDINFO)`, formatted as the C-locale `ps -o lstart=`
/// text (`%a %b %e %H:%M:%S %Y`, local time, whitespace collapsed) so it stays byte-compatible with
/// `ps-lstart:` identities persisted by older builds.
// PERF-DIVERGENCE(locks/process-start-time.ts): upstream calls libproc proc_pidinfo via bun:ffi; the
// earlier port forked /bin/ps per probe; this port calls proc_pidinfo in-process. Output is identical.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn read_macos_libproc_start_identity(pid: u32) -> Option<String> {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // SAFETY: `info` is a zeroed plain-data struct and the buffer size passed matches its size.
    let start_sec = unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let written = libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast::<libc::c_void>(),
            size,
        );
        if written != size {
            return None;
        }
        info.pbi_start_tvsec
    };
    let secs = start_sec as libc::time_t;
    // SAFETY: `tm` is zeroed plain data; `localtime_r` only writes through the valid pointers given.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&secs, &mut tm).is_null() {
            return None;
        }
        tm
    };
    let day = DAYS.get(usize::try_from(tm.tm_wday).ok()?)?;
    let month = MONTHS.get(usize::try_from(tm.tm_mon).ok()?)?;
    // `ps` pads the day with a space (`%e`); the collapsed form is a plain number.
    Some(format!(
        "ps-lstart:{day} {month} {} {:02}:{:02}:{:02} {}",
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        i64::from(tm.tm_year) + 1900
    ))
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn read_bsd_start_identity(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        if let Some(identity) = read_macos_libproc_start_identity(pid) {
            return Some(identity);
        }
    }
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
///
/// The current process's own identity never changes, so it is probed once and cached (keyed by
/// pid, so a forked child with a new pid probes afresh). A failed probe is not cached.
pub fn get_process_start_identity(pid: u32) -> Option<String> {
    static OWN: std::sync::OnceLock<(u32, String)> = std::sync::OnceLock::new();
    if pid != std::process::id() {
        return probe_process_start_identity(pid);
    }
    if let Some((cached_pid, identity)) = OWN.get() {
        if *cached_pid == pid {
            return Some(identity.clone());
        }
        return probe_process_start_identity(pid);
    }
    let identity = probe_process_start_identity(pid)?;
    let _ = OWN.set((pid, identity.clone()));
    Some(identity)
}

fn probe_process_start_identity(pid: u32) -> Option<String> {
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

/// Windows start identity: the process creation `FILETIME` from `GetProcessTimes`, read in process
/// through a `PROCESS_QUERY_LIMITED_INFORMATION` handle (no child process is forked).
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
fn read_windows_start_identity(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: `OpenProcess` takes plain values; a null return means failure and is checked.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut creation, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: `handle` is a valid open process handle and the four out-pointers reference live
    // `FILETIME` locals; the handle is closed exactly once below.
    let ok = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    // SAFETY: `handle` was opened above and is not used after this call.
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return None;
    }
    let ticks = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    Some(format!("windows-process-creation-filetime:{ticks}"))
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

    // Linux: `/proc/<pid>` exists for live and zombie pids alike, matching `kill -0` (which also
    // reports Alive on EPERM), without forking a child per probe.
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new(&format!("/proc/{pid}")).exists() {
            ProcessLiveness::Alive
        } else {
            ProcessLiveness::Dead
        }
    }
    // PERF-DIVERGENCE(locks/process-start-time.ts): upstream probes liveness with `process.kill(pid, 0)`;
    // the earlier port forked `/bin/kill -0`; this port calls kill(2) in-process. Output is identical.
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        #[allow(unsafe_code)]
        fn kill_probe(pid: u32) -> ProcessLiveness {
            let Ok(raw) = libc::pid_t::try_from(pid) else {
                return ProcessLiveness::Unknown;
            };
            // SAFETY: signal 0 performs only an existence/permission check; no signal is delivered.
            if unsafe { libc::kill(raw, 0) } == 0 {
                return ProcessLiveness::Alive;
            }
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ESRCH) => ProcessLiveness::Dead,
                Some(libc::EPERM) => ProcessLiveness::Alive,
                _ => ProcessLiveness::Unknown,
            }
        }
        kill_probe(pid)
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
            } else if stdout.contains("No tasks are running") || stdout.contains("INFO:") || stdout.contains("정보:") {
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
