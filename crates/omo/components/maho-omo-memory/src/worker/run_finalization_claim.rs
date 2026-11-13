use memory_core::locks::{
    AcquireLockError, AcquireLockOptions, CreateLockRecordOptions, WithLockError,
    create_lock_record, run_finalization_lock_path, with_lock,
};
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub struct FinalizationTerminalResult {
    pub run_id: String,
    pub outcome: String,
}
#[derive(Debug, PartialEq, Eq)]
pub enum ClaimedRunResult<T> {
    Completed(T),
    Terminal(FinalizationTerminalResult),
    Busy,
}

fn read_terminal_run(
    run_dir: &Path,
    run_id: &str,
) -> Result<Option<FinalizationTerminalResult>, String> {
    for (file, abandoned) in [("final.json", false), ("abandoned.json", true)] {
        let path = run_dir.join(file);
        if !path.exists() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let outcome = value["outcome"].as_str();
        let valid = if abandoned {
            outcome == Some("abandoned_unknown")
        } else {
            outcome.is_some_and(|outcome| {
                [
                    "merged",
                    "no_changes",
                    "parent_dirty",
                    "dirty_uncommitted",
                    "merge_conflict",
                    "admin_tamper",
                    "timed_out",
                    "failed",
                ]
                .contains(&outcome)
            })
        };
        if value["runId"].as_str() != Some(run_id) || !valid {
            return Err(format!(
                "Invalid {} sentinel for {run_id}",
                if abandoned { "abandoned" } else { "final" }
            ));
        }
        return Ok(Some(FinalizationTerminalResult {
            run_id: run_id.into(),
            outcome: outcome.unwrap_or_default().into(),
        }));
    }
    Ok(None)
}

