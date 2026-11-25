//! senpi `modes/rpc/session-acceptance-compact.ts`: folds accepted transcript transitions
//! and accepted records into compact settled and current state.

use crate::compact_current_components::is_component_record;
use crate::compact_lane_current::{CallFold, CompactLaneCurrent, MessagesLookupFn};
use crate::compact_lane_transitions::CanonicalUnit;
use crate::compact_message_identity::{CallIdentity, LiveIdentity, SettledEnd, TerminalIdentity};
use crate::compact_settled_messages::CompactSettledMessages;
use crate::compact_transcript_flows::TranscriptFlow;
use crate::compact_transition::CompactPersistence;
use maho_core::session_recovery_identity::RecoveryCarrier;
use maho_core::session_transcript_journal::TranscriptTransition;
use serde_json::Value;

/// Adapter bridging `CompactSettledMessages` (or test/future settled stores) with the full S14
/// return values (`SettledEnd`, `Option<LiveIdentity>`, `Option<String>`).
pub trait CompactSettledAdapter {
    fn valid(&self, carrier: Option<&RecoveryCarrier>) -> bool;
    fn append(&self, transition: &TranscriptTransition) -> Option<String>;
    fn end(
        &self,
        record: &Value,
        carrier: Option<&RecoveryCarrier>,
        sequence: u64,
        turn_id: Option<&str>,
        projection: Option<&Value>,
    ) -> Option<SettledEnd>;
    fn fail(&self, carrier: Option<&RecoveryCarrier>, error_message: &str) -> Option<LiveIdentity>;
}

impl CompactSettledAdapter for CompactSettledMessages {
    fn valid(&self, carrier: Option<&RecoveryCarrier>) -> bool {
        CompactSettledMessages::valid(self, carrier).is_some()
    }

    fn append(&self, transition: &TranscriptTransition) -> Option<String> {
        if CompactSettledMessages::append(self, transition).is_err() {
            return None;
        }
        match transition {
            TranscriptTransition::Append {
                owning_message_id, ..
            } => owning_message_id.clone(),
            _ => None,
        }
    }

    fn end(
        &self,
        record: &Value,
        carrier: Option<&RecoveryCarrier>,
        sequence: u64,
        turn_id: Option<&str>,
        _projection: Option<&Value>,
    ) -> Option<SettledEnd> {
        CompactSettledMessages::end(
            self,
            record,
            carrier,
            sequence,
            turn_id.map(ToOwned::to_owned),
        )
        .ok()?;
        let carrier = CompactSettledMessages::valid(self, carrier)?;
        Some(SettledEnd {
            joined: carrier.durable_entry_id.is_some(),
            identity: TerminalIdentity {
                message_id: carrier.message_id.clone(),
                terminal_sequence: sequence,
            },
        })
    }

    fn fail(&self, carrier: Option<&RecoveryCarrier>, error_message: &str) -> Option<LiveIdentity> {
        CompactSettledMessages::fail(self, carrier, error_message).ok()?;
        let carrier = CompactSettledMessages::valid(self, carrier)?;
        Some(LiveIdentity::Message {
            message_id: carrier.message_id.clone(),
        })
    }
}

/// The compact components one lane epoch feeds: its settled terminals and its current state.
pub type CallOwningFn<'a> = &'a dyn Fn(&str, usize) -> Option<String>;

pub struct CompactComponents<'a, S: CompactSettledAdapter = CompactSettledMessages> {
    pub settled: &'a S,
    pub current: &'a mut CompactLaneCurrent,
    pub flow: Option<&'a TranscriptFlow>,
    pub messages_of: Option<MessagesLookupFn>,
    pub call_owning: Option<CallOwningFn<'a>>,
}

/// An accepted record ready for compact folding.
#[derive(Debug, Clone)]
pub struct AcceptedRecord<'a> {
    pub record: &'a Value,
    pub record_type: &'a str,
    pub carrier: Option<&'a RecoveryCarrier>,
    pub canonical: Option<&'a CanonicalUnit>,
    pub sequence: u64,
    pub turn_id: Option<&'a str>,
    pub call: Option<&'a CallFold>,
}

