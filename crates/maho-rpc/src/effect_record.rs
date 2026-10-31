//! The effect ledger's record and status shapes (senpi `rpc/effect-record.ts`).
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Custom transcript entry for every non-input effect; inputs keep `rpc_client_message_admission`.
pub const RPC_EFFECT_ENTRY: &str = "rpc_effect";
pub const EFFECT_LEDGER_CAPABILITY: &str = "effect_ledger";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectActor {
    pub connection_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectOutcomeKind {
    Completed,
    Aborted,
    Error,
    Deadline,
    InterruptedByReopen,
    DroppedByReplace,
    UnknownModel,
    ProviderUnavailable,
    UnsupportedThinkingLevel,
    Superseded,
    ContextBudget,
    Cleared,
    /// 2026.10.16-2 wrote an abort-drained input under this name; it must stay readable.
    ClearedByAbort,
    AlreadyDelivered,
    NotFound,
    Conflict,
    Refused,
}

/// How an effect ended. `completed` needs no words; every other kind carries `reason` and `detail`,
/// plus `action` whenever the caller can do something about it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectOutcome {
    pub kind: EffectOutcomeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aborted_by: Option<EffectActor>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedChangeAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Input,
    Abort,
    Unload,
    SetModel,
    SetThinkingLevel,
    Dequeue,
    EditQueued,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    Accepted,
    Applied,
    Done,
    Failed,
}

/// One `rpc_effect` entry's data. `kind` never holds `input` (inputs use the admission entry).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcEffectRecord {
    pub effect_id: String,
    pub kind: EffectKind,
    pub state: EffectState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<EffectOutcome>,
    /// What a queued-input change answered beyond its status, so a replay answers as the live call did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<QueuedChangeAnswer>,
    pub by: EffectActor,
    pub accepted_at: String,
    pub updated_at: String,
    pub durable_session_id: String,
    pub fingerprint: String,
}

/// senpi `rpcEffectRecordSchema.safeParse`: the serde shape plus the id bounds, the non-input kind and
/// positive revisions.
pub fn parse_rpc_effect_record(value: &serde_json::Value) -> Option<RpcEffectRecord> {
    let record: RpcEffectRecord = serde_json::from_value(value.clone()).ok()?;
    let id_len = record.effect_id.chars().count();
    let revisions_positive = record
        .answer
        .as_ref()
        .is_none_or(|answer| answer.revision != Some(0) && answer.current_revision != Some(0));
    (record.kind != EffectKind::Input && (1..=256).contains(&id_len) && revisions_positive)
        .then_some(record)
}

/// The one answer `get_effect_status` and every effect command's response carry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectStatus {
    pub effect_id: String,
    pub kind: EffectKind,
    pub state: EffectState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<EffectOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<QueuedChangeAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<EffectActor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// The routing handle the effect is being answered on; absent when read from disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub durable_session_id: String,
}

pub fn is_terminal_effect_state(state: EffectState) -> bool {
    matches!(state, EffectState::Done | EffectState::Failed)
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("effect_id_conflict: effectId {effect_id} was already used for a different request")]
pub struct EffectIdConflict {
    pub effect_id: String,
}
impl EffectIdConflict {
    pub const CODE: &'static str = "effect_id_conflict";
    pub const REASON: &'static str =
        "effect_id_conflict: this effectId was already used for a different request";
    pub const ACTION: &'static str = "send the request again with a new effectId";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectCommandKind {
    Abort,
    SetModel,
    SetThinkingLevel,
    Unload,
    Dequeue,
    EditQueued,
}
impl EffectCommandKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Abort => "abort",
            Self::SetModel => "set_model",
            Self::SetThinkingLevel => "set_thinking_level",
            Self::Unload => "unload",
            Self::Dequeue => "dequeue",
            Self::EditQueued => "edit_queued",
        }
    }
    /// The SEMANTIC fields of each kind. The request `id`, the `effectId`, the routing `sessionId` and every
    /// transport field are never hashed, so the same effect retried after a reopen still replays.
    fn semantic_fields(self) -> &'static [&'static str] {
        match self {
            Self::Abort => &["awaitIdle", "clearQueue", "graceMs"],
            Self::SetModel => &["provider", "modelId", "thinkingLevel"],
            Self::SetThinkingLevel => &["level", "scope"],
            Self::Unload => &["interrupt", "clearQueue", "children", "force"],
            Self::Dequeue => &["queuedId", "expectedRevision"],
            Self::EditQueued => &["queuedId", "text", "expectedRevision"],
        }
    }
}

/// `sha256(JSON.stringify({kind, ...semantic fields in table order}))`.
pub fn effect_fingerprint(command: EffectCommandKind, payload: &serde_json::Value) -> String {
    let mut semantic = serde_json::Map::new();
    semantic.insert("kind".into(), command.as_str().into());
    for field in command.semantic_fields() {
        if let Some(value) = payload.get(*field) {
            semantic.insert((*field).to_owned(), value.clone());
        }
    }
    let text = serde_json::Value::Object(semantic).to_string();
    hex::encode(Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fingerprint_hashes_only_semantic_fields_in_order() {
        let base = effect_fingerprint(
            EffectCommandKind::SetModel,
            &json!({"provider":"a","modelId":"m"}),
        );
        let noisy = effect_fingerprint(
            EffectCommandKind::SetModel,
            &json!({"id":"r","effectId":"e","sessionId":"s","modelId":"m","provider":"a"}),
        );
        assert_eq!(base, noisy);
        let expected = hex::encode(Sha256::digest(
            br#"{"kind":"set_model","provider":"a","modelId":"m"}"#,
        ));
        assert_eq!(base, expected);
        assert_ne!(
            base,
            effect_fingerprint(
                EffectCommandKind::SetModel,
                &json!({"provider":"a","modelId":"n"})
            )
        );
    }

    #[test]
    fn record_parse_rejects_inputs_empty_ids_and_zero_revisions() {
        let record = json!({"effectId":"e","kind":"abort","state":"done","by":{"connectionId":"c"},"acceptedAt":"t","updatedAt":"t","durableSessionId":"d","fingerprint":"f","outcome":{"kind":"cleared_by_abort"}});
        assert!(parse_rpc_effect_record(&record).is_some());
        let mut input = record.clone();
        input["kind"] = json!("input");
        assert!(parse_rpc_effect_record(&input).is_none());
        let mut empty = record.clone();
        empty["effectId"] = json!("");
        assert!(parse_rpc_effect_record(&empty).is_none());
        let mut zero = record;
        zero["answer"] = json!({"revision":0});
        assert!(parse_rpc_effect_record(&zero).is_none());
        assert!(
            is_terminal_effect_state(EffectState::Failed)
                && !is_terminal_effect_state(EffectState::Applied)
        );
    }
}
