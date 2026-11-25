//! Port of `packages/coding-agent/src/modes/rpc/machine-op/answer-cache.ts`.
//!
//! `machine_op` answers by `op_id` (`senpi#6752`): an op runs once; a repeat gets the first
//! answer, and a repeat that arrives while the first still runs joins it.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

pub const MACHINE_OP_CACHE_MAX_ENTRIES: usize = 4_096;
pub const MACHINE_OP_CACHE_MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;

pub type ClockFn = Arc<dyn Fn() -> u64 + Send + Sync>;

fn system_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

enum Slot<T> {
    Running {
        owner: String,
        rx: watch::Receiver<Option<Result<T, String>>>,
    },
    Settled {
        owner: String,
        value: T,
        settled_at_ms: u64,
    },
}

impl<T> Slot<T> {
    fn owner(&self) -> &str {
        match self {
            Self::Running { owner, .. } | Self::Settled { owner, .. } => owner,
        }
    }
}

struct CacheInner<T> {
    slots: HashMap<String, Slot<T>>,
    settled_order: VecDeque<String>,
}

impl<T> CacheInner<T> {
    fn expire(&mut self, now_ms: u64) {
        let oldest = now_ms.saturating_sub(MACHINE_OP_CACHE_MAX_AGE_MS);
        self.slots.retain(|_, slot| match slot {
            Slot::Running { .. } => true,
            Slot::Settled { settled_at_ms, .. } => *settled_at_ms >= oldest,
        });
        while let Some(front) = self.settled_order.front() {
            if self.slots.contains_key(front) {
                break;
            }
            self.settled_order.pop_front();
        }
    }

    /// Settled answers leave oldest first; an op still running is never forgotten.
    fn trim(&mut self) {
        while self.slots.len() > MACHINE_OP_CACHE_MAX_ENTRIES {
            let Some(op_id) = self.settled_order.pop_front() else {
                break;
            };
            if matches!(self.slots.get(&op_id), Some(Slot::Settled { .. })) {
                self.slots.remove(&op_id);
            }
        }
    }
}

struct InFlightGuard<'a, T> {
    inner: &'a Mutex<CacheInner<T>>,
    op_id: String,
    active: bool,
}

impl<T> Drop for InFlightGuard<'_, T> {
    fn drop(&mut self) {
        if self.active {
            let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(guard.slots.get(&self.op_id), Some(Slot::Running { .. })) {
                guard.slots.remove(&self.op_id);
            }
        }
    }
}

pub struct MachineOpAnswerCache<T> {
    inner: Mutex<CacheInner<T>>,
    now: ClockFn,
}

