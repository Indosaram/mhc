//! Port of senpi `packages/coding-agent/src/modes/rpc/worker-capacity.ts`.
//!
//! One process's session-worker budget (`SESSION_WORKER_LIMITS.workers`), shared by every pool
//! that starts session-worker isolates: the session registry and the loopless worker pool count
//! against the same cap (Z2, senpi#5413).

use std::sync::{Arc, Mutex};

pub type WorkerCountFn = Arc<dyn Fn() -> usize + Send + Sync>;

#[derive(Default)]
pub struct WorkerCapacity {
    counts: Arc<Mutex<Vec<(u64, WorkerCountFn)>>>,
    next_id: Mutex<u64>,
}

pub struct WorkerCapacityTrack {
    counts: Arc<Mutex<Vec<(u64, WorkerCountFn)>>>,
    id: u64,
}

impl Drop for WorkerCapacityTrack {
    fn drop(&mut self) {
        self.counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(entry_id, _)| *entry_id != self.id);
    }
}

impl WorkerCapacity {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn track(&self, count: WorkerCountFn) -> WorkerCapacityTrack {
        let id = {
            let mut next = self
                .next_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let id = *next;
            *next += 1;
            id
        };
        self.counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((id, count));
        WorkerCapacityTrack {
            counts: self.counts.clone(),
            id,
        }
    }

    pub fn in_use(&self) -> usize {
        let snapshot: Vec<WorkerCountFn> = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(_, f)| f.clone())
            .collect();
        snapshot.into_iter().map(|f| f()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn sums_across_tracked_pools_and_unregisters_on_drop() {
        let cap = WorkerCapacity::new();
        let a = Arc::new(AtomicUsize::new(3));
        let b = Arc::new(AtomicUsize::new(4));
        let a2 = a.clone();
        let b2 = b.clone();
        let t1 = cap.track(Arc::new(move || a2.load(Ordering::SeqCst)));
        let t2 = cap.track(Arc::new(move || b2.load(Ordering::SeqCst)));
        assert_eq!(cap.in_use(), 7);
        a.store(5, Ordering::SeqCst);
        assert_eq!(cap.in_use(), 9);
        drop(t1);
        assert_eq!(cap.in_use(), 4);
        drop(t2);
        assert_eq!(cap.in_use(), 0);
    }
}
