//! Port of senpi packages/coding-agent/src/core/session-recovery-identity.ts.
//!
//! The recovery identity of one session record, never a property on the record, so no byte changes
//! (senpi#4272, R3.C1.2). Upstream keys carriers to event/message objects through WeakMaps; Rust values
//! have no object identity, so the producer hands each observed event its carrier back and callers carry
//! it with the record explicitly (`carry_recovery` is that copy).

use serde_json::Value;

/// The recovery identity a session record carries to the host lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryCarrier {
    pub message_id: String,
    /// Host-issued nonce, internal to the producer/lane handshake; never serialized with a public record.
    pub epoch: String,
    pub durable_entry_id: Option<String>,
}

/// Every copy of a record on its way to the host lane keeps the original's carrier.
pub fn carry_recovery(from: Option<&RecoveryCarrier>) -> Option<RecoveryCarrier> {
    from.cloned()
}

/// A committed lifecycle delivery can predate its terminal record.
pub fn hint(
    carrier: Option<RecoveryCarrier>,
    durable_entry_id: Option<&str>,
) -> Option<RecoveryCarrier> {
    match (carrier, durable_entry_id) {
        (Some(carrier), Some(id)) => Some(RecoveryCarrier {
            durable_entry_id: Some(id.to_owned()),
            ..carrier
        }),
        (carrier, _) => carrier,
    }
}

/// Mints each message's opaque id at its start, in emission order. An assistant stream copies its message
/// per event, so it owns one open slot from start to end; other roles are keyed by the message's
/// `message_key` (the caller's stable handle for that message object). A terminal with no start mints at
/// that terminal; an update with no start has no owner and gets no id.
#[derive(Clone, Debug)]
pub struct SessionRecoveryIdentity {
    open_assistant: Option<String>,
    by_message: std::collections::HashMap<String, String>,
    epoch: String,
}

impl Default for SessionRecoveryIdentity {
    fn default() -> Self {
        Self::new(uuid::Uuid::new_v4().to_string())
    }
}

impl SessionRecoveryIdentity {
    pub fn new(epoch: String) -> Self {
        Self {
            open_assistant: None,
            by_message: std::collections::HashMap::new(),
            epoch,
        }
    }

    /// senpi `setSessionRecoveryEpoch`: a same-epoch reset is a no-op.
    pub fn reset(&mut self, epoch: &str) {
        if epoch == self.epoch {
            return;
        }
        self.epoch = epoch.to_owned();
        self.open_assistant = None;
        self.by_message.clear();
    }

    /// Observes one `message_start` / `message_update` / `message_end` event and returns its carrier.
    pub fn observe(
        &mut self,
        event_type: &str,
        message: &Value,
        message_key: &str,
    ) -> Option<RecoveryCarrier> {
        if !matches!(
            event_type,
            "message_start" | "message_update" | "message_end"
        ) {
            return None;
        }
        let assistant = message.get("role").and_then(Value::as_str) == Some("assistant");
        let known = if assistant {
            self.open_assistant.clone()
        } else {
            self.by_message.get(message_key).cloned()
        };
        let message_id = if event_type == "message_start" {
            uuid::Uuid::new_v4().to_string()
        } else {
            match known {
                Some(id) => id,
                None if event_type == "message_end" => uuid::Uuid::new_v4().to_string(),
                None => return None,
            }
        };
        if assistant {
            self.open_assistant = (event_type != "message_end").then(|| message_id.clone());
        }
        self.by_message
            .insert(message_key.to_owned(), message_id.clone());
        Some(RecoveryCarrier {
            message_id,
            epoch: self.epoch.clone(),
            durable_entry_id: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_assistant_stream_keeps_one_id_from_start_to_end() {
        let mut producer = SessionRecoveryIdentity::new("e1".into());
        let assistant = json!({"role":"assistant"});
        let start = producer
            .observe("message_start", &assistant, "copy-1")
            .expect("start");
        let update = producer
            .observe("message_update", &assistant, "copy-2")
            .expect("update");
        let end = producer
            .observe("message_end", &assistant, "copy-3")
            .expect("end");
        assert_eq!(start.message_id, update.message_id);
        assert_eq!(start.message_id, end.message_id);
        assert_eq!(start.epoch, "e1");
        let next = producer
            .observe("message_start", &assistant, "copy-4")
            .expect("next");
        assert_ne!(next.message_id, start.message_id);
    }

    #[test]
    fn an_update_without_a_start_has_no_owner_and_a_bare_end_mints() {
        let mut producer = SessionRecoveryIdentity::new("e1".into());
        let user = json!({"role":"user"});
        assert!(producer.observe("message_update", &user, "m").is_none());
        assert!(producer.observe("message_end", &user, "m").is_some());
        assert!(producer.observe("agent_end", &user, "m").is_none());
    }

    #[test]
    fn a_same_epoch_reset_keeps_open_slots_and_a_new_one_clears_them() {
        let mut producer = SessionRecoveryIdentity::new("e1".into());
        let assistant = json!({"role":"assistant"});
        let start = producer
            .observe("message_start", &assistant, "a")
            .expect("start");
        producer.reset("e1");
        assert_eq!(
            producer
                .observe("message_update", &assistant, "a")
                .map(|c| c.message_id),
            Some(start.message_id)
        );
        producer.reset("e2");
        assert!(
            producer
                .observe("message_update", &assistant, "a")
                .is_none()
        );
    }

    #[test]
    fn hint_records_the_durable_entry() {
        let carrier = RecoveryCarrier {
            message_id: "m".into(),
            epoch: "e".into(),
            durable_entry_id: None,
        };
        assert_eq!(
            hint(Some(carrier), Some("entry"))
                .and_then(|c| c.durable_entry_id)
                .as_deref(),
            Some("entry")
        );
        assert!(hint(None, Some("entry")).is_none());
    }
}
