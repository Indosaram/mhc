//! Recovery of a reflection reservation whose holder died (#10377).
//!
//! Session-start reconciliation runs once per session, so a host that dies mid-publish leaves
//! runtime/reflection/active.lock held, and every later request only queues behind it. This module
//! decides, fail-closed, whether that holder is provably gone; only then is the existing
//! reconciliation asked to settle the run.

pub type ShouldReconcileCallback = Option<Box<dyn Fn(&str) -> bool + Send + Sync>>;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveHolderVerdict {
    None,
    Live { run_id: String },
    Dead { run_id: String },
    Stuck { run_id: String, reason: String },
}

pub fn classify_active_reflection_holder(
    reflection_dir: &Path,
    current_hostname: Option<&str>,
) -> ActiveHolderVerdict {
    use memory_core::reflection::stale_holder_proof::{
        ReflectionLockProbe, ReflectionLockVerdict, classify_reflection_lock_holder,
    };
    let host = || current_hostname.unwrap_or_default().to_owned();
    let probe = ReflectionLockProbe {
        hostname: current_hostname.map(|_| &host as &dyn Fn() -> String),
        ..Default::default()
    };
    match classify_reflection_lock_holder(reflection_dir, &probe) {
        ReflectionLockVerdict::None => ActiveHolderVerdict::None,
        ReflectionLockVerdict::Live { run_id } => ActiveHolderVerdict::Live { run_id },
        ReflectionLockVerdict::Dead { run_id } => ActiveHolderVerdict::Dead { run_id },
        ReflectionLockVerdict::Stuck { run_id, reason } => {
            ActiveHolderVerdict::Stuck { run_id, reason }
        }
    }
}

pub fn stale_reflection_lock_notice(reflection_dir: &Path, run_id: &str, reason: &str) -> String {
    format!(
        "memory reflection is stuck on a stale lock: run {run_id} still holds it and {reason}, so new reflection requests only queue. Next step: run /doctor; if no reflection run is active, move {} aside (keep a backup) and start a new session.",
        reflection_dir.join("active.lock").display()
    )
}

pub async fn recover_stale_reflection_holder<F, Fut>(
    reflection_dir: &Path,
    reconcile: F,
    should_reconcile: ShouldReconcileCallback,
) -> (ActiveHolderVerdict, Option<String>)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let verdict = classify_active_reflection_holder(reflection_dir, None);
    match &verdict {
        ActiveHolderVerdict::Dead { run_id } => {
            if let Some(should) = &should_reconcile
                && !should(run_id) {
                    let notice = stale_reflection_lock_notice(
                        reflection_dir,
                        run_id,
                        "recovering it did not release the lock",
                    );
                    return (verdict, Some(notice));
                }
            reconcile().await;
            let after = classify_active_reflection_holder(reflection_dir, None);
            match &after {
                ActiveHolderVerdict::Stuck { run_id: r, reason } => {
                    let notice = stale_reflection_lock_notice(reflection_dir, r, reason);
                    (after, Some(notice))
                }
                ActiveHolderVerdict::Dead { run_id: r } if r == run_id => {
                    let notice = stale_reflection_lock_notice(
                        reflection_dir,
                        r,
                        "recovering it did not release the lock",
                    );
                    (after, Some(notice))
                }
                _ => (verdict, None),
            }
        }
        ActiveHolderVerdict::Stuck { run_id, reason } => {
            let notice = stale_reflection_lock_notice(reflection_dir, run_id, reason);
            (verdict, Some(notice))
        }
        _ => (verdict, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_missing_active_lock_when_classified_then_verdict_none() {
        let temp = tempfile::tempdir().unwrap();
        let verdict = classify_active_reflection_holder(temp.path(), None);
        assert_eq!(verdict, ActiveHolderVerdict::None);
    }

    #[test]
    fn given_invalid_json_when_classified_then_stuck() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("active.lock"), "not-json").unwrap();
        let verdict = classify_active_reflection_holder(temp.path(), None);
        assert!(matches!(verdict, ActiveHolderVerdict::Stuck { .. }));
    }
}
