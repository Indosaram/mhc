//! senpi `modes/rpc/compact-mirror-pending.ts`: bounded pre-activation buffer for a selected
//! mirror admission (128 records / 1 MiB before activation; no uncharged `lines[]`).

pub const MIRROR_PENDING_LIMITS_ENTRIES: usize = 128;
pub const MIRROR_PENDING_LIMITS_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorPendingReason {
    AdmissionLimit,
    AttemptCancelled,
    StorageUnavailable,
}

impl MirrorPendingReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdmissionLimit => "admission_limit",
            Self::AttemptCancelled => "attempt_cancelled",
            Self::StorageUnavailable => "storage_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirrorPendingStatus {
    pub entries: usize,
    pub bytes: usize,
}

struct PendingRecord {
    deliver: Box<dyn FnOnce() + Send>,
    drop_fn: Option<Box<dyn FnOnce() + Send>>,
}

#[derive(Default)]
pub struct MirrorPending {
    records: Vec<PendingRecord>,
    bytes: usize,
    active: bool,
    pub reason: Option<MirrorPendingReason>,
}

impl MirrorPending {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(&self) -> MirrorPendingStatus {
        MirrorPendingStatus {
            entries: self.records.len(),
            bytes: self.bytes,
        }
    }

    pub fn enqueue(
        &mut self,
        line: &str,
        deliver: impl FnOnce() + Send + 'static,
        drop_fn: Option<Box<dyn FnOnce() + Send>>,
    ) {
        for action in self.enqueue_actions(line, Box::new(deliver), drop_fn) {
            action();
        }
    }

    pub fn enqueue_shared(
        pending: &std::sync::Mutex<Self>,
        line: &str,
        deliver: impl FnOnce() + Send + 'static,
        drop_fn: Option<Box<dyn FnOnce() + Send>>,
    ) {
        let actions = pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .enqueue_actions(line, Box::new(deliver), drop_fn);
        for action in actions {
            action();
        }
    }

    fn enqueue_actions(
        &mut self,
        line: &str,
        deliver: Box<dyn FnOnce() + Send>,
        drop_fn: Option<Box<dyn FnOnce() + Send>>,
    ) -> Vec<Box<dyn FnOnce() + Send>> {
        if self.reason.is_some() {
            return drop_fn.into_iter().collect();
        }
        if self.active {
            return vec![deliver];
        }
        let bytes = line.len();
        if self.records.len() >= MIRROR_PENDING_LIMITS_ENTRIES
            || self.bytes + bytes > MIRROR_PENDING_LIMITS_BYTES
        {
            self.reason = Some(MirrorPendingReason::AdmissionLimit);
            let mut actions = self.clear_actions();
            actions.extend(drop_fn);
            return actions;
        }
        self.bytes += bytes;
        self.records.push(PendingRecord { deliver, drop_fn });
        Vec::new()
    }

    /// Synchronous ownership transfer into the already installed actor barrier, never a second producer.
    pub fn activate(&mut self) {
        for action in self.activate_actions() {
            action();
        }
    }

    pub fn activate_shared(pending: &std::sync::Mutex<Self>) {
        let actions = pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .activate_actions();
        for action in actions {
            action();
        }
    }

    fn activate_actions(&mut self) -> Vec<Box<dyn FnOnce() + Send>> {
        if self.reason.is_some() || self.active {
            return Vec::new();
        }
        self.active = true;
        let records = std::mem::take(&mut self.records);
        self.bytes = 0;
        records.into_iter().map(|record| record.deliver).collect()
    }

    pub fn retire(&mut self, reason: Option<MirrorPendingReason>) {
        self.reason
            .get_or_insert(reason.unwrap_or(MirrorPendingReason::AttemptCancelled));
        for action in self.clear_actions() {
            action();
        }
    }

