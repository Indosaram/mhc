//! Port of senpi `packages/agent/src/harness/runtime/drive/terminal.ts`.

use std::collections::BTreeSet;

use crate::harness::context::Context;
use crate::harness::session::session::{SessionError, SessionErrorKind, SessionInvariantError};
use crate::harness::session::types::{
    OperationError, OperationMeta, OperationResultRecord, OperationState, SessionReader, TerminalStatus, ToolCall, Write,
};
use crate::harness::session::values::{
    ListWrite, delete_list, delete_value, operation_meta, operation_preparation_prefix, operation_state,
    operation_tool_args_prefix, operation_tool_memo_prefix, pending_assistant_frames, pending_entry,
    pending_tool_output_prefix,
};

pub async fn operation_cleanup_writes(
    reader: &dyn SessionReader,
    operation_id: &str,
    state: &OperationState,
    context: &Context,
) -> Result<Vec<Write>, SessionError> {
    let tool_arguments = reader.scan_values(&operation_tool_args_prefix(operation_id, None), context).await?;
    let tool_memos = reader.scan_values(&operation_tool_memo_prefix(operation_id, None), context).await?;
    let preparations = reader.scan_values(&operation_preparation_prefix(operation_id), context).await?;
    let tool_outputs = reader.scan_values(&pending_tool_output_prefix(operation_id), context).await?;

    let mut pending_ids: BTreeSet<String> = BTreeSet::new();
    if let OperationState::Tools(tools) = state {
        for call in &tools.batch.calls {
            if matches!(call, ToolCall::OutcomeReady { .. }) {
                pending_ids.insert(call.result_entry_id().to_owned());
            }
        }
    }

    let frame_delete: Option<ListWrite> = match state {
        OperationState::AssistantEffectPending(pending) => {
            Some(delete_list(&pending_assistant_frames(operation_id, &pending.response_entry_id)))
        }
        OperationState::DeferredEffectPending(pending) => {
            Some(delete_list(&pending_assistant_frames(operation_id, &pending.response_entry_id)))
        }
        _ => None,
    };

    let mut writes = vec![
        Write::Value(delete_value(&operation_meta(operation_id))),
        Write::Value(delete_value(&operation_state(operation_id))),
    ];
    writes.extend(tool_arguments.iter().map(|stored| Write::Value(delete_value(&stored.address))));
    writes.extend(tool_memos.iter().map(|stored| Write::Value(delete_value(&stored.address))));
    writes.extend(preparations.iter().map(|stored| Write::Value(delete_value(&stored.address))));
    writes.extend(tool_outputs.iter().map(|stored| Write::Value(delete_value(&stored.address))));
    if let Some(frame_delete) = frame_delete {
        writes.push(Write::List(frame_delete));
    }
    writes.extend(pending_ids.iter().map(|id| Write::Value(delete_value(&pending_entry(id)))));
    Ok(writes)
}

pub fn operation_result_record(
    meta: &OperationMeta,
    status: TerminalStatus,
    tip_id: Option<String>,
    error: Option<OperationError>,
) -> Result<OperationResultRecord, SessionInvariantError> {
    if (status == TerminalStatus::Failed) != error.is_some() {
        return Err(SessionInvariantError::new(
            SessionErrorKind::Invariant,
            "Only a failed operation result may carry an error",
        ));
    }
    Ok(OperationResultRecord {
        operation_id: meta.operation_id.clone(),
        kind: meta.intent.kind(),
        status,
        error,
        from_tip_id: meta.source_tip_id.clone(),
        tip_id,
        started_at: meta.started_at,
        ended_at: now_ms(),
    })
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_millis() as i64)
}
