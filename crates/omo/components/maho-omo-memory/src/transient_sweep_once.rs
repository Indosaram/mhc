//! One transient reclaim per memory root per engine process, off the session-open path (`transient-sweep-once.ts`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub const TRANSIENT_RUN_MAX_AGE_MS: i64 = 24 * 60 * 60 * 1000;
const TRANSIENT_SWEEP_DEFER_MS: u64 = 1_000;

static CLAIMED_ROOTS: OnceLock<Mutex<HashMap<PathBuf, i64>>> = OnceLock::new();

fn claimed_roots() -> &'static Mutex<HashMap<PathBuf, i64>> {
    CLAIMED_ROOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct TransientSweepSchedule {
    memory_root: PathBuf,
    claimed_at: i64,
    started: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

impl TransientSweepSchedule {
    /// Cancels the deferred sweep if it has not started yet, handing the claim back so a later
    /// session open in the same process sweeps instead.
    pub fn cancel(&self) {
        if self.started.load(Ordering::Acquire) {
            return;
        }
        self.cancelled.store(true, Ordering::Release);
        let mut claims = claimed_roots()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if claims.get(&self.memory_root).copied() == Some(self.claimed_at) {
            claims.remove(&self.memory_root);
        }
    }
}

/// Schedules at most one deferred transient sweep per `memory_root` per 24h in this process, off
/// the session-open path. Returns `None` when a valid active/recent claim exists.
pub fn schedule_transient_sweep_once(
    memory_root: &Path,
    now_ms: i64,
    defer_ms: Option<u64>,
    start: impl FnOnce() + Send + 'static,
) -> Option<TransientSweepSchedule> {
    let key = memory_root.to_path_buf();
    {
        let mut claims = claimed_roots()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(&previous) = claims.get(&key)
            && now_ms.saturating_sub(previous) < TRANSIENT_RUN_MAX_AGE_MS
        {
            return None;
        }
        claims.insert(key.clone(), now_ms);
    }

    let started = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let delay = Duration::from_millis(defer_ms.unwrap_or(TRANSIENT_SWEEP_DEFER_MS));
    let started_bg = started.clone();
    let cancelled_bg = cancelled.clone();

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            tokio::time::sleep(delay).await;
            if cancelled_bg.load(Ordering::Acquire) {
                return;
            }
            started_bg.store(true, Ordering::Release);
            let _ = tokio::task::spawn_blocking(start).await;
        });
    } else {
        std::thread::spawn(move || {
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            if cancelled_bg.load(Ordering::Acquire) {
                return;
            }
            started_bg.store(true, Ordering::Release);
            if cancelled_bg.load(Ordering::Acquire) {
                return;
            }
            start();
        });
    }

    Some(TransientSweepSchedule {
        memory_root: key,
        claimed_at: now_ms,
        started,
        cancelled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn deduplicates_within_interval_and_hands_claim_back_on_early_cancel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("early-cancel");
        let runs = Arc::new(AtomicUsize::new(0));
        let r1 = runs.clone();
        let first = schedule_transient_sweep_once(&root, 1_000, Some(60_000), move || {
            r1.fetch_add(1, Ordering::SeqCst);
        })
        .expect("first schedule");

        let r2 = runs.clone();
        assert!(
            schedule_transient_sweep_once(&root, 2_000, Some(0), move || {
                r2.fetch_add(1, Ordering::SeqCst);
            })
            .is_none(),
            "second open within 24h must be coalesced"
        );

        first.cancel();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let r3 = runs.clone();
        let rearmed = schedule_transient_sweep_once(&root, 2_500, Some(0), move || {
            r3.fetch_add(1, Ordering::SeqCst);
            let _ = tx.send(());
        })
        .expect("cancelled schedule must hand claim back");

        rx.await.expect("rearmed sweep ran");
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        let r4 = runs.clone();
        assert!(
            schedule_transient_sweep_once(
                &root,
                2_600 + TRANSIENT_RUN_MAX_AGE_MS,
                Some(0),
                move || {
                    r4.fetch_add(1, Ordering::SeqCst);
                }
            )
            .is_some(),
            "claim expires after 24h"
        );
        drop(rearmed);
    }
}
