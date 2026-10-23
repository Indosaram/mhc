//! Start-time evidence for a generation-bound host stop (senpi `host-stop-start-time.ts`).
use std::{fs, path::Path};

/// A stop is an operator action: the budget a loaded Windows host needs for the start-time probe.
pub const STOP_START_TIME_BUDGET_MS: u64 = 10_000;

/// Whether this platform can record a process start time at registration. On Windows the CIM probe often
/// outlives its budget while a host starts, so hosts record `processStartTime: null` and a generation-bound
/// stop cannot compare start times.
pub fn process_start_time_is_unreadable() -> bool {
    cfg!(windows)
}

/// The live start time of `pid` now, or `None` when the process is gone or its start time cannot be read.
pub fn read_live_start_time(pid: u32) -> Option<String> {
    crate::host_reservations::read_process_start_time(pid)
}

/// A recorded start time as milliseconds since the epoch. Linux records clock ticks since boot (USER_HZ is 100
/// on every Linux ABI), darwin records `<sec>.<usec>`; anything else is not comparable.
pub fn process_start_time_ms(recorded: &str) -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let ticks = recorded.trim().parse::<u64>().ok()?;
        let boot = fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|line| {
                line.strip_prefix("btime ")
                    .and_then(|value| value.trim().parse::<u64>().ok())
            })?;
        Some(boot as f64 * 1000. + ticks as f64 * 10.)
    }
    #[cfg(not(target_os = "linux"))]
    {
        recorded
            .trim()
            .parse::<f64>()
            .ok()
            .map(|seconds| seconds * 1000.)
    }
}

/// Proves, without a recorded start time, that `pid` is still the process that wrote `registration_file`: that
/// process started before it wrote the file, so a live start time LATER than the file's mtime is a reused pid.
/// Gone, unreadable or unparseable is not proven.
pub fn started_before_registration(pid: u32, registration_file: &Path) -> bool {
    let Some(started_ms) = read_live_start_time(pid).and_then(|live| process_start_time_ms(&live))
    else {
        return false;
    };
    let Some(written_ms) = fs::metadata(registration_file)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs_f64() * 1000.)
    else {
        return false;
    };
    started_ms <= written_ms
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn own_process_started_before_a_file_written_now_but_not_before_an_ancient_one() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("host.pid");
        fs::write(&file, "{}").unwrap();
        assert!(started_before_registration(std::process::id(), &file));
        assert!(!started_before_registration(u32::MAX, &file));
        assert!(!started_before_registration(
            std::process::id(),
            &temp.path().join("missing")
        ));
    }
    #[test]
    fn unparseable_start_time_is_not_comparable() {
        assert!(process_start_time_ms("not-a-number").is_none());
    }
}
