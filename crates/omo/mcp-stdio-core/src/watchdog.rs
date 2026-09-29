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
        if probe_alive(parent_pid) {
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

#[cfg(not(unix))]
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
