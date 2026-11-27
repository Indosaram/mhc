//! Port of senpi `packages/coding-agent/src/modes/rpc/session-worker-intake.ts`.
//!
//! The host half of one worker incarnation's payload transport (S12b): its permits, its one
//! physical ordinal sequence, and the staging of fragments into the lane's S12a store.

use crate::session_worker_permits::{
    FRAGMENT_MAX_BYTES, HostPermits, WorkerPermitLedger, host_worker_permits,
};
use crate::session_worker_signals::WorkerSignal;
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

const PAID_TYPES: [&str; 6] = [
    "ready",
    "snapshot",
    "output",
    "canonical",
    "canonical_unavailable",
    "fragment",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagingRefusal {
    StagingOverByteLimit,
    FragmentOverLimit,
    LengthMismatch,
    StagingClosed,
    Failed,
}

impl StagingRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StagingOverByteLimit => "staging_over_byte_limit",
            Self::FragmentOverLimit => "fragment_over_limit",
            Self::LengthMismatch => "length_mismatch",
            Self::StagingClosed => "staging_closed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("transfer staging refused ({reason:?})")]
pub struct TransferStagingError {
    pub reason: StagingRefusal,
}

pub type StagedId = u64;

pub trait StagedTransferHandle: Send {
    fn append(&mut self, text: &str) -> Result<(), TransferStagingError>;
    fn commit(self: Box<Self>) -> Result<StagedId, TransferStagingError>;
    fn abort(self: Box<Self>);
}

pub trait TransferStaging: Send + Sync {
    fn begin(
        &self,
        kind: &str,
        id: &str,
        length: usize,
    ) -> Result<Box<dyn StagedTransferHandle>, TransferStagingError>;
    fn read_chunks(&self, staged_id: StagedId) -> BoxFuture<'static, Result<Vec<String>, String>>;
    fn abandon(&self, staged_id: StagedId);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRefusalEvent {
    pub event_type: String,
    pub ordinal: Option<u64>,
    pub length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRefusal {
    pub cause: StagingRefusal,
    pub event: TransferRefusalEvent,
}

#[derive(Debug, Clone)]
pub struct FragmentFrame {
    pub of: String,
    pub length: usize,
    pub offset: usize,
    pub text: String,
    pub ordinal: Option<u64>,
    pub first_ordinal: Option<u64>,
    pub signal: Option<WorkerSignal>,
    pub epoch_signal: Option<WorkerSignal>,
}

struct OpenTransfer {
    transfer: Box<dyn StagedTransferHandle>,
    fed: usize,
}

pub struct WorkerIntake {
    pub permits: Arc<HostPermits>,
    staging: Arc<dyn TransferStaging>,
    fail: Arc<dyn Fn(&str) + Send + Sync>,
    expected: u64,
    serial: u64,
    open: Option<OpenTransfer>,
    refusal: Option<TransferRefusal>,
    assembled_markers: HashSet<u64>,
    next_marker: u64,
}

impl WorkerIntake {
    pub fn new(
        staging: Arc<dyn TransferStaging>,
        fail: Arc<dyn Fn(&str) + Send + Sync>,
        ledger: Option<Arc<WorkerPermitLedger>>,
    ) -> Self {
        let ledger = ledger.unwrap_or_else(host_worker_permits);
        Self {
            permits: HostPermits::new(ledger),
            staging,
            fail,
            expected: 0,
            serial: 0,
            open: None,
            refusal: None,
            assembled_markers: HashSet::new(),
            next_marker: 1,
        }
    }

    pub fn next_ordinal(&self) -> u64 {
        self.expected
    }

    pub fn refusal(&self) -> Option<&TransferRefusal> {
        self.refusal.as_ref()
    }

    pub fn received(&mut self, msg_type: &str, assembled_marker: Option<u64>) {
        let was_assembled =
            assembled_marker.is_some_and(|marker| self.assembled_markers.remove(&marker));
        if PAID_TYPES.contains(&msg_type) && !was_assembled {
            self.permits.returned();
        }
    }

    /// A gap, duplicate or reordered physical frame is denied and ends this incarnation before any mutation.
    pub fn in_order(&mut self, ordinal: Option<u64>, signal: Option<&WorkerSignal>) -> bool {
        if ordinal == Some(self.expected) {
            self.expected += 1;
            return true;
        }
        if let Some(sig) = signal {
            sig.acknowledge(false);
        }
        let kind = match ordinal {
            Some(ord) if ord < self.expected => "duplicate",
            _ => "gap",
        };
        (self.fail)(&format!("session_worker_frame_{kind}"));
        false
    }

    /// Stages one fragment; the final one returns a future resolving to `(assembled_value, marker)`.
    pub fn fragment(
        &mut self,
        frame: FragmentFrame,
    ) -> Option<BoxFuture<'static, Result<(Value, u64), String>>> {
        let end = frame.offset + frame.text.encode_utf16().count() * 2;
        let is_final = end == frame.length;
        if frame.ordinal.is_some()
            && !is_final
            && !self.in_order(frame.ordinal, frame.signal.as_ref())
        {
            self.refuse(&frame, None);
            return None;
        }
        let fed = self.open.as_ref().map_or(0, |o| o.fed);
        if frame.offset != fed {
            let kind = if frame.offset < fed {
                "duplicate"
            } else {
                "gap"
            };
            self.refuse(&frame, Some(&format!("session_worker_frame_{kind}")));
            return None;
        }
        if self.open.is_none() {
            self.serial += 1;
            let transfer_id = format!("t{}", self.serial);
            match self
                .staging
                .begin("worker_frame", &transfer_id, frame.length)
            {
                Ok(transfer) => {
                    self.open = Some(OpenTransfer { transfer, fed });
                }
                Err(err) => {
                    return self.record_staging_error(&frame, err.reason);
                }
            }
        }
        if let Some(open) = self.open.as_mut() {
            if let Err(err) = open.transfer.append(&frame.text) {
                return self.record_staging_error(&frame, err.reason);
            }
            open.fed = end;
        }
        if !is_final {
            return None;
        }
        let open = self.open.take()?;
        let content = match open.transfer.commit() {
            Ok(id) => id,
            Err(err) => return self.record_staging_error(&frame, err.reason),
        };
        let marker = self.next_marker;
        self.next_marker += 1;
        self.assembled_markers.insert(marker);
        let staging = self.staging.clone();
        let ordinal = frame.ordinal;
        let first_ordinal = frame.first_ordinal;
        Some(Box::pin(async move {
            struct AbandonGuard {
                staging: Arc<dyn TransferStaging>,
                id: StagedId,
            }
            impl Drop for AbandonGuard {
                fn drop(&mut self) {
                    self.staging.abandon(self.id);
                }
            }
            let _guard = AbandonGuard {
                staging: staging.clone(),
                id: content,
            };
            let chunks = staging.read_chunks(content).await?;
            let text = chunks.concat();
            let mut message: Value = serde_json::from_str(&text).map_err(|err| err.to_string())?;
            if let Some(obj) = message.as_object_mut() {
                if let Some(ord) = ordinal {
                    obj.insert("ordinal".into(), Value::from(ord));
                }
                if let Some(first) = first_ordinal {
                    obj.insert("firstOrdinal".into(), Value::from(first));
                }
            }
            Ok((message, marker))
        }))
    }

    fn record_staging_error(
        &mut self,
        frame: &FragmentFrame,
        cause: StagingRefusal,
    ) -> Option<BoxFuture<'static, Result<(Value, u64), String>>> {
        let ordinal = frame.ordinal.map(|ord| {
            let rem = frame.length.saturating_sub(frame.offset);
            ord + rem.div_ceil(FRAGMENT_MAX_BYTES) as u64 - 1
        });
        self.refusal = Some(TransferRefusal {
            cause,
            event: TransferRefusalEvent {
                event_type: frame.of.clone(),
                ordinal,
                length: frame.length,
            },
        });
        let at = ordinal.map(|o| format!(" ordinal {o}")).unwrap_or_default();
        let msg = format!(
            "session_worker_transfer_{}: {}{} ({} stored bytes)",
            cause.as_str(),
            frame.of,
            at,
            frame.length
        );
        self.refuse(frame, Some(&msg));
        None
    }

    fn refuse(&mut self, frame: &FragmentFrame, error: Option<&str>) {
        if let Some(open) = self.open.take() {
            open.transfer.abort();
        }
        if let Some(err) = error {
            if let Some(sig) = &frame.signal {
                sig.acknowledge(false);
            }
            (self.fail)(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MemTransfer(Mutex<String>);
    impl StagedTransferHandle for MemTransfer {
        fn append(&mut self, text: &str) -> Result<(), TransferStagingError> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_str(text);
            Ok(())
        }
        fn commit(self: Box<Self>) -> Result<StagedId, TransferStagingError> {
            Ok(1)
        }
        fn abort(self: Box<Self>) {}
    }

    struct MemStaging {
        buf: Mutex<Option<String>>,
    }
    impl TransferStaging for MemStaging {
        fn begin(
            &self,
            _: &str,
            _: &str,
            _: usize,
        ) -> Result<Box<dyn StagedTransferHandle>, TransferStagingError> {
            Ok(Box::new(MemTransfer(Mutex::new(String::new()))))
        }
        fn read_chunks(&self, _: StagedId) -> BoxFuture<'static, Result<Vec<String>, String>> {
            let text = self
                .buf
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .unwrap_or_default();
            Box::pin(async move { Ok(vec![text]) })
        }
        fn abandon(&self, _: StagedId) {}
    }

    #[test]
    fn in_order_rejects_gaps_and_duplicates() {
        let failures = Arc::new(Mutex::new(Vec::<String>::new()));
        let f_clone = failures.clone();
        let staging = Arc::new(MemStaging {
            buf: Mutex::new(None),
        });
        let mut intake = WorkerIntake::new(
            staging,
            Arc::new(move |err| {
                f_clone
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(err.to_owned());
            }),
            None,
        );
        assert!(intake.in_order(Some(0), None));
        assert_eq!(intake.next_ordinal(), 1);
        assert!(!intake.in_order(Some(0), None));
        assert!(!intake.in_order(Some(5), None));
        assert_eq!(
            *failures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["session_worker_frame_duplicate", "session_worker_frame_gap"]
        );
        intake.permits.close();
    }
}
