//! Port of senpi `packages/coding-agent/src/modes/rpc/session-worker-transfer.ts`
//! (and `CompactProducerStamp` from `compact-producer-stamp.ts`).
//!
//! The worker half of the payload transport (S12b; RULING-63 Q3, RULING-64).

use crate::session_worker_permits::{
    FRAGMENT_MAX_BYTES, PermitBlock, PermitOutcome, WorkerPermitGate,
};
use crate::session_worker_protocol::SESSION_WORKER_LIMITS;
use maho_core::session_recovery_identity::RecoveryCarrier;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// UTF-16 code units one fragment carries: D stored bytes, two per UTF-16 code unit.
const FRAGMENT_UNITS: usize = FRAGMENT_MAX_BYTES / 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StampedOrdinal {
    pub ordinal: u64,
    pub first_ordinal: Option<u64>,
}

/// Stamps contiguous ordinals and tracks first-message ordinal bindings (`compact-producer-stamp.ts`).
#[derive(Default)]
pub struct CompactProducerStamp {
    next: u64,
    first: HashMap<String, u64>,
    last_ordinal: Option<u64>,
    last_first_ordinal: Option<u64>,
    last_message_id: Option<String>,
}

impl CompactProducerStamp {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stamp(
        &mut self,
        carrier: Option<&RecoveryCarrier>,
        canonical_type: Option<&str>,
        legacy_type: Option<&str>,
    ) -> StampedOrdinal {
        let ordinal = self.next;
        self.next += 1;
        self.last_ordinal = Some(ordinal);
        self.last_first_ordinal = None;
        self.last_message_id = None;
        let Some(carrier) = carrier else {
            return StampedOrdinal {
                ordinal,
                first_ordinal: None,
            };
        };
        let ends = canonical_type == Some("message_end") || legacy_type == Some("message_end");
        let first_ordinal = if canonical_type.is_some() {
            Some(*self.first.get(&carrier.message_id).unwrap_or(&ordinal))
        } else {
            None
        };
        if ends {
            self.first.remove(&carrier.message_id);
        } else if let Some(first_ord) = first_ordinal {
            self.first.insert(carrier.message_id.clone(), first_ord);
        }
        self.last_first_ordinal = first_ordinal;
        self.last_message_id = Some(carrier.message_id.clone());
        StampedOrdinal {
            ordinal,
            first_ordinal,
        }
    }

    /// Moves the last stamped unit's ordinal past `prefix` fragment frames.
    pub fn defer(&mut self, prefix: u64) -> StampedOrdinal {
        let ordinal = self.last_ordinal.unwrap_or(0);
        let moved = ordinal + prefix;
        self.next += prefix;
        match self.last_first_ordinal {
            None => StampedOrdinal {
                ordinal: moved,
                first_ordinal: None,
            },
            Some(first) if first != ordinal => StampedOrdinal {
                ordinal: moved,
                first_ordinal: Some(first),
            },
            Some(_) => {
                if let Some(ref msg_id) = self.last_message_id
                    && self.first.get(msg_id) == Some(&ordinal)
                {
                    self.first.insert(msg_id.clone(), moved);
                }
                StampedOrdinal {
                    ordinal: moved,
                    first_ordinal: Some(moved),
                }
            }
        }
    }

    pub fn forget(&mut self) {
        self.first.clear();
    }
}

pub struct WorkerTransfer {
    post: Arc<dyn Fn(Value) + Send + Sync>,
    gate: WorkerPermitGate,
    stamp: CompactProducerStamp,
    fail: Arc<dyn Fn(&str) + Send + Sync>,
    on_closed: Arc<dyn Fn() + Send + Sync>,
    spare: bool,
}