/// Extracts `assistantMessageEvent.contentIndex` for streamed `toolcall_*` updates
/// (from `compact-call-identity.ts` `toolcallIndex`).
pub fn toolcall_index(record: &Value) -> Option<usize> {
    let event = record.get("assistantMessageEvent")?;
    let kind = event.get("type").and_then(Value::as_str)?;
    if !kind.starts_with("toolcall_") {
        return None;
    }
    event
        .get("contentIndex")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

/// One accepted transcript transition: its owned join, then the cold flow and persistence it carries.
/// Returns the message an append joined, the id its `join-<n>` member records.
pub fn fold_compact_transition<S: CompactSettledAdapter>(
    components: &mut CompactComponents<'_, S>,
    transition: &TranscriptTransition,
    transcript_available: bool,
    epoch: &str,
) -> Option<String> {
    let owner = if matches!(transition, TranscriptTransition::Append { .. }) && transcript_available
    {
        components.settled.append(transition)
    } else {
        None
    };
    components.current.transcript(
        transition,
        components.flow,
        components.messages_of.clone(),
        epoch,
    );
    owner
}

/// The compact side of one accepted record: canonical and component folds, its terminal, its
/// persistence. Returns the identity the settled store recorded for this record's message
/// (RULING-120), or the call a live call record belongs to (RULING-123).
pub fn fold_compact_record<S: CompactSettledAdapter>(
    components: &mut CompactComponents<'_, S>,
    accepted: &AcceptedRecord<'_>,
) -> Option<LiveIdentity> {
    if let Some(canonical) = accepted.canonical {
        components
            .current
            .fold(canonical, accepted.sequence, accepted.turn_id);
    }
    if is_component_record(accepted.record_type) {
        components
            .current
            .fold_component(accepted.record, accepted.sequence);
    }
    let ended = if accepted.record_type == "message_end" {
        components.settled.end(
            accepted.record,
            accepted.carrier,
            accepted.sequence,
            accepted.turn_id,
            accepted.call.and_then(|c| c.projection.as_ref()),
        )
    } else {
        None
    };
    let failed = if accepted.record_type == "transcript_write_failed" {
        let error_msg = accepted
            .record
            .get("errorMessage")
            .and_then(Value::as_str)
            .unwrap_or("undefined");
        components.settled.fail(accepted.carrier, error_msg)
    } else {
        None
    };

    let status = if accepted.record_type == "transcript_write_failed" {
        Some(CompactPersistence::Failed)
    } else if ended.as_ref().is_some_and(|e| e.joined) {
        Some(CompactPersistence::Appended)
    } else {
        None
    };
    if let (Some(persistence), Some(carrier)) = (status, accepted.carrier)
        && components.settled.valid(Some(carrier))
    {
        components.current.status(&carrier.message_id, persistence);
    }

    let streamed = toolcall_index(accepted.record)
        .and_then(|block| {
            let carrier = accepted.carrier?;
            let lookup = components.call_owning?;
            lookup(&carrier.message_id, block)
        })
        .map(|call_key| LiveIdentity::Call(CallIdentity { call_key }));

    ended
        .map(|e| LiveIdentity::Terminal(e.identity))
        .or(failed)
        .or_else(|| accepted.call.and_then(|c| c.identity.clone()))
        .or(streamed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use serde_json::json;

    #[test]
    fn toolcall_index_extracts_content_index_only_for_toolcall_events() {
        let delta = json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "toolcall_delta", "contentIndex": 2}
        });
        assert_eq!(toolcall_index(&delta), Some(2));

        let text = json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 1}
        });
        assert_eq!(toolcall_index(&text), None);
    }

    #[tokio::test]
    async fn fold_compact_record_returns_terminal_and_streamed_identities() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settled = CompactSettledMessages::new(
            "e1".into(),
            CompactContentStoreOptions {
                dir: dir.path().join("settled").display().to_string(),
                ..Default::default()
            },
            |_| {},
        );
        let mut current = CompactLaneCurrent::new();
        let owner_fn = |msg_id: &str, idx: usize| -> Option<String> {
            (msg_id == "m1" && idx == 0).then(|| "call-k".to_owned())
        };
        let mut comps = CompactComponents {
            settled: &settled,
            current: &mut current,
            flow: None,
            messages_of: None,
            call_owning: Some(&owner_fn),
        };
        let carrier = RecoveryCarrier {
            message_id: "m1".into(),
            epoch: "e1".into(),
            durable_entry_id: None,
        };
        let update = json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "toolcall_delta", "contentIndex": 0}
        });
        let id = fold_compact_record(
            &mut comps,
            &AcceptedRecord {
                record: &update,
                record_type: "message_update",
                carrier: Some(&carrier),
                canonical: None,
                sequence: 1,
                turn_id: Some("t1"),
                call: None,
            },
        );
        assert_eq!(
            id,
            Some(LiveIdentity::Call(CallIdentity {
                call_key: "call-k".into()
            }))
        );

        let end = json!({"type": "message_end", "message": {"role": "assistant", "content": "ok"}});
        let end_id = fold_compact_record(
            &mut comps,
            &AcceptedRecord {
                record: &end,
                record_type: "message_end",
                carrier: Some(&carrier),
                canonical: None,
                sequence: 2,
                turn_id: Some("t1"),
                call: None,
            },
        );
        assert_eq!(
            end_id,
            Some(LiveIdentity::Terminal(TerminalIdentity {
                message_id: "m1".into(),
                terminal_sequence: 2,
            }))
        );
    }
}
