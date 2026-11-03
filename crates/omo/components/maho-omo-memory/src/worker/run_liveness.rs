use memory_core::locks::{
    ProcessLiveness, get_pid_liveness, get_process_start_identity, start_identities_comparable,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunProcessVerdict {
    Alive,
    Dead,
    Unknown,
    Absent,
}

/// A live pid is dead only when its recorded start identity provably belongs to another process.
/// Two identities prove reuse only inside one scheme: a ledger written as `ps-lstart:` by a
/// supervisor and read back as `proc-start-epoch:` by this runtime describes the same instant in
/// different words, and treating that as reuse tore live worktrees down mid-run (#8304).
fn recorded_start_was_reused(recorded: &str, actual: &str) -> bool {
    start_identities_comparable(recorded, actual) && recorded != actual
}

pub fn classify_run_process_with(
    pid: Option<u32>,
    recorded_start: Option<Option<&str>>,
    liveness: impl FnOnce(u32) -> ProcessLiveness,
    process_start: impl FnOnce(u32) -> Option<String>,
) -> RunProcessVerdict {
    let Some(pid) = pid else {
        return RunProcessVerdict::Absent;
    };
    let Some(recorded_start) = recorded_start else {
        return RunProcessVerdict::Unknown;
    };
    match liveness(pid) {
        ProcessLiveness::Dead => RunProcessVerdict::Dead,
        ProcessLiveness::Unknown => RunProcessVerdict::Unknown,
        ProcessLiveness::Alive => match (recorded_start, process_start(pid)) {
            (Some(recorded), Some(actual)) if recorded_start_was_reused(recorded, &actual) => {
                RunProcessVerdict::Dead
            }
            (Some(_), Some(_)) => RunProcessVerdict::Alive,
            _ => RunProcessVerdict::Unknown,
        },
    }
}

pub fn classify_run_process(
    pid: Option<u32>,
    recorded_start: Option<Option<&str>>,
) -> RunProcessVerdict {
    classify_run_process_with(
        pid,
        recorded_start,
        get_pid_liveness,
        get_process_start_identity,
    )
}

/// Launcher liveness: dead on ESRCH, or on a live pid whose recorded start identity proves reuse.
pub fn is_launcher_dead_with(
    pid: u32,
    recorded_start: Option<&str>,
    liveness: impl FnOnce(u32) -> ProcessLiveness,
    process_start: impl FnOnce(u32) -> Option<String>,
) -> bool {
    match liveness(pid) {
        ProcessLiveness::Dead => true,
        ProcessLiveness::Alive => recorded_start.is_some_and(|recorded| {
            process_start(pid).is_some_and(|actual| recorded_start_was_reused(recorded, &actual))
        }),
        ProcessLiveness::Unknown => false,
    }
}

/// The error an aborted observation reports (upstream rethrows `signal.reason`).
pub const OBSERVATION_ABORTED: &str = "The reflection run observation was aborted.";

/// `signal?.throwIfAborted()`: an aborted observation stops before any terminal claim.
pub fn throw_if_aborted(signal: Option<&maho_ext_api::AbortSignal>) -> Result<(), String> {
    if signal.is_some_and(maho_ext_api::AbortSignal::is_aborted) {
        Err(OBSERVATION_ABORTED.to_owned())
    } else {
        Ok(())
    }
}

pub async fn wait_until(deadline_at: f64, now: impl FnOnce() -> f64) {
    // Never aborted: the `signal` variant below is the cancellable one.
    let _ = wait_until_signal(deadline_at, now, None).await;
}

/// `waitUntil(deadlineAt, now, signal)`: rejects as soon as the signal aborts, even mid-sleep.
pub async fn wait_until_signal(
    deadline_at: f64,
    now: impl FnOnce() -> f64,
    signal: Option<&maho_ext_api::AbortSignal>,
) -> Result<(), String> {
    throw_if_aborted(signal)?;
    let delay = (deadline_at - now()).max(0.0);
    if delay == 0.0 {
        return Ok(());
    }
    let sleep = tokio::time::sleep(std::time::Duration::from_secs_f64(delay / 1000.0));
    match signal {
        None => {
            sleep.await;
            Ok(())
        }
        Some(signal) => tokio::select! {
            () = sleep => Ok(()),
            () = signal.cancelled() => Err(OBSERVATION_ABORTED.to_owned()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn deadline_wait_uses_exact_remaining_budget() {
        let start = tokio::time::Instant::now();
        wait_until(1500.0, || 500.0).await;
        assert_eq!(start.elapsed(), std::time::Duration::from_millis(1000));
        let start = tokio::time::Instant::now();
        wait_until(500.0, || 1500.0).await;
        assert_eq!(start.elapsed(), std::time::Duration::ZERO);
    }
    #[test]
    fn absent_and_legacy_never_probe() {
        assert_eq!(
            classify_run_process_with(None, None, |_| panic!("probe"), |_| panic!("identity")),
            RunProcessVerdict::Absent
        );
        assert_eq!(
            classify_run_process_with(Some(42), None, |_| panic!("probe"), |_| panic!("identity")),
            RunProcessVerdict::Unknown
        );
    }
    #[test]
    fn dead_and_unknown_do_not_read_identity() {
        for (liveness, verdict) in [
            (ProcessLiveness::Dead, RunProcessVerdict::Dead),
            (ProcessLiveness::Unknown, RunProcessVerdict::Unknown),
        ] {
            assert_eq!(
                classify_run_process_with(
                    Some(42),
                    Some(Some("old")),
                    |_| liveness,
                    |_| panic!("identity")
                ),
                verdict
            );
        }
    }
    #[test]
    fn matching_recycled_and_unverifiable_processes() {
        for (recorded, actual, verdict) in [
            (Some("old"), Some("old"), RunProcessVerdict::Alive),
            (Some("old"), Some("new"), RunProcessVerdict::Dead),
            (None, Some("old"), RunProcessVerdict::Unknown),
            (Some("old"), None, RunProcessVerdict::Unknown),
        ] {
            assert_eq!(
                classify_run_process_with(
                    Some(42),
                    Some(recorded),
                    |_| ProcessLiveness::Alive,
                    |_| actual.map(str::to_owned)
                ),
                verdict
            );
        }
    }
    const PS_LSTART: &str = "ps-lstart:Tue Sep 15 14:59:03 2026";
    const EPOCH: &str = "proc-start-epoch:1789452000";
    const EPOCH_LATER: &str = "proc-start-epoch:1789455600";
    fn classify_alive(recorded: &str, actual: &str) -> RunProcessVerdict {
        classify_run_process_with(
            Some(222),
            Some(Some(recorded)),
            |_| ProcessLiveness::Alive,
            |_| Some(actual.to_owned()),
        )
    }
    #[test]
    fn a_cross_scheme_mismatch_is_not_pid_reuse_and_the_process_is_alive() {
        assert_eq!(classify_alive(PS_LSTART, EPOCH), RunProcessVerdict::Alive);
    }
    #[test]
    fn a_same_scheme_mismatch_is_pid_reuse_and_the_process_is_dead() {
        assert_eq!(classify_alive(EPOCH, EPOCH_LATER), RunProcessVerdict::Dead);
        assert_eq!(classify_alive(EPOCH, EPOCH), RunProcessVerdict::Alive);
    }
    #[test]
    fn an_unknown_pid_is_dead_regardless_of_the_recorded_scheme() {
        assert_eq!(
            classify_run_process_with(
                Some(222),
                Some(Some(PS_LSTART)),
                |_| ProcessLiveness::Dead,
                |_| panic!("identity")
            ),
            RunProcessVerdict::Dead
        );
    }
    #[test]
    fn scheme_less_legacy_identities_that_differ_still_mean_pid_reuse() {
        assert_eq!(
            classify_alive("supervisor-start", "different-start"),
            RunProcessVerdict::Dead
        );
    }
    #[test]
    fn a_live_launcher_read_back_in_another_scheme_is_not_dead() {
        assert!(!is_launcher_dead_with(
            111,
            Some(PS_LSTART),
            |_| ProcessLiveness::Alive,
            |_| Some(EPOCH.to_owned())
        ));
    }
    #[test]
    fn a_live_launcher_pid_reused_in_the_same_scheme_is_dead() {
        assert!(is_launcher_dead_with(
            111,
            Some(EPOCH),
            |_| ProcessLiveness::Alive,
            |_| Some(EPOCH_LATER.to_owned())
        ));
        assert!(is_launcher_dead_with(
            111,
            None,
            |_| ProcessLiveness::Dead,
            |_| panic!("identity")
        ));
        assert!(!is_launcher_dead_with(
            111,
            None,
            |_| ProcessLiveness::Alive,
            |_| panic!("identity")
        ));
        assert!(!is_launcher_dead_with(
            111,
            Some(EPOCH),
            |_| ProcessLiveness::Unknown,
            |_| panic!("identity")
        ));
    }
    #[test]
    fn current_process_matches_real_start_identity() {
        let pid = std::process::id();
        let start = get_process_start_identity(pid);
        assert_eq!(
            classify_run_process(Some(pid), Some(start.as_deref())),
            if start.is_some() {
                RunProcessVerdict::Alive
            } else {
                RunProcessVerdict::Unknown
            }
        );
    }
    #[tokio::test(start_paused = true)]
    async fn an_abort_rejects_a_pending_wait_and_a_pre_aborted_signal_never_waits() {
        let signal = maho_ext_api::AbortSignal::default();
        let aborter = signal.clone();
        let waiting =
            tokio::spawn(async move { wait_until_signal(60_000.0, || 0.0, Some(&signal)).await });
        tokio::task::yield_now().await;
        aborter.abort();
        assert_eq!(waiting.await.unwrap(), Err(OBSERVATION_ABORTED.to_owned()));
        assert_eq!(
            wait_until_signal(0.0, || 0.0, Some(&aborter)).await,
            Err(OBSERVATION_ABORTED.to_owned())
        );
        assert_eq!(wait_until_signal(0.0, || 0.0, None).await, Ok(()));
    }
}
