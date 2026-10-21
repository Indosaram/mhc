//! Parent-liveness watchdog: an opt-in poll that reports when the watched parent process dies.
//!
//! The reference implementation arms a `setInterval` whose callback runs the liveness probe; this
//! port runs the same poll on a dedicated thread and cancels it by disconnecting the channel when
//! the handle is cleared.

use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Default parent poll interval: thirty seconds, matching the reference implementation.
pub const DEFAULT_PARENT_POLL_INTERVAL_MS: u64 = 30_000;

/// Injectable liveness probe; the default probes the operating system.
pub type AliveProbe = Arc<dyn Fn(u32) -> bool + Send + Sync>;

/// Whether a process id still exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLiveness {
    /// The process exists, or the probe could not prove otherwise.
    Alive,
    /// The operating system reported that no such process exists.
    Dead,
}

/// Opt-in watchdog configuration, mirroring the reference implementation's `ParentWatchdogConfig`.
#[derive(Clone, Default)]
pub struct ParentWatchdogConfig {
    /// Process to watch; defaults to the current process's parent.
    pub parent_pid: Option<u32>,
    /// Poll interval in milliseconds; `Some(0)` disables the watchdog. Defaults to
    /// [`DEFAULT_PARENT_POLL_INTERVAL_MS`].
    pub poll_interval_ms: Option<u64>,
    /// Replaces the operating-system probe.
    pub probe_alive: Option<AliveProbe>,
    /// Observes every completed probe on the polling thread, before a dead parent is handled.
    /// Defaults to absent, so healthy polls emit nothing unless explicitly observed.
    pub on_poll: Option<Arc<dyn Fn(bool) + Send + Sync>>,
}

/// A running watchdog; call [`ParentWatchdogHandle::clear`] to stop polling.
pub struct ParentWatchdogHandle {
    poll: Option<IntervalThread>,
}

impl ParentWatchdogHandle {
    /// Stops the poll and joins its thread. Idempotent; a disabled watchdog has nothing to stop.
    pub fn clear(&mut self) {
        if let Some(mut poll) = self.poll.take() {
            poll.stop();
        }
    }
}

/// Starts the watchdog when configured; `None` or a zero poll interval creates no timer at all.
///
/// `on_dead_parent` runs at most once, on the polling thread, with the watched pid and the poll
/// interval.
pub fn create_parent_watchdog<F>(
    config: Option<ParentWatchdogConfig>,
    on_dead_parent: F,
) -> ParentWatchdogHandle
where
    F: Fn(u32, u64) + Send + 'static,
{
    let Some(config) = config else {
        return ParentWatchdogHandle { poll: None };
    };
    let poll_interval_ms = config
        .poll_interval_ms
        .unwrap_or(DEFAULT_PARENT_POLL_INTERVAL_MS);
    if poll_interval_ms == 0 {
        return ParentWatchdogHandle { poll: None };
    }
    let parent_pid = config.parent_pid.unwrap_or_else(parent_process_id);
    let probe_alive: AliveProbe = match config.probe_alive {
        Some(probe) => probe,
        None => Arc::new(is_process_alive),
    };
    let mut fired = false;
    let poll = IntervalThread::spawn(poll_interval_ms, move || {
        if fired {
            return true;
        }
        let alive = probe_alive(parent_pid);
        if let Some(on_poll) = &config.on_poll {
            on_poll(alive);
        }
        if alive {
            return false;
        }
        fired = true;
        on_dead_parent(parent_pid, poll_interval_ms);
        true
    });
    ParentWatchdogHandle { poll: Some(poll) }
}

/// Probes a pid for liveness by sending it signal 0.
pub fn probe_process(pid: u32) -> ProcessLiveness {
    match platform::probe(pid) {
        None => ProcessLiveness::Alive,
        Some(errno) => classify_probe_error(Some(errno)),
    }
}

/// Classifies a failed liveness probe: only `ESRCH` means dead.
///
/// A liveness probe must never end a healthy server, so every other errno (`EPERM`, or a
/// platform-specific code) is conservatively reported alive; a false positive costs one more poll.
pub fn classify_probe_error(errno: Option<i32>) -> ProcessLiveness {
    match errno {
        Some(error) if error == platform::ESRCH => ProcessLiveness::Dead,
        _ => ProcessLiveness::Alive,
    }
}

/// Whether a process id still exists; never panics and never propagates a probe failure.
pub fn is_process_alive(pid: u32) -> bool {
    probe_process(pid) == ProcessLiveness::Alive
}

/// The id of the process that started the current process.
pub fn parent_process_id() -> u32 {
    platform::parent_process_id()
}

