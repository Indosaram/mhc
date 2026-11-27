//! Recovery of a reflection reservation whose holder died after this session already reconciled
//! (`reflection-stale-active.ts`, #10377, #11976, #12517, c9a9370877).
//!
//! Session-start reconciliation runs once per session, so a host that dies mid-publish later leaves
//! `runtime/reflection/active.lock` held and every later request only queues behind it. The shared
//! reaper (`memory_core::reflection::stale_lock_reaper`, also run by `/doctor --fix`) decides,
//! fail-closed, whether that holder is provably gone; a dead holder and reapable stuck shapes
//! (launch interrupted, processes dead, unreadable ledger) are settled by reconciliation, a torn
//! lock is moved aside. Anything it cannot prove is reported with the next step, never silently
//! kept.

use std::path::Path;

use memory_core::locks::ProcessLiveness;
use memory_core::reflection::stale_holder::{LockReadError, ReflectionLockProbe};
use memory_core::reflection::stale_lock_reaper::{
    StaleLockDecision, StaleLockProbe, StaleLockReaperAction, StaleLockReaperOptions,
    run_stale_lock_reaper,
};

pub use memory_core::reflection::stale_holder_proof::{
    ReflectionLockProbe as ActiveHolderSeams, ReflectionLockVerdict,
};

struct ProbeAdapter<'a>(&'a ActiveHolderSeams<'a>);

impl ReflectionLockProbe for ProbeAdapter<'_> {
    fn read_text(&self, path: &Path) -> Result<String, LockReadError> {
        match self.0.read_text {
            Some(read) => read(path).map_err(LockReadError::new),
            None => {
                memory_core::reflection::stale_holder::SystemReflectionLockProbe.read_text(path)
            }
        }
    }

    fn hostname(&self) -> String {
        match self.0.hostname {
            Some(host) => host(),
            None => memory_core::reflection::stale_holder::SystemReflectionLockProbe.hostname(),
        }
    }

    fn pid_liveness(&self, pid: u32) -> ProcessLiveness {
        match self.0.pid_liveness {
            Some(check) => check(pid),
            None => {
                memory_core::reflection::stale_holder::SystemReflectionLockProbe.pid_liveness(pid)
            }
        }
    }

    fn process_start_identity(&self, pid: u32) -> Result<Option<String>, String> {
        match self.0.process_start {
            Some(start) => Ok(start(pid)),
            None => memory_core::reflection::stale_holder::SystemReflectionLockProbe
                .process_start_identity(pid),
        }
    }
}

impl StaleLockProbe for ProbeAdapter<'_> {}

/// The shared decision (`memory-core reflection/stale-holder.ts`); `omo doctor` runs the same code.
pub fn classify_active_reflection_holder(
    reflection_dir: &Path,
    seams: Option<&ActiveHolderSeams<'_>>,
) -> ActiveHolderVerdict {
    let owned = ActiveHolderSeams::default();
    memory_core::reflection::stale_holder_proof::classify_reflection_lock_holder(
        reflection_dir,
        seams.unwrap_or(&owned),
    )
}

pub type ActiveHolderVerdict = ReflectionLockVerdict;

pub fn stale_reflection_lock_notice(reflection_dir: &Path, run_id: &str, reason: &str) -> String {
    format!(
        "memory reflection is stuck on a stale lock: run {run_id} still holds it and {reason}, so new reflection requests only queue. Next step: run /doctor; if no reflection run is active, run `/doctor --fix` or move {} aside (keep a backup) and start a new session.",
        reflection_dir.join("active.lock").display()
    )
}

pub struct StaleHolderRecoveryInput<'a> {
    pub reflection_dir: &'a Path,
    pub reconcile: &'a dyn Fn() -> Result<(), String>,
    pub seams: Option<&'a ActiveHolderSeams<'a>>,
    /// False once this session already tried to settle the run: report it instead of reconciling again.
    pub should_reconcile: Option<&'a dyn Fn(&str) -> bool>,
}

pub struct StaleHolderRecovery {
    pub verdict: ActiveHolderVerdict,
    pub notice: Option<String>,
    pub recovered_run_id: Option<String>,
}

const NOT_RELEASED: &str = "recovering it did not release the lock";

