use crate::{PtyError, PtyResult};
use portable_pty::{ChildKiller, MasterPty};

#[cfg(unix)]
pub(crate) fn process_group(master: &dyn MasterPty) -> Option<i32> {
    master.process_group_leader()
}

#[cfg(not(unix))]
pub(crate) fn process_group(_master: &dyn MasterPty) -> Option<i32> {
    None
}

#[cfg(unix)]
pub(crate) fn send_signal(
    process_group: Option<i32>,
    signal: i32,
    fallback: &mut dyn ChildKiller,
) -> PtyResult<()> {
    if let Some(process_group) = process_group {
        // SAFETY: libc::kill does not dereference Rust pointers. The process group id and signal
        // are plain integers from portable-pty/libc constants, and the negative pid intentionally
        // targets the child process group.
        let result = unsafe { libc::kill(-process_group, signal) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(PtyError::from(error));
        }
    }
    fallback.kill().map_err(PtyError::from)
}

#[cfg(windows)]
pub(crate) fn send_signal(
    process_group: Option<i32>,
    _signal: i32,
    fallback: &mut dyn ChildKiller,
) -> PtyResult<()> {
    if let Some(pid) = process_group.filter(|&pid| pid > 0) {
        const PROCESS_TERMINATE: u32 = 0x0001;
        unsafe extern "system" {
            fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut std::ffi::c_void;
            fn TerminateProcess(process: *mut std::ffi::c_void, exit_code: u32) -> i32;
            fn CloseHandle(object: *mut std::ffi::c_void) -> i32;
        }
        // SAFETY: OpenProcess takes plain integers; a null handle is checked before any use.
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid as u32) };
        if !handle.is_null() {
            // SAFETY: `handle` is a live process handle opened with PROCESS_TERMINATE.
            let res = unsafe { TerminateProcess(handle, 1) };
            // SAFETY: `handle` came from OpenProcess and is closed exactly once.
            unsafe { CloseHandle(handle) };
            if res != 0 {
                return Ok(());
            }
        }
    }
    match fallback.kill() {
        Ok(()) => Ok(()),
        Err(err)
            if err.raw_os_error() == Some(5)
                || err.raw_os_error() == Some(6)
                || err.raw_os_error() == Some(2)
                || err.to_string().contains("Access is denied")
                || err.to_string().contains("No such process")
                || err.to_string().contains("os error 5")
                || err.to_string().contains("os error 6")
                || err.to_string().contains("os error 2") =>
        {
            Ok(())
        }
        Err(err) => Err(PtyError::from(err)),
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn send_signal(
    _process_group: Option<i32>,
    _signal: i32,
    fallback: &mut dyn ChildKiller,
) -> PtyResult<()> {
    fallback.kill().map_err(PtyError::from)
}

#[cfg(unix)]
pub(crate) fn signal_signal(signal: &str) -> PtyResult<i32> {
    match signal.to_ascii_uppercase().as_str() {
        "HUP" | "SIGHUP" => Ok(libc::SIGHUP),
        "INT" | "SIGINT" => Ok(libc::SIGINT),
        "TERM" | "SIGTERM" => Ok(libc::SIGTERM),
        "KILL" | "SIGKILL" => Ok(libc::SIGKILL),
        other => Err(PtyError::new(format!("unsupported signal: {other}"))),
    }
}

#[cfg(not(unix))]
pub(crate) fn signal_signal(_signal: &str) -> PtyResult<i32> {
    Ok(0)
}

#[cfg(unix)]
pub(crate) fn kill_signal() -> i32 {
    libc::SIGKILL
}

#[cfg(not(unix))]
pub(crate) fn kill_signal() -> i32 {
    0
}

/// Whether `pid` names a live process. Access denied counts as alive: the process exists but
/// belongs to someone else.
#[cfg(windows)]
pub fn process_alive(pid: u32) -> bool {
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_ACCESS_DENIED: u32 = 5;
    unsafe extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut std::ffi::c_void;
        fn GetExitCodeProcess(process: *mut std::ffi::c_void, exit_code: *mut u32) -> i32;
        fn CloseHandle(object: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }
    if pid == 0 {
        return false;
    }
    // SAFETY: OpenProcess takes plain integers; a null handle is checked before any use.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // SAFETY: reads the calling thread's last-error value; no pointers involved.
        return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
    }
    let mut exit_code = 0u32;
    // SAFETY: `handle` is a live process handle and `exit_code` outlives the call.
    let queried = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    // SAFETY: `handle` came from OpenProcess and is closed exactly once.
    unsafe { CloseHandle(handle) };
    queried != 0 && exit_code == STILL_ACTIVE
}