impl<T: Clone + Send + Sync + 'static> Default for MachineOpAnswerCache<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + Send + Sync + 'static> MachineOpAnswerCache<T> {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(system_now_ms))
    }

    pub fn with_clock(now: ClockFn) -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                slots: HashMap::new(),
                settled_order: VecDeque::new(),
            }),
            now,
        }
    }

    pub fn len(&self) -> usize {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        guard.expire((self.now)());
        guard.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn settled(&self, op_id: &str) -> Option<T> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        guard.expire((self.now)());
        match guard.slots.get(op_id) {
            Some(Slot::Settled { value, .. }) => Some(value.clone()),
            _ => None,
        }
    }

    pub fn owner_of(&self, op_id: &str) -> Option<String> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        guard.expire((self.now)());
        guard.slots.get(op_id).map(|slot| slot.owner().to_owned())
    }

    pub async fn answer<F, Fut>(&self, op_id: &str, owner: &str, run: F) -> Result<T, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, String>>,
    {
        let existing_rx = {
            let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            guard.expire((self.now)());
            match guard.slots.get(op_id) {
                Some(Slot::Settled { value, .. }) => return Ok(value.clone()),
                Some(Slot::Running { rx, .. }) => Some(rx.clone()),
                None => None,
            }
        };

        if let Some(mut rx) = existing_rx {
            loop {
                if let Some(outcome) = rx.borrow_and_update().clone() {
                    return outcome;
                }
                if rx.changed().await.is_err() {
                    return Err("in-flight machine_op leader dropped before settling".to_owned());
                }
            }
        }

        let (tx, rx) = watch::channel(None);
        let existing_rx = {
            let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            guard.expire((self.now)());
            if let Some(slot) = guard.slots.get(op_id) {
                match slot {
                    Slot::Settled { value, .. } => return Ok(value.clone()),
                    Slot::Running { rx, .. } => Some(rx.clone()),
                }
            } else {
                guard.slots.insert(
                    op_id.to_owned(),
                    Slot::Running {
                        owner: owner.to_owned(),
                        rx,
                    },
                );
                None
            }
        };
        if let Some(mut rx) = existing_rx {
            loop {
                if let Some(outcome) = rx.borrow_and_update().clone() {
                    return outcome;
                }
                if rx.changed().await.is_err() {
                    return Err("in-flight machine_op leader dropped before settling".to_owned());
                }
            }
        }

        let mut drop_guard = InFlightGuard {
            inner: &self.inner,
            op_id: op_id.to_owned(),
            active: true,
        };

        let result = run().await;
        drop_guard.active = false;

        match result {
            Ok(value) => {
                {
                    let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
                    guard.slots.insert(
                        op_id.to_owned(),
                        Slot::Settled {
                            owner: owner.to_owned(),
                            value: value.clone(),
                            settled_at_ms: (self.now)(),
                        },
                    );
                    guard.settled_order.push_back(op_id.to_owned());
                    guard.trim();
                }
                let _ = tx.send(Some(Ok(value.clone())));
                Ok(value)
            }
            Err(err) => {
                {
                    let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
                    guard.slots.remove(op_id);
                }
                let _ = tx.send(Some(Err(err.clone())));
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    #[tokio::test]
    async fn forgets_oldest_settled_answer_past_max_entries() {
        let cache = MachineOpAnswerCache::<String>::new();
        let runs = Arc::new(AtomicUsize::new(0));
        for index in 0..MACHINE_OP_CACHE_MAX_ENTRIES {
            let runs = Arc::clone(&runs);
            let res = cache
                .answer(&format!("op-{index}"), "device-a", || async move {
                    let n = runs.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok(format!("run {n}"))
                })
                .await
                .expect("ok");
            assert_eq!(res, format!("run {}", index + 1));
        }
        assert_eq!(cache.len(), MACHINE_OP_CACHE_MAX_ENTRIES);
        assert_eq!(cache.settled("op-0").as_deref(), Some("run 1"));

        let runs_ref = Arc::clone(&runs);
        cache
            .answer(
                &format!("op-{MACHINE_OP_CACHE_MAX_ENTRIES}"),
                "device-a",
                || async move {
                    let n = runs_ref.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok(format!("run {n}"))
                },
            )
            .await
            .expect("ok");

        assert_eq!(cache.len(), MACHINE_OP_CACHE_MAX_ENTRIES);
        assert_eq!(cache.settled("op-0"), None);
        assert_eq!(cache.settled("op-1").as_deref(), Some("run 2"));
    }

    #[tokio::test]
    async fn forgets_answer_older_than_max_age_and_reruns() {
        let now = Arc::new(AtomicU64::new(1_000));
        let now_for_cache = Arc::clone(&now);
        let cache = MachineOpAnswerCache::<String>::with_clock(Arc::new(move || {
            now_for_cache.load(Ordering::SeqCst)
        }));
        let runs = Arc::new(AtomicUsize::new(0));

        let run_once = |runs: Arc<AtomicUsize>| async move {
            let n = runs.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(format!("run {n}"))
        };

        assert_eq!(
            cache
                .answer("op", "device-a", || run_once(Arc::clone(&runs)))
                .await,
            Ok("run 1".to_owned())
        );
        now.fetch_add(MACHINE_OP_CACHE_MAX_AGE_MS, Ordering::SeqCst);
        assert_eq!(
            cache
                .answer("op", "device-a", || run_once(Arc::clone(&runs)))
                .await,
            Ok("run 1".to_owned())
        );
        now.fetch_add(1, Ordering::SeqCst);
        assert_eq!(cache.settled("op"), None);
        assert_eq!(
            cache
                .answer("op", "device-a", || run_once(Arc::clone(&runs)))
                .await,
            Ok("run 2".to_owned())
        );
    }

    #[tokio::test]
    async fn joins_repeat_to_in_flight_run_and_never_exposes_unsettled() {
        let cache = Arc::new(MachineOpAnswerCache::<String>::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<String>();
        let gate_rx = Arc::new(tokio::sync::Mutex::new(Some(gate_rx)));

        let c1 = Arc::clone(&cache);
        let r1 = Arc::clone(&runs);
        let g1 = Arc::clone(&gate_rx);
        let t1 = tokio::spawn(async move {
            c1.answer("op", "device-a", || async move {
                r1.fetch_add(1, Ordering::SeqCst);
                let rx = g1.lock().await.take().expect("rx");
                Ok(rx.await.expect("gate"))
            })
            .await
        });

        tokio::task::yield_now().await;
        assert_eq!(cache.settled("op"), None);
        assert_eq!(cache.owner_of("op").as_deref(), Some("device-a"));

        let c2 = Arc::clone(&cache);
        let r2 = Arc::clone(&runs);
        let t2 = tokio::spawn(async move {
            c2.answer("op", "device-a", || async move {
                r2.fetch_add(1, Ordering::SeqCst);
                Ok("second".to_owned())
            })
            .await
        });

        tokio::task::yield_now().await;
        gate_tx.send("done".to_owned()).expect("send");

        assert_eq!(t1.await.expect("join1"), Ok("done".to_owned()));
        assert_eq!(t2.await.expect("join2"), Ok("done".to_owned()));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(cache.settled("op").as_deref(), Some("done"));
    }
}