pub fn with_run_finalization_claim<T>(
    locks: &Path,
    run_dir: &Path,
    run_id: &str,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<ClaimedRunResult<T>, String> {
    with_run_finalization_claim_timeouts(
        locks,
        run_dir,
        run_id,
        FINALIZE_ATTEMPT_TIMEOUTS_MS,
        operation,
    )
}

fn with_run_finalization_claim_timeouts<T>(
    locks: &Path,
    run_dir: &Path,
    run_id: &str,
    attempt_timeouts_ms: &[u64],
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<ClaimedRunResult<T>, String> {
    let record = create_lock_record(
        "reflection-finalize",
        CreateLockRecordOptions {
            run_id: Some(run_id.into()),
        },
    )
    .map_err(|e| e.to_string())?;
    let path = run_finalization_lock_path(locks, run_id).map_err(|e| e.to_string())?;
    let mut operation = Some(operation);
    claim_with_attempts(attempt_timeouts_ms, |wait_timeout_ms| {
        let claimed = with_lock(
            &path,
            &record,
            &AcquireLockOptions {
                wait_timeout_ms: Some(wait_timeout_ms),
                ..Default::default()
            },
            || {
                let operation = operation
                    .take()
                    .ok_or_else(|| "Finalization operation ran more than once".to_owned())?;
                Ok::<_, String>(match read_terminal_run(run_dir, run_id)? {
                    Some(terminal) => ClaimedRunResult::Terminal(terminal),
                    None => ClaimedRunResult::Completed(operation()?),
                })
            },
        );
        match claimed {
            Ok(result) => Ok(result),
            Err(WithLockError::Acquire(AcquireLockError::Contention(_))) => {
                // The child has usually already exited and the holder is a short critical section,
                // so a contended attempt retries the acquire. Giving up on the first contention
                // strands the run and its single active slot until the next bind-time reconcile.
                Ok(match read_terminal_run(run_dir, run_id)? {
                    Some(terminal) => ClaimedRunResult::Terminal(terminal),
                    None => ClaimedRunResult::Busy,
                })
            }
            Err(error) => Err(error.to_string()),
        }
    })
}

const FINALIZE_ATTEMPT_TIMEOUTS_MS: &[u64] = &[2_000, 2_000, 5_000];

/// Runs `attempt` once per entry in `attempt_timeouts_ms`, passing that entry in as the bounded
/// lock wait. `Busy` is the only value that triggers a retry; anything else ends the loop.
fn claim_with_attempts<T>(
    attempt_timeouts_ms: &[u64],
    mut attempt: impl FnMut(u64) -> Result<ClaimedRunResult<T>, String>,
) -> Result<ClaimedRunResult<T>, String> {
    for wait_timeout_ms in attempt_timeouts_ms {
        match attempt(*wait_timeout_ms)? {
            ClaimedRunResult::Busy => continue,
            settled => return Ok(settled),
        }
    }
    Ok(ClaimedRunResult::Busy)
}

#[cfg(test)]
mod tests {
    use super::super::run_artifacts::write_run_json_atomic;
    use super::*;
    #[test]
    fn concurrent_finalizer_observes_terminal_without_reexecution() {
        let root = tempfile::tempdir().unwrap();
        let locks = root.path().join("locks");
        let run_dir = root.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (claimed_tx, claimed_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let operations = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let first_locks = &locks;
            let first_dir = &run_dir;
            let first_operations = &operations;
            let first = scope.spawn(move || {
                with_run_finalization_claim(first_locks, first_dir, "run", || {
                    first_operations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    claimed_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    write_run_json_atomic(
                        &first_dir.join("final.json"),
                        &serde_json::json!({"version":1,"runId":"run","outcome":"merged"}),
                        0o600,
                    )
                    .map_err(|e| e.to_string())?;
                    Ok("first")
                })
            });
            claimed_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let second = scope.spawn(|| {
                with_run_finalization_claim(&locks, &run_dir, "run", || {
                    operations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok("second")
                })
            });
            release_tx.send(()).unwrap();
            assert_eq!(
                first.join().unwrap().unwrap(),
                ClaimedRunResult::Completed("first")
            );
            assert_eq!(
                second.join().unwrap().unwrap(),
                ClaimedRunResult::Terminal(FinalizationTerminalResult {
                    run_id: "run".into(),
                    outcome: "merged".into()
                })
            );
        });
        assert_eq!(operations.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!run_finalization_lock_path(&locks, "run").unwrap().exists());
    }
    #[test]
    fn invalid_terminal_rejects_without_operation() {
        let root = tempfile::tempdir().unwrap();
        write_run_json_atomic(
            &root.path().join("final.json"),
            &serde_json::json!({"runId":"foreign","outcome":"merged"}),
            0o600,
        )
        .unwrap();
        assert!(
            with_run_finalization_claim(&root.path().join("locks"), root.path(), "run", || Ok::<
                _,
                String,
            >(
                ()
            ))
            .is_err()
        );
    }
    #[test]
    fn claim_retries_a_contended_attempt_until_it_settles() {
        let root = tempfile::tempdir().unwrap();
        let run_dir = root.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let locks = root.path().join("locks");
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let operations = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let holder = scope.spawn(|| {
                with_run_finalization_claim(&locks, &run_dir, "run", move || {
                    held_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .unwrap();
                    Ok("holder")
                })
            });
            held_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let contender = scope.spawn(|| {
                with_run_finalization_claim_timeouts(
                    &locks,
                    &run_dir,
                    "run",
                    &[40, 40, 5_000],
                    || {
                        operations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok("settled")
                    },
                )
            });
            // No sleep: the bounded attempt timeouts are the only wait. If the holder releases
            // before the contender starts, its first attempt simply succeeds.
            release_tx.send(()).unwrap();
            assert_eq!(
                holder.join().unwrap().unwrap(),
                ClaimedRunResult::Completed("holder")
            );
            assert_eq!(
                contender.join().unwrap().unwrap(),
                ClaimedRunResult::Completed("settled")
            );
        });
        assert_eq!(operations.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    #[test]
    fn claim_reports_busy_after_every_attempt_is_contended() {
        let mut attempts = 0;
        let result = claim_with_attempts(&[1, 1, 1], |_| {
            attempts += 1;
            Ok(ClaimedRunResult::<()>::Busy)
        })
        .unwrap();
        assert_eq!(result, ClaimedRunResult::Busy);
        assert_eq!(attempts, 3);
    }
    #[test]
    fn claim_runs_the_operation_once_and_never_retries_a_settled_attempt() {
        let mut attempts = 0;
        let result = claim_with_attempts(&[1, 1, 1], |_| {
            attempts += 1;
            Ok(ClaimedRunResult::Completed("first"))
        })
        .unwrap();
        assert_eq!(result, ClaimedRunResult::Completed("first"));
        assert_eq!(attempts, 1);
    }
    #[test]
    fn claim_surfaces_a_terminal_sentinel_instead_of_retrying_past_it() {
        // A contention that observes a terminal sentinel ends the retry loop immediately; only a
        // genuine Busy (no sentinel yet) is retried.
        let root = tempfile::tempdir().unwrap();
        write_run_json_atomic(
            &root.path().join("final.json"),
            &serde_json::json!({"version":1,"runId":"run","outcome":"merged"}),
            0o600,
        )
        .unwrap();
        let mut attempts = 0;
        let result = claim_with_attempts(&[1, 1, 1], |_| {
            attempts += 1;
            Ok::<_, String>(match read_terminal_run(root.path(), "run")? {
                Some(terminal) => ClaimedRunResult::<()>::Terminal(terminal),
                None => ClaimedRunResult::Busy,
            })
        })
        .unwrap();
        assert_eq!(
            result,
            ClaimedRunResult::Terminal(FinalizationTerminalResult {
                run_id: "run".into(),
                outcome: "merged".into()
            })
        );
        assert_eq!(attempts, 1);
    }
    #[test]
    fn abandoned_terminal_and_admin_tamper_are_retained() {
        let root = tempfile::tempdir().unwrap();
        for (file, outcome) in [
            ("abandoned.json", "abandoned_unknown"),
            ("final.json", "admin_tamper"),
        ] {
            write_run_json_atomic(
                &root.path().join(file),
                &serde_json::json!({"runId":"run","outcome":outcome}),
                0o600,
            )
            .unwrap();
            let result =
                with_run_finalization_claim(&root.path().join("locks"), root.path(), "run", || {
                    Err::<(), _>("must not execute".into())
                })
                .unwrap();
            assert_eq!(
                result,
                ClaimedRunResult::Terminal(FinalizationTerminalResult {
                    run_id: "run".into(),
                    outcome: outcome.into()
                })
            );
        }
    }
}