    pub fn retire_shared(pending: &std::sync::Mutex<Self>, reason: Option<MirrorPendingReason>) {
        let actions = {
            let mut guard = pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .reason
                .get_or_insert(reason.unwrap_or(MirrorPendingReason::AttemptCancelled));
            guard.clear_actions()
        };
        for action in actions {
            action();
        }
    }

    fn clear_actions(&mut self) -> Vec<Box<dyn FnOnce() + Send>> {
        let records = std::mem::take(&mut self.records);
        self.bytes = 0;
        records
            .into_iter()
            .filter_map(|record| record.drop_fn)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn shared_activation_and_retirement_run_callbacks_without_lock() {
        for activate in [true, false] {
            let pending = Arc::new(std::sync::Mutex::new(MirrorPending::new()));
            let calls = Arc::new(AtomicUsize::new(0));
            let delivered_pending = Arc::clone(&pending);
            let delivered_calls = Arc::clone(&calls);
            let dropped_pending = Arc::clone(&pending);
            let dropped_calls = Arc::clone(&calls);
            MirrorPending::enqueue_shared(
                &pending,
                "line",
                move || {
                    assert!(delivered_pending.try_lock().is_ok());
                    delivered_calls.fetch_add(1, Ordering::SeqCst);
                },
                Some(Box::new(move || {
                    assert!(dropped_pending.try_lock().is_ok());
                    dropped_calls.fetch_add(1, Ordering::SeqCst);
                })),
            );
            if activate {
                MirrorPending::activate_shared(&pending);
            } else {
                MirrorPending::retire_shared(&pending, None);
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn buffers_until_activate_and_delivers_immediately_after() {
        let mut pending = MirrorPending::new();
        let delivered = Arc::new(AtomicUsize::new(0));
        let d1 = delivered.clone();
        pending.enqueue(
            "{\"a\":1}\n",
            move || {
                d1.fetch_add(1, Ordering::SeqCst);
            },
            None,
        );
        assert_eq!(delivered.load(Ordering::SeqCst), 0);
        assert_eq!(
            pending.status(),
            MirrorPendingStatus {
                entries: 1,
                bytes: 8
            }
        );

        pending.activate();
        assert_eq!(delivered.load(Ordering::SeqCst), 1);
        assert_eq!(
            pending.status(),
            MirrorPendingStatus {
                entries: 0,
                bytes: 0
            }
        );

        let d2 = delivered.clone();
        pending.enqueue(
            "{\"b\":2}\n",
            move || {
                d2.fetch_add(1, Ordering::SeqCst);
            },
            None,
        );
        assert_eq!(delivered.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn enforces_entry_and_byte_limits_with_admission_limit_refusal() {
        let mut pending = MirrorPending::new();
        let dropped = Arc::new(AtomicUsize::new(0));
        for _ in 0..MIRROR_PENDING_LIMITS_ENTRIES {
            let d = dropped.clone();
            pending.enqueue(
                "x",
                || {},
                Some(Box::new(move || {
                    d.fetch_add(1, Ordering::SeqCst);
                })),
            );
        }
        assert!(pending.reason.is_none());
        let d = dropped.clone();
        pending.enqueue(
            "overflow",
            || {},
            Some(Box::new(move || {
                d.fetch_add(1, Ordering::SeqCst);
            })),
        );
        assert_eq!(pending.reason, Some(MirrorPendingReason::AdmissionLimit));
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            MIRROR_PENDING_LIMITS_ENTRIES + 1
        );
    }

    #[test]
    fn retire_drops_buffered_records() {
        let mut pending = MirrorPending::new();
        let dropped = Arc::new(AtomicUsize::new(0));
        let d = dropped.clone();
        pending.enqueue(
            "abc",
            || {},
            Some(Box::new(move || {
                d.fetch_add(1, Ordering::SeqCst);
            })),
        );
        pending.retire(None);
        assert_eq!(pending.reason, Some(MirrorPendingReason::AttemptCancelled));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        pending.activate();
        assert_eq!(
            pending.status(),
            MirrorPendingStatus {
                entries: 0,
                bytes: 0
            }
        );
    }
}
