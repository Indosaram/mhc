//! Recovery of a reflection reservation whose holder died after this session already reconciled
//! (`reflection-stale-active.ts`, #10377).
//!
//! Session-start reconciliation runs once per session, so a host that dies mid-publish later leaves
//! `runtime/reflection/active.lock` held and every later request only queues behind it. This module
//! decides, fail-closed, whether that holder is provably gone; only then is the existing
//! reconciliation asked to settle the run. Reconciliation stays the only code that settles a run.
//! When liveness cannot be proven either way, nothing is reclaimed and the stuck state is reported
//! with the next step instead, so the stop is never silent.

use std::path::Path;

pub use memory_core::reflection::stale_holder_proof::{
    ReflectionLockProbe as ActiveHolderSeams, ReflectionLockVerdict,
};

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
        "memory reflection is stuck on a stale lock: run {run_id} still holds it and {reason}, so new reflection requests only queue. Next step: run /doctor; if no reflection run is active, move {} aside (keep a backup) and start a new session.",
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
}

/// Settles a provably dead holder through reconciliation; reports any other non-live holder.
pub fn recover_stale_reflection_holder(input: StaleHolderRecoveryInput<'_>) -> StaleHolderRecovery {
    let verdict = classify_active_reflection_holder(input.reflection_dir, input.seams);
    match verdict {
        ActiveHolderVerdict::Dead { ref run_id } => {
            if input.should_reconcile.is_some_and(|should| !should(run_id)) {
                return StaleHolderRecovery {
                    notice: Some(stale_reflection_lock_notice(
                        input.reflection_dir,
                        run_id,
                        "recovering it did not release the lock",
                    )),
                    verdict,
                };
            }
            if let Err(error) = (input.reconcile)() {
                // The reconcile failed, so the lock is still held by a dead owner: report, never retry here.
                let after = classify_active_reflection_holder(input.reflection_dir, input.seams);
                return StaleHolderRecovery {
                    notice: Some(stale_reflection_lock_notice(
                        input.reflection_dir,
                        after.run_id().unwrap_or(run_id),
                        &format!("recovering it failed: {error}"),
                    )),
                    verdict: after,
                };
            }
            let after = classify_active_reflection_holder(input.reflection_dir, input.seams);
            match &after {
                ActiveHolderVerdict::Stuck { run_id, reason } => StaleHolderRecovery {
                    notice: Some(stale_reflection_lock_notice(
                        input.reflection_dir,
                        run_id,
                        reason,
                    )),
                    verdict: after,
                },
                ActiveHolderVerdict::Dead { run_id }
                    if run_id == verdict.run_id().unwrap_or_default() =>
                {
                    StaleHolderRecovery {
                        notice: Some(stale_reflection_lock_notice(
                            input.reflection_dir,
                            run_id,
                            "recovering it did not release the lock",
                        )),
                        verdict: after,
                    }
                }
                // Reconciled (or released by something else): the original verdict stands with no notice.
                _ => StaleHolderRecovery {
                    verdict,
                    notice: None,
                },
            }
        }
        ActiveHolderVerdict::Stuck {
            ref run_id,
            ref reason,
        } => StaleHolderRecovery {
            notice: Some(stale_reflection_lock_notice(
                input.reflection_dir,
                run_id,
                reason,
            )),
            verdict,
        },
        ActiveHolderVerdict::None | ActiveHolderVerdict::Live { .. } => StaleHolderRecovery {
            verdict,
            notice: None,
        },
    }
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
}
