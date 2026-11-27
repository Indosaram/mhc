//! Port of senpi packages/coding-agent/src/core/stall-warning.ts.
//!
//! Deadline-bounded warnings for waits that have no deadline of their own (`senpi#6189`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

pub const STALL_WARNING_MS: u64 = 30_000;

pub fn stall_threshold_ms(timeout_seconds: Option<f64>) -> u64 {
    let Some(secs) = timeout_seconds else {
        return STALL_WARNING_MS;
    };
    if !secs.is_finite() || secs <= 0.0 {
        return STALL_WARNING_MS;
    }
    let ms = (secs * 1000.0).trunc() as u64;
    ms.clamp(1, STALL_WARNING_MS)
}

pub struct StallWatch {
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl StallWatch {
    pub fn stop(&self) {
        if let Some(handle) = self
            .handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            handle.abort();
        }
    }
}

impl Drop for StallWatch {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn watch_for_stall(threshold_ms: u64, on_stall: Arc<dyn Fn(u64) + Send + Sync>) -> StallWatch {
    let started = Instant::now();
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(threshold_ms)).await;
        let elapsed = started.elapsed().as_millis() as u64;
        on_stall(elapsed);
    });
    StallWatch {
        handle: Mutex::new(Some(handle)),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingQueueSnapshot {
    pub queue_depth: usize,
    pub queue_head: Option<String>,
    pub age_ms: Option<u64>,
}

struct QueueEntry {
    label: String,
    enqueued_at: Instant,
}

#[derive(Default)]
pub struct PendingQueueLedger {
    inner: Mutex<(u64, BTreeMap<u64, QueueEntry>)>,
}

impl PendingQueueLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn track_enter(&self, label: impl Into<String>) -> u64 {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = guard.0;
        guard.0 += 1;
        guard.1.insert(
            id,
            QueueEntry {
                label: label.into(),
                enqueued_at: Instant::now(),
            },
        );
        id
    }

    pub fn release(&self, id: u64) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
            .remove(&id);
    }

    pub fn snapshot(&self) -> PendingQueueSnapshot {
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((_, head)) = guard.1.iter().next() else {
            return PendingQueueSnapshot::default();
        };
        PendingQueueSnapshot {
            queue_depth: guard.1.len(),
            queue_head: Some(head.label.clone()),
            age_ms: Some(head.enqueued_at.elapsed().as_millis() as u64),
        }
    }
}
