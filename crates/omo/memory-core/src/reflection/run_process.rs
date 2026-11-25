//! Process classification for recorded reflection run supervisors and children.

use crate::locks::process_identity::{
    ProcessLiveness, get_pid_liveness, get_process_start_identity, start_identities_conflict,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedProcessVerdict {
    Alive,
    Dead,
    Unknown,
    Absent,
}

#[derive(Default, Clone, Copy)]
pub struct RecordedProcessSeams<'a> {
    pub pid_liveness: Option<&'a dyn Fn(u32) -> ProcessLiveness>,
    pub process_start: Option<&'a dyn Fn(u32) -> Option<String>>,
}

fn start_verdict(
    pid: u32,
    recorded_start: Option<&str>,
    seams: &RecordedProcessSeams<'_>,
) -> RecordedProcessVerdict {
    let actual_start = match seams.process_start {
        Some(probe) => probe(pid),
        None => get_process_start_identity(pid),
    };
    let (Some(recorded), Some(actual)) = (recorded_start, actual_start) else {
        return RecordedProcessVerdict::Unknown;
    };
    if start_identities_conflict(recorded, &actual) {
        RecordedProcessVerdict::Dead
    } else {
        RecordedProcessVerdict::Alive
    }
}

/// Strict classification for a valid run ledger (#8304): `recorded_start: None` returns `Unknown`
/// before checking kernel liveness so queued/launching/deadline paths keep waiting.
///
/// Outer `Option` encodes whether the `processStart` / `childProcessStart` field was present in JSON:
/// `None` = property absent (`undefined`), `Some(None)` = explicitly `null`, `Some(Some(s))` = string.
pub fn classify_recorded_process(
    pid: Option<u32>,
    recorded_start: Option<Option<&str>>,
    seams: &RecordedProcessSeams<'_>,
) -> RecordedProcessVerdict {
    let Some(pid) = pid else {
        return RecordedProcessVerdict::Absent;
    };
    let Some(recorded_start) = recorded_start else {
        return RecordedProcessVerdict::Unknown;
    };
    let liveness = match seams.pid_liveness {
        Some(probe) => probe(pid),
        None => get_pid_liveness(pid),
    };
    match liveness {
        ProcessLiveness::Dead => RecordedProcessVerdict::Dead,
        ProcessLiveness::Unknown => RecordedProcessVerdict::Unknown,
        ProcessLiveness::Alive => start_verdict(pid, recorded_start, seams),
    }
}

/// Lenient classification for an UNPARSEABLE or stranded ledger only (#11920): checks kernel liveness
/// first so a dead PID is dead regardless of whether `processStart` was captured.
pub fn classify_recorded_process_lenient(
    pid: Option<u32>,
    recorded_start: Option<Option<&str>>,
    seams: &RecordedProcessSeams<'_>,
) -> RecordedProcessVerdict {
    let Some(pid) = pid else {
        return RecordedProcessVerdict::Absent;
    };
    let liveness = match seams.pid_liveness {
        Some(probe) => probe(pid),
        None => get_pid_liveness(pid),
    };
    match liveness {
        ProcessLiveness::Dead => RecordedProcessVerdict::Dead,
        ProcessLiveness::Unknown => RecordedProcessVerdict::Unknown,
        ProcessLiveness::Alive => match recorded_start {
            None => RecordedProcessVerdict::Unknown,
            Some(recorded) => start_verdict(pid, recorded, seams),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_vs_lenient_when_process_start_is_missing_for_dead_pid() {
        let seams = RecordedProcessSeams {
            pid_liveness: Some(&|_| ProcessLiveness::Dead),
            process_start: Some(&|_| None),
        };
        assert_eq!(
            classify_recorded_process(Some(42), None, &seams),
            RecordedProcessVerdict::Unknown
        );
        assert_eq!(
            classify_recorded_process_lenient(Some(42), None, &seams),
            RecordedProcessVerdict::Dead
        );
    }

    #[test]
    fn reused_pid_start_identity_is_dead() {
        let seams = RecordedProcessSeams {
            pid_liveness: Some(&|_| ProcessLiveness::Alive),
            process_start: Some(&|_| Some("linux-proc-start-ticks:200".to_owned())),
        };
        assert_eq!(
            classify_recorded_process(Some(42), Some(Some("linux-proc-start-ticks:100")), &seams),
            RecordedProcessVerdict::Dead
        );
    }
}
