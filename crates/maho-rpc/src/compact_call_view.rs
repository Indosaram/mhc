//! senpi `modes/rpc/compact-call-view.ts`: whole-view assembly of the calls at a cut; a
//! streaming call's raw prefix is read through the active message.

use crate::compact_call_model::{
    CallCorrelation, CallCounters, CallInput, CallJoin, CallOutcome, CallPhase, CallRaw,
    FrozenCalls, OutstandingCall,
};
use crate::compact_component_roots::CompactComponentRoots;
use crate::compact_content_store::CompactContentStore;
use crate::compact_read_lease::CompactError;
use crate::compact_session_calls::CompactSessionCalls;
use crate::compact_state_view::MessageRoot;
use crate::compact_text_view::{FrozenBlock, read_compact_blocks};
use crate::compact_values::{CompactValues, ValueRef};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct CallView {
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
    pub input: CallInput,
    pub correlation: Option<CallCorrelation>,
    pub outcome: Option<CallOutcome>,
    pub call_unknown: bool,
    pub raw: Option<String>,
    pub hook: Value,
    pub progress: Value,
    pub result: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallsView {
    pub sequence: u64,
    pub calls: Vec<CallView>,
    pub counters: CallCounters,
}

pub struct CallsViewOwner<'a> {
    pub calls: &'a CompactSessionCalls,
    pub store: &'a CompactContentStore,
    pub block_roots: &'a CompactComponentRoots<FrozenBlock>,
    pub message_roots: &'a CompactComponentRoots<MessageRoot>,
}

async fn read_slot(
    values: &CompactValues,
    reference: Option<&ValueRef>,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
) -> Result<Value, CompactError> {
    let Some(reference) = reference else {
        return Ok(Value::Null);
    };
    let value = values.read(reference).await?;
    valid()?;
    Ok(value)
}

async fn read_raw(
    owner: &CallsViewOwner<'_>,
    active: Option<&ValueRef>,
    address: Option<&CallRaw>,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
) -> Result<Option<String>, CompactError> {
    let Some(address) = address else {
        return Ok(None);
    };
    match address {
        CallRaw::Owned { root, tail_root } => {
            let mut text = owner.store.text().chunks(*root, valid).await?.concat();
            text.push_str(&owner.store.text().chunks(*tail_root, valid).await?.concat());
            Ok(Some(text))
        }
        CallRaw::Active {
            message_id,
            content_index,
        } => {
            for (_, row) in owner.message_roots.entries(active, valid).await? {
                if row.message_id != *message_id {
                    continue;
                }
                for (_, block) in owner
                    .block_roots
                    .entries(row.blocks.as_ref(), valid)
                    .await?
                {
                    if block.content_index == *content_index {
                        let views =
                            read_compact_blocks(&owner.store.text(), &[block], valid).await?;
                        return Ok(views.into_iter().next().map(|v| v.text));
                    }
                }
            }
            Ok(None)
        }
    }
}

/// Whole-view assembly of the calls at a cut; a streaming call's raw prefix is read through the
/// active message.
pub async fn read_calls(
    frozen_calls: &FrozenCalls,
    active: Option<&ValueRef>,
    owner: &CallsViewOwner<'_>,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
) -> Result<CallsView, CompactError> {
    let values = CompactValues::new(owner.store.clone());
    let mut calls = Vec::new();
    for (_, call) in owner
        .calls
        .entries(frozen_calls.reference.as_ref(), valid)
        .await?
    {
        let OutstandingCall {
            runtime_item_key,
            turn_id,
            tool_call_id,
            owning_message_id,
            content_index,
            parent_key,
            phase,
            join,
            start_sequence,
            started_at_ms,
            requested_name,
            resolved_name,
            seed,
            raw,
            input,
            correlation,
            hook,
            progress,
            result,
            outcome,
            call_unknown,
        } = call;
        calls.push(CallView {
            runtime_item_key,
            turn_id,
            tool_call_id,
            owning_message_id,
            content_index,
            parent_key,
            phase,
            join,
            start_sequence,
            started_at_ms,
            requested_name,
            resolved_name,
            seed,
            input,
            correlation,
            outcome,
            call_unknown,
            raw: read_raw(owner, active, raw.as_ref(), valid).await?,
            hook: read_slot(&values, hook.as_ref(), valid).await?,
            progress: read_slot(&values, progress.as_ref(), valid).await?,
            result: read_slot(&values, result.as_ref(), valid).await?,
        });
    }
    valid()?;
    Ok(CallsView {
        sequence: frozen_calls.sequence,
        calls,
        counters: frozen_calls.counters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_call_model::CallScope;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_session_calls::{CallStep, ExecutionStart};
    use serde_json::json;

    #[tokio::test]
    async fn assembles_calls_view_with_slots_and_owned_raw() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let block_roots = CompactComponentRoots::new(store.clone(), |b: &FrozenBlock| {
            vec![b.root, b.tail_root.unwrap_or(0)]
        });
        let message_roots = CompactComponentRoots::new(store.clone(), |m: &MessageRoot| {
            vec![m.blocks.as_ref().map_or(0, |b| b.owner)]
        });
        let mut calls = CompactSessionCalls::new(store.clone());
        let step = CallStep {
            sequence: 1,
            scope: CallScope {
                session_epoch: "E".into(),
                turn_id: Some("T".into()),
            },
        };
        calls
            .execution_start(
                &step,
                ExecutionStart {
                    tool_call_id: "tc1".into(),
                    tool_name: "read".into(),
                    args: json!({"path": "a"}),
                    parent_tool_call_id: None,
                    key: None,
                },
            )
            .expect("start");
        calls
            .execution_update(
                &CallStep {
                    sequence: 2,
                    scope: step.scope.clone(),
                },
                "tc1",
                &json!({"path": "a"}),
                &json!({"chunk": "hello"}),
            )
            .expect("update");
        let frozen = calls.freeze().expect("freeze");
        let ok = || -> Result<(), CompactError> { Ok(()) };
        let view = read_calls(
            &frozen,
            None,
            &CallsViewOwner {
                calls: &calls,
                store: &store,
                block_roots: &block_roots,
                message_roots: &message_roots,
            },
            &ok,
        )
        .await
        .expect("read");
        assert_eq!(view.calls.len(), 1);
        assert_eq!(view.calls[0].progress, json!({"chunk": "hello"}));
        calls.release(&frozen).expect("release");
        store.retire().await.expect("retire");
    }
}
