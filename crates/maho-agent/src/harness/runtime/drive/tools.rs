use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::{ToolBatch, ToolCall};
use crate::types::AgentToolResult;
use maho_ai::types::{ContentBlock, ToolCall as AgentToolCall, ToolResultMessage};

pub struct ToolOutcome {
    pub tool_call: AgentToolCall,
    pub message: ToolResultMessage,
    pub terminate: bool,
}

pub const INTERRUPTION_MARKER: &str = "[Tool execution was interrupted. The preceding output is the latest durable progress snapshot; newer live output may be missing, and the external outcome is unknown.]";

pub fn find_call(
    batch: &ToolBatch,
    source_index: usize,
    result_entry_id: &str,
) -> Option<ToolCall> {
    batch
        .calls
        .iter()
        .find(|call| {
            call.source_index() == source_index && call.result_entry_id() == result_entry_id
        })
        .cloned()
}

pub fn replace_call(batch: &ToolBatch, replacement: ToolCall) -> ToolBatch {
    let mut next = batch.clone();
    for call in &mut next.calls {
        if call.source_index() == replacement.source_index()
            && call.result_entry_id() == replacement.result_entry_id()
        {
            *call = replacement.clone();
        }
    }
    next
}

pub fn validate_memo_name(name: &str) -> Result<(), SessionError> {
    if name.is_empty() {
        return Err(session_invariant_error(
            "Tool invocation memo name must not be empty",
        ));
    }
    if name.contains(':') {
        return Err(session_invariant_error(
            "Tool invocation memo name must not contain ':'",
        ));
    }
    Ok(())
}

fn synthetic_message(
    call: &AgentToolCall,
    content: Vec<ContentBlock>,
    checkpoint: Option<&AgentToolResult>,
    timestamp: i64,
) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content,
        details: checkpoint.map(|result| result.details.clone()),
        usage: checkpoint.and_then(|result| result.usage),
        added_tool_names: None,
        is_error: true,
        timestamp,
    }
}

pub fn aborted_outcome(call: AgentToolCall, timestamp: i64) -> ToolOutcome {
    let message = synthetic_message(
        &call,
        vec![ContentBlock::text(
            "Tool execution was cancelled before completion.",
        )],
        None,
        timestamp,
    );
    ToolOutcome {
        tool_call: call,
        message,
        terminate: false,
    }
}

pub fn interrupted_outcome(
    call: AgentToolCall,
    checkpoint: Option<&AgentToolResult>,
    timestamp: i64,
) -> ToolOutcome {
    let mut content = checkpoint
        .map(|result| result.content.clone())
        .unwrap_or_default();
    content.push(ContentBlock::text(INTERRUPTION_MARKER));
    let message = synthetic_message(&call, content, checkpoint, timestamp);
    ToolOutcome {
        tool_call: call,
        message,
        terminate: false,
    }
}

pub fn truncated_outcome(call: AgentToolCall, timestamp: i64) -> ToolOutcome {
    let text = format!(
        "Tool call {} was not executed because the assistant response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
        serde_json::json!(call.name)
    );
    let message = synthetic_message(&call, vec![ContentBlock::text(text)], None, timestamp);
    ToolOutcome {
        tool_call: call,
        message,
        terminate: false,
    }
}

pub async fn publish_tool_intent(
    lane: &crate::harness::runtime::lane::Lane,
    call: ToolCall,
    args: serde_json::Value,
    replay: crate::harness::session::types::ToolCallReplay,
    context: &crate::harness::context::Context,
) -> Result<crate::harness::runtime::types::ContinueOperationResult<ToolCall>, SessionError> {
    lane.continue_operation(
        move |_, current, meta, _| {
            Box::pin(async move {
                let crate::harness::session::types::OperationState::Tools(mut run) = current else {
                    return Err(session_invariant_error("Expected tools operation"));
                };
                let pending = ToolCall::EffectPending {
                    source_index: call.source_index(),
                    result_entry_id: call.result_entry_id().into(),
                    replay,
                };
                let write = crate::harness::session::types::Write::Value(
                    crate::harness::session::values::set_value(
                        &crate::harness::session::values::operation_tool_args(
                            &meta.operation_id,
                            &run.batch.turn_id,
                            call.source_index(),
                        ),
                        args,
                    ),
                );
                run.batch = replace_call(&run.batch, pending.clone());
                Ok(crate::harness::runtime::lane::OperationCommand::Commit {
                    decision: crate::harness::runtime::lane::CommitDecision {
                        writes: vec![write],
                        materialize: std::sync::Arc::new(move |_| pending.clone()),
                        events: None,
                    },
                    operation_state: Box::new(
                        crate::harness::session::types::OperationState::Tools(run),
                    ),
                    lane: None,
                })
            })
        },
        context,
    )
    .await
}

pub async fn publish_tool_outcome(
    lane: &crate::harness::runtime::lane::Lane,
    call: ToolCall,
    finalized: ToolOutcome,
    context: &crate::harness::context::Context,
) -> Result<(), SessionError> {
    let context = context.clone();
    let read_context = context.clone();
    lane.settle_operation(
        move |_, current, meta, reader| {
            Box::pin(async move {
                let crate::harness::session::types::OperationState::Tools(mut run) = current else {
                    return Err(session_invariant_error("Expected tools operation"));
                };
                let memos = reader
                    .scan_values(
                        &crate::harness::session::values::operation_tool_memo_prefix(
                            &meta.operation_id,
                            Some(call.result_entry_id()),
                        ),
                        &read_context,
                    )
                    .await?;
                let terminate = matches!(
                    run.operation.control,
                    crate::harness::session::types::Control::Running
                ) && finalized.terminate;
                let pending = crate::harness::session::types::PendingEntry::Message {
                    payload: crate::types::AgentMessage::Llm(maho_ai::types::Message::ToolResult(
                        finalized.message,
                    )),
                };
                let mut writes = vec![
                    crate::harness::session::types::Write::Value(
                        crate::harness::session::values::set_value(
                            &crate::harness::session::values::pending_entry(call.result_entry_id()),
                            crate::harness::runtime::lane::encoded(&pending)?,
                        ),
                    ),
                    crate::harness::session::types::Write::Value(
                        crate::harness::session::values::delete_value(
                            &crate::harness::session::values::pending_tool_output(
                                &meta.operation_id,
                                call.result_entry_id(),
                            ),
                        ),
                    ),
                ];
                writes.extend(memos.into_iter().map(|memo| {
                    crate::harness::session::types::Write::Value(
                        crate::harness::session::values::delete_value(&memo.address),
                    )
                }));
                run.batch = replace_call(
                    &run.batch,
                    ToolCall::OutcomeReady {
                        source_index: call.source_index(),
                        result_entry_id: call.result_entry_id().into(),
                        terminate,
                    },
                );
                Ok(crate::harness::runtime::lane::OperationCommand::Commit {
                    decision: crate::harness::runtime::lane::CommitDecision {
                        writes,
                        materialize: std::sync::Arc::new(|_| ()),
                        events: None,
                    },
                    operation_state: Box::new(
                        crate::harness::session::types::OperationState::Tools(run),
                    ),
                    lane: None,
                })
            })
        },
        &context,
    )
    .await
}
