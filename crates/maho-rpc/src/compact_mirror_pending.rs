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
        if self.reason.is_some() {
            if let Some(drop_fn) = drop_fn {
                drop_fn();
            }
            return;
        }
        if self.active {
            deliver();
            return;
        }
        let bytes = line.len();
        if self.records.len() >= MIRROR_PENDING_LIMITS_ENTRIES
            || self.bytes + bytes > MIRROR_PENDING_LIMITS_BYTES
        {
            self.reason = Some(MirrorPendingReason::AdmissionLimit);
            self.clear();
            if let Some(drop_fn) = drop_fn {
                drop_fn();
            }
            return;
        }
        self.bytes += bytes;
        self.records.push(PendingRecord {
            deliver: Box::new(deliver),
            drop_fn,
        });
    }

    /// Synchronous ownership transfer into the already installed actor barrier, never a second producer.
    pub fn activate(&mut self) {
        if self.reason.is_some() || self.active {
            return;
        }
        self.active = true;
        let records = std::mem::take(&mut self.records);
        self.bytes = 0;
        for record in records {
            (record.deliver)();
        }
    }

    pub fn retire(&mut self, reason: Option<MirrorPendingReason>) {
        self.reason
            .get_or_insert(reason.unwrap_or(MirrorPendingReason::AttemptCancelled));
        self.clear();
    }

    fn clear(&mut self) {
        let records = std::mem::take(&mut self.records);
        self.bytes = 0;
        for record in records {
            if let Some(drop_fn) = record.drop_fn {
                drop_fn();
            }
        }
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