fn to_holder_verdict(
    verdict: memory_core::reflection::stale_holder::ReflectionLockVerdict,
) -> ActiveHolderVerdict {
    match verdict {
        memory_core::reflection::stale_holder::ReflectionLockVerdict::None => {
            ActiveHolderVerdict::None
        }
        memory_core::reflection::stale_holder::ReflectionLockVerdict::Live { run_id } => {
            ActiveHolderVerdict::Live { run_id }
        }
        memory_core::reflection::stale_holder::ReflectionLockVerdict::Dead { run_id } => {
            ActiveHolderVerdict::Dead { run_id }
        }
        memory_core::reflection::stale_holder::ReflectionLockVerdict::Stuck { run_id, reason } => {
            ActiveHolderVerdict::Stuck { run_id, reason }
        }
    }
}

/// Settles or reaps a provably gone holder; reports any other non-live holder.
pub fn recover_stale_reflection_holder(input: StaleHolderRecoveryInput<'_>) -> StaleHolderRecovery {
    let current = classify_active_reflection_holder(input.reflection_dir, input.seams);
    if matches!(
        current,
        ActiveHolderVerdict::None | ActiveHolderVerdict::Live { .. }
    ) {
        return StaleHolderRecovery {
            verdict: current,
            notice: None,
            recovered_run_id: None,
        };
    }

    let runtime_dir = input
        .reflection_dir
        .parent()
        .unwrap_or(input.reflection_dir);
    let owned = ActiveHolderSeams::default();
    let probe = ProbeAdapter(input.seams.unwrap_or(&owned));
    let mut settle_err: Option<String> = None;
    let mut settle = || {
        if let Err(error) = (input.reconcile)() {
            settle_err = Some(error.clone());
            return Err(error);
        }
        Ok(())
    };

    let report = match run_stale_lock_reaper(StaleLockReaperOptions {
        runtime_dir,
        probe: Some(&probe),
        settle: Some(&mut settle),
        settle_always: false,
        should_settle: input.should_reconcile,
        dry_run: false,
        append_receipt: None,
    }) {
        Ok(report) => report,
        Err(error) => {
            let after = classify_active_reflection_holder(input.reflection_dir, input.seams);
            let run_id = after
                .run_id()
                .or_else(|| current.run_id())
                .unwrap_or("unknown");
            let reason = if let Some(err) = settle_err {
                format!("recovering it failed: {err}")
            } else {
                format!("recovering it failed: {error}")
            };
            return StaleHolderRecovery {
                notice: Some(stale_reflection_lock_notice(
                    input.reflection_dir,
                    run_id,
                    &reason,
                )),
                verdict: after,
                recovered_run_id: None,
            };
        }
    };

    let before = to_holder_verdict(report.before);
    let after = to_holder_verdict(report.after);

    let decision_run_id = match &report.decision {
        StaleLockDecision::None => {
            return StaleHolderRecovery {
                verdict: before,
                notice: None,
                recovered_run_id: None,
            };
        }
        StaleLockDecision::Finalize { run_id }
        | StaleLockDecision::Stuck { run_id, .. }
        | StaleLockDecision::Reap { run_id, .. } => run_id.clone(),
    };

    if report.action == StaleLockReaperAction::SettleDeclined {
        return StaleHolderRecovery {
            verdict: before,
            notice: Some(stale_reflection_lock_notice(
                input.reflection_dir,
                &decision_run_id,
                NOT_RELEASED,
            )),
            recovered_run_id: None,
        };
    }

    if matches!(
        report.action,
        StaleLockReaperAction::Settled | StaleLockReaperAction::Reaped
    ) {
        if let ActiveHolderVerdict::Stuck {
            ref run_id,
            ref reason,
        } = after
        {
            return StaleHolderRecovery {
                notice: Some(stale_reflection_lock_notice(
                    input.reflection_dir,
                    run_id,
                    reason,
                )),
                verdict: after,
                recovered_run_id: None,
            };
        }
        if let ActiveHolderVerdict::Dead { ref run_id } = after
            && run_id == &decision_run_id
        {
            return StaleHolderRecovery {
                notice: Some(stale_reflection_lock_notice(
                    input.reflection_dir,
                    run_id,
                    NOT_RELEASED,
                )),
                verdict: after,
                recovered_run_id: None,
            };
        }
        return StaleHolderRecovery {
            verdict: before,
            notice: None,
            recovered_run_id: Some(decision_run_id),
        };
    }

    let reason = match &report.decision {
        StaleLockDecision::Stuck { reason, .. } => reason.as_str(),
        _ => NOT_RELEASED,
    };
    StaleHolderRecovery {
        verdict: before,
        notice: Some(stale_reflection_lock_notice(
            input.reflection_dir,
            &decision_run_id,
            reason,
        )),
        recovered_run_id: None,
    }
}

