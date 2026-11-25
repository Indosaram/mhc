//! senpi `modes/rpc/compact-call-fold.ts`: folds seeded block transitions, terminal messages,
//! and execution/hook records into `CompactSessionCalls`.

use crate::compact_active_block::ActiveBlock;
use crate::compact_call_identity::is_call_named_record;
use crate::compact_call_model::{
    CALL_UNKNOWN, CallCorrelation, CallIdentity, CallProjection, CallRaw, CallScope,
    OutstandingCall, call_identity,
};
use crate::compact_read_lease::CompactError;
use crate::compact_seeded_block::SeededBlockEvent;
use crate::compact_seeded_messages::SeededMessage;
use crate::compact_session_calls::{AnnouncedBlock, CallStep, CompactSessionCalls, ExecutionStart};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct CallFold {
    pub changed: bool,
    pub projection: Option<CallProjection>,
    /// An execution or hook record's call (RULING-123): the one it changed, or `callUnknown` when
    /// it changed none.
    pub identity: Option<CallIdentity>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeededBlockTransition {
    pub message_id: String,
    pub event: SeededBlockEvent,
    pub correlation: Option<Value>,
    pub resolved_tool_name: Option<String>,
}

fn owned_raw(block: Option<&ActiveBlock>) -> Option<CallRaw> {
    block.map(|b| CallRaw::Owned {
        root: b.root,
        tail_root: b.tail_root,
    })
}

fn text_field(record: &Value, key: &str) -> String {
    match record.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn correlation_of(correlation: Option<&Value>) -> Option<CallCorrelation> {
    let obj = correlation?.as_object()?;
    Some(CallCorrelation {
        client_message_id: obj.get("clientMessageId").cloned(),
        client_turn_id: obj.get("clientTurnId").cloned(),
    })
}

/// The announcement side: the seeded block records and the terminal message of one assistant
/// stream.
pub fn fold_call_block(
    calls: &mut CompactSessionCalls,
    t: &SeededBlockTransition,
    step: &CallStep,
    block: Option<&ActiveBlock>,
) -> Result<(), CompactError> {
    let correlation = correlation_of(t.correlation.as_ref());
    match &t.event {
        SeededBlockEvent::ToolcallStart {
            content_index,
            id,
            tool_name,
            seed,
        } => calls.tool_start(
            step,
            AnnouncedBlock {
                message_id: t.message_id.clone(),
                content_index: *content_index,
                id: id.clone(),
                name: tool_name.clone(),
                arguments: seed.block.get("arguments").cloned().unwrap_or(Value::Null),
                resolved_name: t.resolved_tool_name.clone(),
                correlation,
            },
        ),
        SeededBlockEvent::ToolcallEnd {
            content_index,
            tool_call,
        } => calls.announce(
            step,
            AnnouncedBlock {
                message_id: t.message_id.clone(),
                content_index: *content_index,
                id: text_field(tool_call, "id"),
                name: text_field(tool_call, "name"),
                arguments: tool_call.get("arguments").cloned().unwrap_or(Value::Null),
                resolved_name: t.resolved_tool_name.clone(),
                correlation: None,
            },
            owned_raw(block),
        ),
        SeededBlockEvent::MessageBlockMetadata {
            content_index,
            extra_fields,
            ..
        } => {
            if let Some(Value::String(name)) = extra_fields.get("name") {
                calls.requested_name(step, &t.message_id, *content_index, name.clone())?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The terminal payload is authoritative for every `toolCall` block; a block no event announced
/// gets its call here.
pub fn fold_call_terminal(
    calls: &mut CompactSessionCalls,
    message_id: &str,
    message: &SeededMessage,
    step: &CallStep,
    live: Option<&BTreeMap<usize, ActiveBlock>>,
) -> Result<(), CompactError> {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Ok(());
    }
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return Ok(());
    };
    for (content_index, block) in content.iter().enumerate() {
        if block.get("type").and_then(Value::as_str) == Some("toolCall") {
            calls.announce(
                step,
                AnnouncedBlock {
                    message_id: message_id.to_owned(),
                    content_index,
                    id: text_field(block, "id"),
                    name: text_field(block, "name"),
                    arguments: block.get("arguments").cloned().unwrap_or(Value::Null),
                    resolved_name: None,
                    correlation: None,
                },
                owned_raw(live.and_then(|map| map.get(&content_index))),
            )?;
        }
    }
    Ok(())
}

/// The execution side: a JSON-normal legacy record folded at one lane step (RULING-13 Q9 reports
/// `changed`).
pub fn fold_call_record(
    calls: &mut CompactSessionCalls,
    record: &Value,
    scope: CallScope,
    sequence: u64,
    call_key: Option<String>,
) -> Result<CallFold, CompactError> {
    let step = CallStep { sequence, scope };
    let before = calls.table.counters.replacements;
    let mut projection: Option<CallProjection> = None;
    let mut call: Option<OutstandingCall> = None;
    let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "tool_execution_start" => {
            call = calls.execution_start(
                &step,
                ExecutionStart {
                    tool_call_id: text_field(record, "toolCallId"),
                    tool_name: text_field(record, "toolName"),
                    args: record.get("args").cloned().unwrap_or(Value::Null),
                    parent_tool_call_id: record
                        .get("parentToolCallId")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    key: call_key,
                },
            )?;
        }
        "tool_execution_update" => {
            call = calls.execution_update(
                &step,
                &text_field(record, "toolCallId"),
                record.get("args").unwrap_or(&Value::Null),
                record.get("partialResult").unwrap_or(&Value::Null),
            )?;
        }
        "tool_execution_end" => {
            call = calls.execution_end(
                &step,
                &text_field(record, "toolCallId"),
                record.get("result").unwrap_or(&Value::Null),
                record.get("isError").and_then(Value::as_bool) == Some(true),
            )?;
        }
        "tool_hook_status" => {
            call = calls.hook(&step, &text_field(record, "toolCallId"), record)?;
        }
        "turn_end" => calls.turn_end(&step)?,
        "agent_settled" => calls.agent_settled(&step)?,
        "message_end" => {
            if let Some(message) = record.get("message")
                && message.get("role").and_then(Value::as_str) == Some("toolResult")
            {
                projection = calls.settle(
                    &step,
                    &text_field(message, "toolCallId"),
                    message.get("isError").and_then(Value::as_bool) == Some(true),
                )?;
            }
        }
        _ => {}
    }
    let named = is_call_named_record(kind);
    let identity = if named {
        let id = match call.as_ref() {
            Some(c) if calls.table.shares_id_in_message(c) => CALL_UNKNOWN,
            other => call_identity(other),
        };
        Some(id)
    } else {
        None
    };
    Ok(CallFold {
        changed: calls.table.counters.replacements != before,
        projection,
        identity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::{CompactContentStore, CompactContentStoreOptions};
    use serde_json::json;

    #[tokio::test]
    async fn folds_execution_records_and_reports_unknown_for_unmatched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let mut calls = CompactSessionCalls::new(store.clone());
        let scope = CallScope {
            session_epoch: "E".into(),
            turn_id: Some("T".into()),
        };
        let ignored = fold_call_record(
            &mut calls,
            &json!({"type":"tool_execution_end","toolCallId":"missing","result":null,"isError":false}),
            scope.clone(),
            1,
            None,
        )
        .expect("fold");
        assert!(!ignored.changed);
        assert_eq!(ignored.identity, Some(CALL_UNKNOWN));

        let started = fold_call_record(
            &mut calls,
            &json!({"type":"tool_execution_start","toolCallId":"tc1","toolName":"read","args":{}}),
            scope,
            2,
            None,
        )
        .expect("start");
        assert!(started.changed);
        assert_eq!(
            started.identity,
            Some(CallIdentity::RuntimeItemKey {
                runtime_item_key: "E:2.0".into()
            })
        );
        store.retire().await.expect("retire");
    }
}