/// A periodic poll; dropping the keep-alive sender disconnects the channel, which wakes
/// `recv_timeout` and ends the thread.
struct IntervalThread {
    keep_alive: Option<Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl IntervalThread {
    fn spawn<F>(interval_ms: u64, mut on_tick: F) -> Self
    where
        F: FnMut() -> bool + Send + 'static,
    {
        let (keep_alive, receiver) = std::sync::mpsc::channel::<()>();
        let interval = Duration::from_millis(interval_ms);
        let handle = std::thread::spawn(move || {
            loop {
                match receiver.recv_timeout(interval) {
                    Err(RecvTimeoutError::Timeout) => {
                        if on_tick() {
                            return;
                        }
                    }
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
        Self {
            keep_alive: Some(keep_alive),
            handle: Some(handle),
        }
    }

    fn stop(&mut self) {
        self.keep_alive = None;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}


#[cfg(unix)]
mod platform {
    /// POSIX `ESRCH`; the same value on Linux, macOS, and the BSDs.
    pub const ESRCH: i32 = 3;

    // `pid_t` is `i32` on every unix target Rust supports.
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }

    /// Sends signal 0 to the pid; `Some(errno)` when the process cannot be signalled.
    pub fn probe(pid: u32) -> Option<i32> {
        // A pid beyond `i32::MAX` cannot name a process; report it as not existing.
        let Ok(pid) = i32::try_from(pid) else {
            return Some(ESRCH);
        };
        // SAFETY: signal 0 tests for the process's existence and has no other effect; the pid is
        // passed by value and no pointer or buffer crosses the FFI boundary.
        let result = unsafe { kill(pid, 0) };
        if result == 0 {
            return None;
        }
        std::io::Error::last_os_error().raw_os_error()
    }

    pub fn parent_process_id() -> u32 {
        std::os::unix::process::parent_id()
    }
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, INVALID_HANDLE_VALUE, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// The errno Node's win32 `process.kill(pid, 0)` reports for an exited or unknown pid.
    pub const ESRCH: i32 = 3;

    /// libuv's win32 signal-0 probe: `OpenProcess`, then `GetExitCodeProcess`; a pid naming no
    /// process or an exited process is `ESRCH`, any other open failure is reported as its code.
    pub fn probe(pid: u32) -> Option<i32> {
        // SAFETY: OpenProcess takes plain integers; a null handle is checked before any use.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            // SAFETY: GetLastError only reads the calling thread's last-error value.
            let error = unsafe { GetLastError() };
            return Some(if error == ERROR_INVALID_PARAMETER { ESRCH } else { i32::try_from(error).unwrap_or(i32::MAX) });
        }
        let mut exit_code = 0u32;
        // SAFETY: `handle` is a live process handle and `exit_code` outlives the call.
        let queried = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
        // SAFETY: `handle` came from OpenProcess and is closed exactly once.
        unsafe { CloseHandle(handle) };
        if queried != 0 && exit_code != STILL_ACTIVE as u32 {
            return Some(ESRCH);
        }
        None
    }

    /// Finds the current process's parent in a ToolHelp process snapshot; 0 when unavailable.
    pub fn parent_process_id() -> u32 {
        // SAFETY: GetCurrentProcessId has no preconditions.
        let current = unsafe { GetCurrentProcessId() };
        // SAFETY: a process snapshot takes plain integers; the handle is checked before use.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return 0;
        }
        // SAFETY: PROCESSENTRY32W is plain data; all-zero is a valid value before dwSize is set.
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).unwrap_or(0);
        let mut parent = 0;
        // SAFETY: `snapshot` is a live snapshot handle and `entry` is sized via dwSize.
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while more {
            if entry.th32ProcessID == current {
                parent = entry.th32ParentProcessID;
                break;
            }
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        // SAFETY: `snapshot` came from CreateToolhelp32Snapshot and is closed exactly once.
        unsafe { CloseHandle(snapshot) };
        parent
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    pub const ESRCH: i32 = 3;

    /// Outside unix there is no portable probe, so the watchdog never reports a parent exit.
    pub fn probe(_pid: u32) -> Option<i32> {
        None
    }

    pub fn parent_process_id() -> u32 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn every_poll_is_reported_before_the_dead_parent_callback() {
        // Given three healthy probes followed by a dead parent, with an observer subscribed.
        let probes = AtomicUsize::new(0);
        let (sender, events) = std::sync::mpsc::channel();
        let polls = sender.clone();
        let config = ParentWatchdogConfig {
            parent_pid: Some(424_242),
            poll_interval_ms: Some(1),
            probe_alive: Some(Arc::new(move |pid| {
                assert_eq!(pid, 424_242);
                probes.fetch_add(1, Ordering::SeqCst) < 3
            })),
            on_poll: Some(Arc::new(move |alive| {
                polls.send(Some(alive)).expect("poll receiver remains open");
            })),
        };

        // When the watchdog polls until the parent dies.
        let mut watchdog = create_parent_watchdog(Some(config), move |pid, interval| {
            assert_eq!((pid, interval), (424_242, 1));
            sender.send(None).expect("event receiver remains open");
        });

        // Then every result arrives in order, including false before the single exit callback.
        for expected in [Some(true), Some(true), Some(true), Some(false), None] {
            assert_eq!(
                events.recv_timeout(Duration::from_secs(5)).expect("watchdog event"),
                expected
            );
        }
        watchdog.clear();
        assert!(events.try_recv().is_err(), "polling stops after parent death");
    }

    #[test]
    fn disabled_watchdog_never_probes_or_reports_a_poll() {
        // Given a zero interval, neither callback may run.
        let config = ParentWatchdogConfig {
            poll_interval_ms: Some(0),
            probe_alive: Some(Arc::new(|_| panic!("disabled watchdog probed"))),
            on_poll: Some(Arc::new(|_| panic!("disabled watchdog reported a poll"))),
            ..ParentWatchdogConfig::default()
        };

        // When the watchdog is created and cleared.
        let mut watchdog = create_parent_watchdog(Some(config), |_, _| {
            panic!("disabled watchdog reported parent death");
        });
        watchdog.clear();

        // Then no polling thread was armed.
        assert!(watchdog.poll.is_none());
    }
}