impl WorkerTransfer {
    pub fn new(
        post: Arc<dyn Fn(Value) + Send + Sync>,
        block: Arc<PermitBlock>,
        stamp: CompactProducerStamp,
        fail: Arc<dyn Fn(&str) + Send + Sync>,
        on_closed: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            post,
            gate: WorkerPermitGate::new(block, SESSION_WORKER_LIMITS.control_ms),
            stamp,
            fail,
            on_closed,
            spare: false,
        }
    }

    pub fn send<F>(&mut self, build: F) -> Result<(), String>
    where
        F: FnOnce() -> Result<Value, String>,
    {
        if !self.take() {
            return Ok(());
        }
        let message = match build() {
            Ok(msg) => msg,
            Err(err) => {
                self.spare = true;
                return Err(err);
            }
        };
        let text = serde_json::to_string(&message).map_err(|e| e.to_string())?;
        let units: Vec<u16> = text.encode_utf16().collect();
        if units.len() <= FRAGMENT_UNITS {
            (self.post)(message);
            return Ok(());
        }
        let mut ranges = Vec::new();
        let mut start = 0;
        while start < units.len() {
            let mut end = start.saturating_add(FRAGMENT_UNITS).min(units.len());
            if end < units.len() && (0xd800..=0xdbff).contains(&units[end - 1]) {
                end -= 1;
            }
            ranges.push((start, end));
            start = end;
        }
        let count = ranges.len();
        let has_ordinal = message.get("ordinal").is_some_and(|v| !v.is_null());
        let commit = has_ordinal.then(|| self.stamp.defer((count - 1) as u64));
        let msg_type = message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let signal = message.get("signal").cloned();

        for (index, (start, end)) in ranges.into_iter().enumerate() {
            if index > 0 && !self.take() {
                return Ok(());
            }
            let is_last = index == count - 1;
            let chunk_text =
                String::from_utf16(&units[start..end]).map_err(|err| err.to_string())?;
            let mut frag = json!({
                "type": "fragment",
                "of": msg_type,
                "length": units.len() * 2,
                "offset": start * 2,
                "text": chunk_text,
            });
            if let Some(commit) = commit {
                frag["ordinal"] = json!(commit.ordinal - (count - 1 - index) as u64);
                if is_last && let Some(first) = commit.first_ordinal {
                    frag["firstOrdinal"] = json!(first);
                }
            }
            if is_last && let Some(ref sig) = signal {
                frag["signal"] = sig.clone();
            }
            (self.post)(frag);
        }
        Ok(())
    }

    fn take(&mut self) -> bool {
        if self.spare {
            self.spare = false;
            return true;
        }
        match self.gate.acquire() {
            PermitOutcome::Granted => true,
            PermitOutcome::Closed => {
                (self.on_closed)();
                false
            }
            other => {
                (self.fail)(&format!("session_worker_permit_{}", other.as_str()));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_worker_permits::{HostPermits, WorkerPermitLedger};
    use std::sync::Mutex;

    #[test]
    fn fragmented_message_preserves_surrogate_pair_at_boundary() {
        let ledger = Arc::new(WorkerPermitLedger::default());
        let host = HostPermits::new(ledger);
        let posted = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured = Arc::clone(&posted);
        let returned = host.clone();
        let mut transfer = WorkerTransfer::new(
            Arc::new(move |value| {
                captured.lock().expect("posted").push(value);
                returned.returned();
            }),
            host.block.clone(),
            CompactProducerStamp::new(),
            Arc::new(|_| {}),
            Arc::new(|| {}),
        );
        let empty = serde_json::to_string(&json!({"type": "output", "body": ""})).expect("json");
        let prefix = empty.find("\"body\":\"").expect("body") + "\"body\":\"".len();
        let body = format!(
            "{}😀{}",
            "a".repeat(FRAGMENT_UNITS - prefix - 1),
            "z".repeat(16)
        );
        let message = json!({"type": "output", "body": body});
        let original = serde_json::to_string(&message).expect("json");
        transfer.send(|| Ok(message)).expect("send");
        let fragments = posted.lock().expect("posted");
        assert!(fragments.len() > 1);
        let reconstructed: String = fragments
            .iter()
            .map(|fragment| fragment["text"].as_str().expect("fragment text"))
            .collect();
        assert_eq!(reconstructed, original);
        let mut offset = 0;
        for fragment in fragments.iter() {
            assert_eq!(fragment["offset"].as_u64(), Some(offset as u64));
            offset += fragment["text"]
                .as_str()
                .expect("fragment text")
                .encode_utf16()
                .count()
                * 2;
        }
        host.close();
    }

    #[test]
    fn small_message_posts_directly_and_large_message_fragments_with_deferred_ordinal() {
        let ledger = Arc::new(WorkerPermitLedger::default());
        let host = HostPermits::new(ledger);
        let posted = Arc::new(Mutex::new(Vec::<Value>::new()));
        let p_clone = posted.clone();
        let mut stamp = CompactProducerStamp::new();
        let carrier = RecoveryCarrier {
            message_id: "m1".into(),
            epoch: "e1".into(),
            durable_entry_id: None,
        };
        let s0 = stamp.stamp(Some(&carrier), Some("message_start"), None);
        assert_eq!(s0.ordinal, 0);
        assert_eq!(s0.first_ordinal, Some(0));

        let mut transfer = WorkerTransfer::new(
            Arc::new(move |val| {
                p_clone
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(val);
            }),
            host.block.clone(),
            stamp,
            Arc::new(|_| {}),
            Arc::new(|| {}),
        );

        transfer
            .send(|| Ok(json!({ "type": "ready", "ordinal": 0 })))
            .expect("send small");
        assert_eq!(
            posted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            1
        );
        host.returned();

        // Large payload (> FRAGMENT_UNITS chars) fragments and returns permits between frames.
        let host2 = host.clone();
        let posted2 = Arc::new(Mutex::new(Vec::<Value>::new()));
        let p2 = posted2.clone();
        let mut transfer2 = WorkerTransfer::new(
            Arc::new(move |val| {
                p2.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(val);
                host2.returned();
            }),
            host.block.clone(),
            CompactProducerStamp::new(),
            Arc::new(|_| {}),
            Arc::new(|| {}),
        );
        let big = "a".repeat(FRAGMENT_UNITS + 100);
        transfer2
            .send(|| Ok(json!({ "type": "output", "ordinal": 0, "body": big })))
            .expect("send fragmented");
        let frags = posted2
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(frags.len(), 2);
        assert_eq!(frags[0]["type"], "fragment");
        assert_eq!(frags[1]["type"], "fragment");
        assert_eq!(frags[0]["ordinal"], 0);
        assert_eq!(frags[1]["ordinal"], 1);
        host.close();
    }
}
