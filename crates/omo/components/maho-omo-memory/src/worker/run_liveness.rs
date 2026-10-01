use memory_core::locks::{ProcessLiveness, get_pid_liveness, get_process_start_identity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunProcessVerdict { Alive, Dead, Unknown, Absent }

pub fn classify_run_process_with(
    pid: Option<u32>,
    recorded_start: Option<Option<&str>>,
    liveness: impl FnOnce(u32) -> ProcessLiveness,
    process_start: impl FnOnce(u32) -> Option<String>,
) -> RunProcessVerdict {
    let Some(pid) = pid else { return RunProcessVerdict::Absent; };
    let Some(recorded_start) = recorded_start else { return RunProcessVerdict::Unknown; };
    match liveness(pid) {
        ProcessLiveness::Dead => RunProcessVerdict::Dead,
        ProcessLiveness::Unknown => RunProcessVerdict::Unknown,
        ProcessLiveness::Alive => match (recorded_start, process_start(pid)) {
            (Some(recorded), Some(actual)) if recorded == actual => RunProcessVerdict::Alive,
            (Some(_), Some(_)) => RunProcessVerdict::Dead,
            _ => RunProcessVerdict::Unknown,
        },
    }
}

pub fn classify_run_process(pid: Option<u32>, recorded_start: Option<Option<&str>>) -> RunProcessVerdict {
    classify_run_process_with(pid, recorded_start, get_pid_liveness, get_process_start_identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_and_legacy_never_probe() {
        assert_eq!(classify_run_process_with(None, None, |_| panic!("probe"), |_| panic!("identity")), RunProcessVerdict::Absent);
        assert_eq!(classify_run_process_with(Some(42), None, |_| panic!("probe"), |_| panic!("identity")), RunProcessVerdict::Unknown);
    }
    #[test]
    fn dead_and_unknown_do_not_read_identity() {
        for (liveness, verdict) in [(ProcessLiveness::Dead, RunProcessVerdict::Dead), (ProcessLiveness::Unknown, RunProcessVerdict::Unknown)] {
            assert_eq!(classify_run_process_with(Some(42), Some(Some("old")), |_| liveness, |_| panic!("identity")), verdict);
        }
    }
    #[test]
    fn matching_recycled_and_unverifiable_processes() {
        for (recorded, actual, verdict) in [(Some("old"), Some("old"), RunProcessVerdict::Alive), (Some("old"), Some("new"), RunProcessVerdict::Dead), (None, Some("old"), RunProcessVerdict::Unknown), (Some("old"), None, RunProcessVerdict::Unknown)] {
            assert_eq!(classify_run_process_with(Some(42), Some(recorded), |_| ProcessLiveness::Alive, |_| actual.map(str::to_owned)), verdict);
        }
    }
    #[test]
    fn current_process_matches_real_start_identity() {
        let pid = std::process::id();
        let start = get_process_start_identity(pid);
        assert_eq!(classify_run_process(Some(pid), Some(start.as_deref())), if start.is_some() { RunProcessVerdict::Alive } else { RunProcessVerdict::Unknown });
    }
}