/// The once-per-session reconcile: moves a torn `active.lock` aside first, then reconciles (`c9a9370877`).
///
/// If moving a torn lock aside fails, logs via `on_reaper_error` and still runs `reconcile` so once-per-session
/// reconciliation still runs even on sidecar/reaper failure.
pub fn reconcile_through_stale_lock_reaper(
    runtime_dir: &Path,
    reconcile: &dyn Fn() -> Result<(), String>,
    seams: Option<&ActiveHolderSeams<'_>>,
    on_reaper_error: &dyn Fn(&str),
) -> Result<(), String> {
    let owned = ActiveHolderSeams::default();
    let probe = ProbeAdapter(seams.unwrap_or(&owned));
    let mut settled = false;
    let mut settle = || {
        settled = true;
        reconcile()
    };
    match run_stale_lock_reaper(StaleLockReaperOptions {
        runtime_dir,
        probe: Some(&probe),
        settle: Some(&mut settle),
        settle_always: true,
        should_settle: None,
        dry_run: false,
        append_receipt: None,
    }) {
        Ok(_) => Ok(()),
        Err(error) => {
            if settled {
                return Err(error);
            }
            on_reaper_error(&error);
            reconcile()
        }
    }
}

/// Async variant for `after_bind`: moves any torn `active.lock` aside first, then awaits `reconcile` (`c9a9370877`).
/// If the reaper fails before `reconcile` runs, logs via `on_reaper_error` and still awaits `reconcile`.
pub async fn reconcile_through_stale_lock_reaper_async<F, Fut>(
    runtime_dir: &Path,
    reconcile: F,
    on_reaper_error: &(dyn Fn(&str) + Send + Sync),
) -> Result<(), String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    {
        use memory_core::reflection::stale_lock_reaper::{
            StaleLockReapAction, SystemStaleLockProbe, decide_stale_reflection_lock,
        };
        let reflection_dir = runtime_dir.join("reflection");
        let (_, decision) = decide_stale_reflection_lock(&reflection_dir, &SystemStaleLockProbe);
        if matches!(
            decision,
            StaleLockDecision::Reap {
                action: StaleLockReapAction::LockTorn,
                ..
            }
        ) && let Err(error) = run_stale_lock_reaper(StaleLockReaperOptions {
            runtime_dir,
            probe: None,
            settle: None,
            settle_always: false,
            should_settle: None,
            dry_run: false,
            append_receipt: None,
        }) {
            on_reaper_error(&error);
        }
    }
    reconcile().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::locks::ProcessLiveness;
    use std::path::PathBuf;

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const OUTCOME: &str = r#"{"version":1,"runId":"run-1","attempt":1,"finishedAt":"2026-01-01T00:00:00.000Z","childExit":{"code":0,"signal":null},"timedOut":false}"#;

    struct Fixture {
        _root: tempfile::TempDir,
        reflection: PathBuf,
    }

    fn fixture(launcher_pid: u32) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let reflection = root.path().join("reflection");
        write(
            &reflection,
            "active.lock",
            &serde_json::json!({
                "runId":"run-1","launcherPid":launcher_pid,
                "launcherHostname":memory_core::support::host::hostname(),
            })
            .to_string(),
        );
        Fixture {
            _root: root,
            reflection,
        }
    }

    fn seams<'a>(liveness: &'a dyn Fn(u32) -> ProcessLiveness) -> ActiveHolderSeams<'a> {
        ActiveHolderSeams {
            pid_liveness: Some(liveness),
            ..Default::default()
        }
    }

    #[test]
    fn a_provably_dead_holder_is_settled_through_reconciliation() {
        let fixture = fixture(4242);
        write(&fixture.reflection, "runs/run-1/outcome.json", OUTCOME);
        let dead = |_: u32| ProcessLiveness::Dead;
        let seams = seams(&dead);
        let reconciles = std::cell::Cell::new(0);
        let recovery = recover_stale_reflection_holder(StaleHolderRecoveryInput {
            reflection_dir: &fixture.reflection,
            reconcile: &|| {
                reconciles.set(reconciles.get() + 1);
                // Reconciliation is the only code that settles a run: it removes the lock.
                std::fs::remove_file(fixture.reflection.join("active.lock"))
                    .map_err(|error| error.to_string())
            },
            seams: Some(&seams),
            should_reconcile: None,
        });
        assert_eq!(reconciles.get(), 1);
        assert_eq!(
            recovery.verdict,
            ActiveHolderVerdict::Dead {
                run_id: "run-1".into()
            }
        );
        assert!(recovery.notice.is_none());
    }

    #[test]
    fn a_live_or_unknown_holder_is_never_reconciled() {
        for liveness in [ProcessLiveness::Alive, ProcessLiveness::Unknown] {
            let fixture = fixture(4242);
            write(&fixture.reflection, "runs/run-1/outcome.json", OUTCOME);
            let check = move |_: u32| liveness;
            let seams = seams(&check);
            let reconciles = std::cell::Cell::new(0);
            let recovery = recover_stale_reflection_holder(StaleHolderRecoveryInput {
                reflection_dir: &fixture.reflection,
                reconcile: &|| {
                    reconciles.set(reconciles.get() + 1);
                    Ok(())
                },
                seams: Some(&seams),
                should_reconcile: None,
            });
            assert_eq!(reconciles.get(), 0);
            assert!(matches!(
                recovery.verdict,
                ActiveHolderVerdict::Live { .. } | ActiveHolderVerdict::Stuck { .. }
            ));
        }
    }

    #[test]
    fn a_dead_holder_that_stays_held_is_reported_once_with_the_next_step() {
        let fixture = fixture(4242);
        write(&fixture.reflection, "runs/run-1/outcome.json", OUTCOME);
        let dead = |_: u32| ProcessLiveness::Dead;
        let seams = seams(&dead);
        let attempts = std::cell::Cell::new(0);
        let should = |_: &str| {
            attempts.set(attempts.get() + 1);
            false
        };
        let recovery = recover_stale_reflection_holder(StaleHolderRecoveryInput {
            reflection_dir: &fixture.reflection,
            reconcile: &|| panic!("a run this session already tried is not reconciled again"),
            seams: Some(&seams),
            should_reconcile: Some(&should),
        });
        assert_eq!(attempts.get(), 1);
        let notice = recovery.notice.expect("a stuck holder is reported");
        assert!(notice.contains("run run-1 still holds it"));
        assert!(notice.contains("recovering it did not release the lock"));
        assert!(notice.contains("Next step: run /doctor"));
        assert!(notice.contains("active.lock"));
    }

    #[test]
    fn an_unreadable_lock_is_reported_as_stuck_and_never_reconciled() {
        let fixture = fixture(4242);
        let denied = |_: &Path| Err("EACCES".to_string());
        let seams = ActiveHolderSeams {
            read_text: Some(&denied),
            ..Default::default()
        };
        let recovery = recover_stale_reflection_holder(StaleHolderRecoveryInput {
            reflection_dir: &fixture.reflection,
            reconcile: &|| panic!("an unreadable lock is not provably dead"),
            seams: Some(&seams),
            should_reconcile: None,
        });
        assert!(matches!(
            recovery.verdict,
            ActiveHolderVerdict::Stuck { .. }
        ));
        assert!(recovery.notice.is_some());
    }

    #[test]
    fn reconcile_through_stale_lock_reaper_reaps_torn_lock_and_runs_reconcile() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("runtime");
        let reflection = runtime.join("reflection");
        write(&reflection, "active.lock", "{not-json");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7_200);
        std::fs::OpenOptions::new()
            .write(true)
            .open(reflection.join("active.lock"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        let errors = std::cell::RefCell::new(Vec::<String>::new());
        let reconciled = std::cell::Cell::new(false);
        let result = reconcile_through_stale_lock_reaper(
            &runtime,
            &|| {
                reconciled.set(true);
                Ok(())
            },
            None,
            &|err| errors.borrow_mut().push(err.to_string()),
        );
        assert!(result.is_ok());
        assert!(reconciled.get());
        assert!(errors.borrow().is_empty());
        assert!(!reflection.join("active.lock").exists());
    }

    #[test]
    fn reconcile_through_stale_lock_reaper_still_reconciles_when_reaper_errors() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("runtime");
        let reflection = runtime.join("reflection");
        write(&reflection, "active.lock", "corrupt");
        let denied = |_: &Path| Err("EACCES".to_string());
        let seams = ActiveHolderSeams {
            read_text: Some(&denied),
            ..Default::default()
        };
        let reconciled = std::cell::Cell::new(false);
        let result = reconcile_through_stale_lock_reaper(
            &runtime,
            &|| {
                reconciled.set(true);
                Ok(())
            },
            Some(&seams),
            &|_| {},
        );
        assert!(result.is_ok());
        assert!(reconciled.get());
    }
}
