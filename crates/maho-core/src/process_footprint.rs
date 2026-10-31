//! Port of senpi packages/coding-agent/src/core/process-footprint.ts.
//!
//! How much memory a process really holds, read from the kernel without spawning anything. RSS keeps
//! counting freed pages, so the footprint is reported: `RssAnon` from `/proc/<pid>/status` on Linux.
//! The darwin (`proc_pid_rusage`) and win32 (`K32GetProcessMemoryInfo`) readers need FFI, which this
//! crate denies (`unsafe_code = "deny"`); there `read_process_footprint` answers `None`, and the own
//! process falls back to `VmRSS` labelled `rss` where it is readable. Every read never fails.

/// Which kernel counter `bytes` came from; `Rss` means no footprint counter was readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessFootprintMeasure {
    PhysFootprint,
    RssAnon,
    PrivateUsage,
    Rss,
}

impl ProcessFootprintMeasure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PhysFootprint => "phys_footprint",
            Self::RssAnon => "rss_anon",
            Self::PrivateUsage => "private_usage",
            Self::Rss => "rss",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessFootprint {
    pub bytes: u64,
    pub measure: ProcessFootprintMeasure,
}

fn status_kib(status: &str, field: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let rest = line.strip_prefix(field)?.strip_prefix(':')?;
        let value = rest.trim().strip_suffix("kB")?.trim();
        value
            .chars()
            .all(|c| c.is_ascii_digit())
            .then(|| value.parse::<u64>().ok())
            .flatten()
    })
}

/// Anonymous resident memory in bytes from `/proc/<pid>/status` text, or `None` when absent.
pub fn parse_proc_status_rss_anon(status: &str) -> Option<u64> {
    status_kib(status, "RssAnon").map(|kib| kib * 1024)
}

fn read_status(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/status")).ok()
}

/// The memory footprint of `pid`, or `None` when it cannot be read (the process is gone, is not ours to
/// inspect, or this platform has no reader).
pub fn read_process_footprint(pid: u32) -> Option<ProcessFootprint> {
    if pid == 0 {
        return None;
    }
    if cfg!(target_os = "linux") {
        let bytes = parse_proc_status_rss_anon(&read_status(pid)?)?;
        return Some(ProcessFootprint {
            bytes,
            measure: ProcessFootprintMeasure::RssAnon,
        });
    }
    None
}

/// The memory footprint of this process: RSS (`VmRSS`) when no footprint counter is readable, and zero
/// RSS when not even that is (a platform without `/proc`).
pub fn read_own_footprint() -> ProcessFootprint {
    let pid = std::process::id();
    read_process_footprint(pid).unwrap_or_else(|| ProcessFootprint {
        bytes: read_status(pid)
            .and_then(|status| status_kib(&status, "VmRSS"))
            .map_or(0, |kib| kib * 1024),
        measure: ProcessFootprintMeasure::Rss,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rss_anon_in_kib() {
        assert_eq!(
            parse_proc_status_rss_anon("Name:\tx\nRssAnon:\t   12 kB\nVmRSS:\t900 kB\n"),
            Some(12 * 1024)
        );
        assert_eq!(parse_proc_status_rss_anon("VmRSS:\t900 kB\n"), None);
        assert_eq!(parse_proc_status_rss_anon("RssAnon:\t12 MB\n"), None);
    }

    #[test]
    fn reads_this_process_and_rejects_pid_zero() {
        assert_eq!(read_process_footprint(0), None);
        let own = read_own_footprint();
        if cfg!(target_os = "linux") {
            assert_eq!(own.measure, ProcessFootprintMeasure::RssAnon);
            assert!(own.bytes > 0);
        }
    }
}
