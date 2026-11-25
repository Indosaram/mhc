//! senpi `modes/rpc/compact-call-model.ts`: data model for outstanding calls, projections,
//! counters, identity stamps, and tree ordinals.

use crate::compact_content_store::LeafId;
use crate::compact_values::ValueRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallPhase {
    Streaming,
    AwaitingExecution,
    Executing,
    CompletedAwaitingEntry,
}

pub const TERMINAL_PHASE: CallPhase = CallPhase::CompletedAwaitingEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallJoin {
    Pending,
    Carrierless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOutcomeSource {
    ExecutionEnd,
    ResultMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CallOutcome {
    #[serde(rename_all = "camelCase")]
    Result {
        is_error: bool,
        source: CallOutcomeSource,
    },
    NotExecuted,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CallRaw {
    Active {
        message_id: String,
        content_index: usize,
    },
    Owned {
        root: LeafId,
        tail_root: LeafId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallInput {
    pub announced: Value,
    pub started: Value,
    pub prepared: Value,
}

impl Default for CallInput {
    fn default() -> Self {
        Self {
            announced: Value::Null,
            started: Value::Null,
            prepared: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallCorrelation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_turn_id: Option<Value>,
}

/// One outstanding call as the calls root stores it (R3.C3.2): a JSON-normal header plus refs to
/// owned slots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutstandingCall {
    pub runtime_item_key: String,
    pub turn_id: String,
    pub tool_call_id: Option<String>,
    pub owning_message_id: Option<String>,
    pub content_index: Option<usize>,
    pub parent_key: Option<String>,
    pub phase: CallPhase,
    pub join: CallJoin,
    pub start_sequence: Option<u64>,
    pub started_at_ms: Option<i64>,
    pub requested_name: Option<String>,
    pub resolved_name: Option<String>,
    pub seed: Value,
    pub raw: Option<CallRaw>,
    pub input: CallInput,
    pub correlation: Option<CallCorrelation>,
    pub hook: Option<ValueRef>,
    pub progress: Option<ValueRef>,
    pub result: Option<ValueRef>,
    pub outcome: Option<CallOutcome>,
    /// #5348 (RULING-123 C3.4): present while another call of this call's own message holds the
    /// same `toolCallId`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub call_unknown: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedCalls {
    pub calls: Vec<CallProjection>,
    pub complete: bool,
    pub omitted: usize,
}

/// The persisted form a settled toolResult row carries once its call retired (R3.C3.3, RULING-13
/// Q2/Q5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallProjection {
    pub runtime_item_key: String,
    pub turn_id: String,
    pub tool_call_id: Option<String>,
    pub owning_message_id: Option<String>,
    pub content_index: Option<usize>,
    pub parent_key: Option<String>,
    pub requested_name: Option<String>,
    pub resolved_name: Option<String>,
    pub seed: Value,
    pub input: CallInput,
    pub correlation: Option<CallCorrelation>,
    pub start_sequence: Option<u64>,
    pub started_at_ms: Option<i64>,
    pub outcome: Option<CallOutcome>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub call_unknown: bool,
    pub nested_calls: NestedCalls,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredCounters {
    pub unknown: u64,
    pub late: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallCounters {
    pub live: usize,
    pub carrierless: usize,
    pub replacements: u64,
    pub header_bytes: usize,
    pub slot_bytes: usize,
    pub progress_replacements: u64,
    pub ignored: IgnoredCounters,
    pub collisions: u64,
    pub reused: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrozenCalls {
    pub sequence: u64,
    pub reference: Option<ValueRef>,
    pub counters: CallCounters,
}

/// The lane facts a call transition consumes: the key epoch (RULING-13 Q1) and the lane-minted
/// turn (Q6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallScope {
    pub session_epoch: String,
    pub turn_id: Option<String>,
}

/// Execution, hook and boundary records the lane folds into the calls component.
pub const CALL_RECORDS: [&str; 7] = [
    "tool_execution_start",
    "tool_execution_update",
    "tool_execution_end",
    "tool_hook_status",
    "turn_end",
    "agent_settled",
    "message_end",
];

/// Records whose only lane effect is a call transition: ignored, they take no sequence (RULING-13
/// Q9).
pub const CALL_ONLY_RECORDS: [&str; 4] = [
    "tool_execution_update",
    "tool_execution_end",
    "tool_hook_status",
    "agent_settled",
];

/// Builds a fresh `OutstandingCall` initialized from `FRESH_CALL` defaults.
pub fn fresh_call(runtime_item_key: String, turn_id: String, phase: CallPhase) -> OutstandingCall {
    OutstandingCall {
        runtime_item_key,
        turn_id,
        tool_call_id: None,
        owning_message_id: None,
        content_index: None,
        parent_key: None,
        phase,
        join: CallJoin::Pending,
        start_sequence: None,
        started_at_ms: None,
        requested_name: None,
        resolved_name: None,
        seed: Value::Null,
        raw: None,
        input: CallInput::default(),
        correlation: None,
        hook: None,
        progress: None,
        result: None,
        outcome: None,
        call_unknown: false,
    }
}

pub fn runtime_item_key(scope: &CallScope, creation_sequence: u64, k: usize) -> String {
    format!("{}:{creation_sequence}.{k}", scope.session_epoch)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CallIdentity {
    #[serde(rename_all = "camelCase")]
    RuntimeItemKey { runtime_item_key: String },
    #[serde(rename_all = "camelCase")]
    CallUnknown { call_unknown: bool },
}

pub const CALL_UNKNOWN: CallIdentity = CallIdentity::CallUnknown { call_unknown: true };

pub fn call_identity(call: Option<&OutstandingCall>) -> CallIdentity {
    match call {
        Some(call) => CallIdentity::RuntimeItemKey {
            runtime_item_key: call.runtime_item_key.clone(),
        },
        None => CALL_UNKNOWN,
    }
}

/// Fixed-width tree ordinal so the root orders calls by creation, independent of the epoch string.
pub fn call_order(creation_sequence: u64, k: usize) -> String {
    format!("{creation_sequence:016}.{k:04}")
}

pub fn call_leaves(call: &OutstandingCall) -> Vec<LeafId> {
    let mut leaves = vec![
        call.hook.as_ref().map_or(0, |r| r.owner),
        call.progress.as_ref().map_or(0, |r| r.owner),
        call.result.as_ref().map_or(0, |r| r.owner),
    ];
    if let Some(CallRaw::Owned { root, tail_root }) = call.raw {
        leaves.push(root);
        leaves.push(tail_root);
    }
    leaves
}

pub fn terminal(call: &OutstandingCall) -> bool {
    call.phase == TERMINAL_PHASE
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_call_identity_and_order_per_spec() {
        assert_eq!(call_order(42, 3), "0000000000000042.0003");
        let scope = CallScope {
            session_epoch: "ep".into(),
            turn_id: Some("t1".into()),
        };
        assert_eq!(runtime_item_key(&scope, 7, 1), "ep:7.1");
        assert_eq!(
            serde_json::to_value(CALL_UNKNOWN).expect("json"),
            json!({"callUnknown": true})
        );
        let call = fresh_call("k1".into(), "t1".into(), CallPhase::Streaming);
        assert_eq!(
            serde_json::to_value(call_identity(Some(&call))).expect("json"),
            json!({"runtimeItemKey": "k1"})
        );
    }
}
