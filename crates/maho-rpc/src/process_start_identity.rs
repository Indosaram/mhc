//! A recorded process start compared on ONE clock per platform (senpi `utils/process-start-identity.ts`,
//! senpi#3816 review), so an NTP or VM wall-clock step never reads a live process as reused.
//!
//! - Linux: `/proc/<pid>/stat` start ticks on both sides. They count on the boot clock, which no wall-clock step
//!   moves, and reading them spawns nothing.
//! - Elsewhere: the platform's recorded start time on both sides (macOS stores it as the wall time at fork).

/// `/proc/<pid>/stat` field 22 (start time in clock ticks since boot), or `None` when unreadable or off Linux.
pub fn read_process_start_ticks(pid: u32) -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_start_ticks(&stat)
}

/// The command name (field 2) may contain spaces and parentheses; fields resume after its last `)`.
fn parse_start_ticks(stat: &str) -> Option<String> {
    let (_, rest) = stat.rsplit_once(") ")?;
    rest.split(' ')
        .nth(19)
        .filter(|ticks| !ticks.is_empty() && ticks.bytes().all(|byte| byte.is_ascii_digit()))
        .map(str::to_owned)
}

/// The supervisor start the host was told about: Linux boot-clock ticks, or the platform's start time elsewhere.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordedStart {
    pub ticks: Option<String>,
    pub time: Option<String>,
}

/// `Unknown` whenever either side is missing or unreadable: an unreadable identity is never evidence of reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartComparison {
    Same,
    Changed,
    Unknown,
}

/// senpi `compareRecordedStart`: Linux compares ticks; elsewhere the recorded start time. `observed` is the value
/// read now on the same clock (`read_process_start_ticks` on Linux).
pub fn compare_recorded_start(
    linux: bool,
    recorded: &RecordedStart,
    observed: Option<&str>,
) -> StartComparison {
    let recorded = if linux {
        recorded.ticks.as_deref()
    } else {
        recorded.time.as_deref()
    };
    match (recorded, observed) {
        (Some(recorded), Some(observed)) if recorded == observed => StartComparison::Same,
        (Some(_), Some(_)) => StartComparison::Changed,
        _ => StartComparison::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ticks_survive_a_command_name_with_spaces_and_parentheses() {
        let stat = "42 (a (b) c) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 777 19";
        assert_eq!(parse_start_ticks(stat).as_deref(), Some("777"));
        assert_eq!(parse_start_ticks("42 (x) S 1"), None);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_reads_its_own_ticks() {
        assert!(read_process_start_ticks(std::process::id()).is_some());
    }
    #[test]
    fn unreadable_or_missing_identity_is_unknown() {
        let recorded = |value: &str| RecordedStart {
            ticks: Some(value.into()),
            time: Some(value.into()),
        };
        assert_eq!(
            compare_recorded_start(true, &RecordedStart::default(), Some("1")),
            StartComparison::Unknown
        );
        assert_eq!(
            compare_recorded_start(true, &recorded("1"), None),
            StartComparison::Unknown
        );
        assert_eq!(
            compare_recorded_start(false, &recorded("1"), Some("1")),
            StartComparison::Same
        );
        assert_eq!(
            compare_recorded_start(true, &recorded("1"), Some("2")),
            StartComparison::Changed
        );
    }
}
