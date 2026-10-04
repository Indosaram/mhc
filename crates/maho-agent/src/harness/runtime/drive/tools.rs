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

pub async fn read_checkpoint(lane: &crate::harness::runtime::lane::Lane, drive: &crate::harness::runtime::types::Drive, call: &ToolCall) -> Result<Option<AgentToolResult>, SessionError> {
    let key = crate::harness::session::values::pending_tool_output(&drive.operation_id, call.result_entry_id());
    let context = drive.context.clone();
    lane.command(move |_, reader| Box::pin(async move {
        let stored = reader.get_value(&key, &context).await?;
        let result = stored.map(|value| serde_json::from_value(value.value).map_err(|error| session_invariant_error(error.to_string()))).transpose()?;
        Ok(crate::harness::runtime::lane::LaneCommand::Return { result })
    }), &drive.context).await
}

pub async fn clear_replay_checkpoint(lane: &crate::harness::runtime::lane::Lane, drive: &crate::harness::runtime::types::Drive, batch: &ToolBatch, call: &ToolCall, tool_call: &AgentToolCall) -> Result<serde_json::Value, SessionError> {
    let args_key = crate::harness::session::values::operation_tool_args(&drive.operation_id, &batch.turn_id, call.source_index());
    let output_key = crate::harness::session::values::pending_tool_output(&drive.operation_id, call.result_entry_id());
    let result_entry_id = call.result_entry_id().to_owned();
    let context = drive.context.clone();
    let name = lane.name.clone();
    let event = crate::harness::events::HarnessEvent { lane: Some(name), recovery: Some(true), payload: crate::harness::events::HarnessEventPayload::ToolStart { run_id: drive.operation_id.clone(), turn_id: batch.turn_id.clone(), tool_call_id: tool_call.id.clone(), tool_name: tool_call.name.clone() } };
    lane.command(move |state, reader| Box::pin(async move {
        let stored = reader.get_value(&args_key, &context).await?.ok_or_else(|| session_invariant_error(format!("Tool call {result_entry_id} is missing persisted arguments")))?;
        Ok(crate::harness::runtime::lane::LaneCommand::Commit { decision: crate::harness::runtime::lane::CommitDecision { writes: vec![crate::harness::session::types::Write::Value(crate::harness::session::values::delete_value(&output_key))], materialize: std::sync::Arc::new(move |_| stored.value.clone()), events: Some(std::sync::Arc::new(move |_| vec![event.clone()])) }, next: Box::new(state) })
    }), &drive.context).await
}

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

// ---------------------------------------------------------------------------------------------
// Port of the remaining pinned `drive/tools.ts` orchestration: `runTools` and its helpers.
// ---------------------------------------------------------------------------------------------

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::harness::context::Context;
use crate::harness::events::{HarnessEvent, HarnessEventPayload};
use crate::harness::execution::effect_gate::GateRefusal;
use crate::harness::execution::tools::{
    apply_before_tool_decision, create_tool_result_message, execute_tool_call, finalize_tool_call,
    prepare_tool_call, ClearToolCallOutcome, ClearedToolCall, ExecutedToolCall, FinalizedToolCall,
    ImmediateToolOutcome, ToolCallPreparation,
};
use crate::harness::hooks::{HookInvocation, HookName, HookResult, HookRunError};
use crate::harness::runtime::drive::tool_placement::{
    materialize_ready, read_tool_batch_source, tool_call_for, ToolBatchSource,
};
use crate::harness::runtime::lane::{Lane, LaneCommand};
use crate::harness::runtime::progress::open_tool_progress;
use crate::harness::runtime::types::{ContinueOperationResult, Drive, ProcedureResult, RuntimeDriveLane};
use crate::harness::session::types::{
    Control, OperationState, ToolCall as DurableCall, ToolCallReplay, ToolExecutionMode,
    ToolsOperation, Write,
};
use crate::harness::session::values::{delete_value, operation_tool_memo, set_value};
use crate::harness::types::{
    AgentHarnessTool, AgentHarnessToolInvocation, AgentHarnessToolUpdateCallback,
    AgentHarnessToolUpdateOptions,
};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

/// Pinned `currentBatch`: the durable tools operation the lane currently owns, if any.
fn current_tools(lane: &Lane) -> Option<ToolsOperation> {
    let operation = lane.state().operation?;
    match operation.state {
        OperationState::Tools(run) => Some(run),
        _ => None,
    }
}

/// Pinned `ownsEffect`: the call is still the lane's `effect_pending` entry for its slot.
fn owns_effect(state: &crate::harness::runtime::types::LaneState, source_index: usize, result_entry_id: &str) -> bool {
    let Some(operation) = &state.operation else { return false; };
    let OperationState::Tools(run) = &operation.state else { return false; };
    run.batch.calls.iter().any(|call| {
        call.source_index() == source_index
            && call.result_entry_id() == result_entry_id
            && matches!(call, DurableCall::EffectPending { .. })
    })
}

fn outcome_from_finalized(finalized: FinalizedToolCall) -> ToolOutcome {
    ToolOutcome {
        message: create_tool_result_message(&finalized),
        tool_call: finalized.tool_call,
        terminate: finalized.terminate,
    }
}

fn outcome_from_immediate(outcome: ImmediateToolOutcome) -> ToolOutcome {
    outcome_from_finalized(FinalizedToolCall {
        tool_call: outcome.tool_call,
        result: outcome.result,
        is_error: outcome.is_error,
        terminate: outcome.terminate,
    })
}

/// Pinned `invocationCapability.invocation`: the durable memo/identity handle a running tool owns.
struct ToolInvocation {
    invocation_id: String,
    operation_id: String,
    turn_id: String,
    source_index: usize,
    result_entry_id: String,
    lane: Arc<Lane>,
    context: Context,
    active: Arc<AtomicBool>,
}

impl AgentHarnessToolInvocation for ToolInvocation {
    fn invocation_id(&self) -> &str {
        &self.invocation_id
    }
    fn operation_id(&self) -> &str {
        &self.operation_id
    }
    fn turn_id(&self) -> &str {
        &self.turn_id
    }
    fn get_memo<'a>(&'a self, name: &'a str) -> maho_ai::types::BoxFuture<'a, Option<serde_json::Value>> {
        Box::pin(async move {
            if validate_memo_name(name).is_err() || !self.active.load(Ordering::SeqCst) {
                return None;
            }
            let lane = self.lane.clone();
            let context = self.context.clone();
            let address = operation_tool_memo(&self.operation_id, &self.invocation_id, name);
            let source_index = self.source_index;
            let result_entry_id = self.result_entry_id.clone();
            lane.command(
                move |state, reader| {
                    Box::pin(async move {
                        if !owns_effect(&state, source_index, &result_entry_id) {
                            return Ok(LaneCommand::Reject { error: "Tool invocation no longer owns its durable effect".into() });
                        }
                        let stored = reader.get_value(&address, &context).await?;
                        Ok(LaneCommand::Return { result: stored.map(|stored| stored.value) })
                    })
                },
                &self.context,
            )
            .await
            .ok()
            .flatten()
        })
    }
    fn set_memo<'a>(&'a self, name: &'a str, value: Option<serde_json::Value>) -> maho_ai::types::BoxFuture<'a, ()> {
        Box::pin(async move {
            if validate_memo_name(name).is_err() || !self.active.load(Ordering::SeqCst) {
                return;
            }
            let lane = self.lane.clone();
            let address = operation_tool_memo(&self.operation_id, &self.invocation_id, name);
            let source_index = self.source_index;
            let result_entry_id = self.result_entry_id.clone();
            let _ = lane
                .command(
                    move |state, _reader| {
                        Box::pin(async move {
                            if !owns_effect(&state, source_index, &result_entry_id) {
                                return Ok(LaneCommand::Reject { error: "Tool invocation no longer owns its durable effect".into() });
                            }
                            let write = match &value {
                                Some(value) => set_value(&address, value.clone()),
                                None => delete_value(&address),
                            };
                            Ok(LaneCommand::Commit {
                                decision: crate::harness::runtime::lane::CommitDecision {
                                    writes: vec![Write::Value(write)],
                                    materialize: Arc::new(|_| ()),
                                    events: None,
                                },
                                next: Box::new(state),
                            })
                        })
                    },
                    &self.context,
                )
                .await;
        })
    }
}

fn invocation_capability(lane: &Arc<Lane>, drive: &Drive, batch: &ToolBatch, call: &DurableCall) -> (Arc<dyn AgentHarnessToolInvocation>, Arc<AtomicBool>) {
    let active = Arc::new(AtomicBool::new(true));
    let invocation = Arc::new(ToolInvocation {
        invocation_id: call.result_entry_id().to_owned(),
        operation_id: drive.operation_id.clone(),
        turn_id: batch.turn_id.clone(),
        source_index: call.source_index(),
        result_entry_id: call.result_entry_id().to_owned(),
        lane: lane.clone(),
        context: drive.context.clone(),
        active: active.clone(),
    });
    (invocation, active)
}

async fn run_after_tool_hook(
    lane: &Arc<Lane>,
    drive: &Drive,
    cleared: &ClearedToolCall<()>,
    executed: &ExecutedToolCall,
) -> Result<Option<crate::harness::execution::tools::AfterToolPatch>, HookRunError> {
    let mut invocation = HookInvocation::new(lane.name.clone(), drive.operation_id.clone());
    invocation.tool_call_id = Some(cleared.tool_call.id.clone());
    invocation.tool_name = Some(cleared.tool_call.name.clone());
    invocation.args = cleared.args.clone();
    invocation.content = executed.result.content.clone();
    invocation.details = Some(executed.result.details.clone());
    invocation.is_error = executed.is_error;
    invocation.usage = executed.result.usage;
    match lane.hooks.run_tool_with_gate(HookName::AfterTool, invocation, &drive.gate, &drive.context).await? {
        HookResult::AfterTool(patch) => Ok(Some(crate::harness::execution::tools::AfterToolPatch {
            content: patch.content,
            details: patch.details,
            is_error: patch.is_error,
            usage: patch.usage,
            terminate: patch.terminate,
        })),
        _ => Ok(None),
    }
}

/// Pinned `performToolInvocation`: run one cleared call through the effect gate, publishing progress.
async fn perform_tool_invocation(
    lane: &Arc<Lane>,
    drive: &Drive,
    batch: &ToolBatch,
    call: &DurableCall,
    cleared: ClearedToolCall<()>,
    tool_context: (),
    recovery: bool,
) -> Result<ToolOutcome, SessionError> {
    let (invocation, active) = invocation_capability(lane, drive, batch, call);
    let progress = Arc::new(open_tool_progress(lane.progress_lane(), drive, &batch.turn_id, call.source_index(), call.result_entry_id()));
    let latest: Arc<Mutex<Option<maho_ai::types::BoxFuture<'static, ()>>>> = Arc::new(Mutex::new(None));
    let tool_call = cleared.tool_call.clone();
    let args = cleared.args.clone();
    let tool = cleared.tool.clone();
    let on_update: AgentHarnessToolUpdateCallback = {
        let lane = lane.clone();
        let latest = latest.clone();
        let progress = progress.clone();
        let context = drive.context.clone();
        let run_id = drive.operation_id.clone();
        let turn_id = batch.turn_id.clone();
        let tool_call_id = tool_call.id.clone();
        let tool_name = tool_call.name.clone();
        let name = lane.name.clone();
        Arc::new(move |partial, options: Option<AgentHarnessToolUpdateOptions>| {
            let mut event = HarnessEvent::new(
                HarnessEventPayload::ToolUpdate { run_id: run_id.clone(), turn_id: turn_id.clone(), tool_call_id: tool_call_id.clone(), tool_name: tool_name.clone() },
                Some(name.clone()),
            );
            event.recovery = recovery.then_some(true);
            *latest.lock().unwrap_or_else(|error| error.into_inner()) = Some(lane.events.begin_emit_batch(vec![event], context.clone()));
            if options.and_then(|options| options.checkpoint) == Some(true) {
                progress.write(partial);
            }
        })
    };
    let execution = match execute_tool_call(cleared, &drive.gate, on_update, tool_context, invocation.clone(), drive.context.clone()).await {
        Ok(executed) => executed,
        Err(GateRefusal::Abort(abort)) => {
            active.store(false, Ordering::SeqCst);
            progress.seal();
            let _ = progress.drain().await;
            abort.cancellation.wait().await;
            return Ok(if recovery { interrupted_outcome(tool_call, None, now_ms()) } else { aborted_outcome(tool_call, now_ms()) });
        }
        Err(GateRefusal::Closed(message)) => {
            active.store(false, Ordering::SeqCst);
            progress.seal();
            let _ = progress.drain().await;
            return Err(session_invariant_error(message));
        }
    };
    active.store(false, Ordering::SeqCst);
    progress.seal();
    if let Some(delivery) = latest.lock().unwrap_or_else(|error| error.into_inner()).take() {
        delivery.await;
    }
    progress.drain().await?;
    let patch = match run_after_tool_hook(lane, drive, &ClearedToolCall { tool_call: tool_call.clone(), tool: tool.clone(), args: args.clone() }, &execution).await {
        Ok(patch) => patch,
        Err(HookRunError::Gate(GateRefusal::Abort(_))) => None,
        Err(error) => return Err(session_invariant_error(error.to_string())),
    };
    let finalized = finalize_tool_call(&ClearedToolCall { tool_call, tool, args }, execution, patch);
    Ok(outcome_from_finalized(finalized))
}

enum PreparedToolInvocation {
    Ready { cleared: ClearedToolCall<()> },
    Outcome { outcome: ToolOutcome },
}

async fn prepare_tool_invocation(
    lane: &Arc<Lane>,
    drive: &Drive,
    sources: &ToolBatchSource,
    call: &DurableCall,
    tools: &[Arc<AgentHarnessTool<()>>],
) -> Result<PreparedToolInvocation, SessionError> {
    let tool_call = tool_call_for(sources, call)?.clone();
    if sources.assistant.stop_reason == maho_ai::types::StopReason::Length {
        return Ok(PreparedToolInvocation::Outcome { outcome: truncated_outcome(tool_call, now_ms()) });
    }
    let prepared = match prepare_tool_call(tool_call.clone(), tools) {
        ToolCallPreparation::Immediate(outcome) => return Ok(PreparedToolInvocation::Outcome { outcome: outcome_from_immediate(outcome) }),
        ToolCallPreparation::Prepared(prepared) => prepared,
    };
    let mut invocation = HookInvocation::new(lane.name.clone(), drive.operation_id.clone());
    invocation.tool_call_id = Some(tool_call.id.clone());
    invocation.tool_name = Some(tool_call.name.clone());
    invocation.args = prepared.args.clone();
    let decision = match lane.hooks.run_tool_with_gate(HookName::BeforeTool, invocation, &drive.gate, &drive.context).await {
        Ok(HookResult::BeforeTool { args, block }) => Some(crate::harness::execution::tools::BeforeToolDecision {
            args,
            block: block.map(|block| crate::harness::execution::tools::BlockDecision { reason: block.reason, terminate: block.terminate }),
        }),
        Ok(_) => None,
        Err(HookRunError::Gate(GateRefusal::Abort(abort))) => {
            abort.cancellation.wait().await;
            return Ok(PreparedToolInvocation::Outcome { outcome: aborted_outcome(tool_call, now_ms()) });
        }
        Err(error) => return Err(session_invariant_error(error.to_string())),
    };
    Ok(match apply_before_tool_decision(prepared, decision) {
        ClearToolCallOutcome::Immediate(outcome) => PreparedToolInvocation::Outcome { outcome: outcome_from_immediate(outcome) },
        ClearToolCallOutcome::Cleared(cleared) => PreparedToolInvocation::Ready { cleared },
    })
}

type ToolTask<'a> = maho_ai::types::BoxFuture<'a, Result<(), SessionError>>;

/// Pinned `startToolInvocation`: prepare, publish intent, then perform one call.
fn start_tool_invocation<'a>(
    lane: &'a Arc<Lane>,
    drive: &'a Drive,
    run: &'a ToolsOperation,
    sources: &'a ToolBatchSource,
    call: DurableCall,
    tools: &'a [Arc<AgentHarnessTool<()>>],
    recovery: bool,
) -> ToolTask<'a> {
    Box::pin(async move {
        let prepared = prepare_tool_invocation(lane, drive, sources, &call, tools).await?;
        match prepared {
            PreparedToolInvocation::Outcome { outcome } => {
                publish_tool_outcome(lane.as_ref(), call, outcome, &drive.context).await
            }
            PreparedToolInvocation::Ready { cleared } => {
                let tool_call = cleared.tool_call.clone();
                // Pinned `cleared.tool.replay ?? "never"`: the Rust `AgentHarnessTool` carries no
                // tool-level replay, so the pinned fallback (`never`) is the faithful value.
                let replay = ToolCallReplay::Never;
                let args = serde_json::Value::Object(cleared.args.clone());
                let effect_pending = publish_tool_intent(lane.as_ref(), call.clone(), args.clone(), replay, &drive.context).await?;
                if let Some(event) = tool_start_event(lane, drive, &run.batch, &tool_call, recovery) {
                    lane.emit_batch(vec![event], &drive.context).await;
                }
                match effect_pending {
                    ContinueOperationResult::CancelRequested => publish_tool_outcome(lane.as_ref(), call, aborted_outcome(tool_call, now_ms()), &drive.context).await,
                    ContinueOperationResult::Result { value } => {
                        let outcome = perform_tool_invocation(lane, drive, &run.batch, &value, cleared, (), recovery).await?;
                        publish_tool_outcome(lane.as_ref(), value, outcome, &drive.context).await
                    }
                }
            }
        }
    })
}

fn tool_start_event(
    lane: &Arc<Lane>,
    drive: &Drive,
    batch: &ToolBatch,
    tool_call: &AgentToolCall,
    recovery: bool,
) -> Option<HarnessEvent> {
    let mut event = HarnessEvent::new(
        HarnessEventPayload::ToolStart { run_id: drive.operation_id.clone(), turn_id: batch.turn_id.clone(), tool_call_id: tool_call.id.clone(), tool_name: tool_call.name.clone() },
        Some(lane.name.clone()),
    );
    event.recovery = recovery.then_some(true);
    Some(event)
}

/// Pinned `recoverToolInvocation`: replay-safe recovery or a synthetic interrupted outcome.
fn recover_tool_invocation<'a>(
    lane: &'a Arc<Lane>,
    drive: &'a Drive,
    run: &'a ToolsOperation,
    sources: &'a ToolBatchSource,
    call: DurableCall,
    tools_by_name: &'a BTreeMap<String, Arc<AgentHarnessTool<()>>>,
    cancelled: bool,
) -> ToolTask<'a> {
    Box::pin(async move {
        let tool_call = tool_call_for(sources, &call)?.clone();
        let replay = match &call {
            DurableCall::EffectPending { replay, .. } => replay.clone(),
            _ => ToolCallReplay::Never,
        };
        let tool = tools_by_name.get(&tool_call.name).cloned();
        // Pinned also requires the resolved tool to be replay-safe; the Rust `AgentHarnessTool`
        // carries no tool-level replay, so the durable call's own replay flag is authoritative.
        if !cancelled && replay == ToolCallReplay::Safe {
            let args = clear_replay_checkpoint(lane.as_ref(), drive, &run.batch, &call, &tool_call).await?;
            let args_map = match args {
                serde_json::Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };
            let cleared = ClearedToolCall { tool_call: tool_call.clone(), tool: tool.ok_or_else(|| session_invariant_error("Replay-safe tool is unavailable"))?, args: args_map };
            let outcome = perform_tool_invocation(lane, drive, &run.batch, &call, cleared, (), true).await?;
            return publish_tool_outcome(lane.as_ref(), call, outcome, &drive.context).await;
        }
        let checkpoint = read_checkpoint(lane.as_ref(), drive, &call).await?;
        publish_tool_outcome(lane.as_ref(), call, interrupted_outcome(tool_call, checkpoint.as_ref(), now_ms()), &drive.context).await
    })
}

async fn run_sequential(
    lane: &Arc<Lane>,
    drive: &Drive,
    run: &ToolsOperation,
    sources: &ToolBatchSource,
    execution: Option<(&[Arc<AgentHarnessTool<()>>], &BTreeMap<String, Arc<AgentHarnessTool<()>>>)>,
    recovery: bool,
) -> Result<ProcedureResult, SessionError> {
    let transitions = run.batch.calls.len() * 2 + 1;
    for _ in 0..=transitions {
        materialize_ready(lane.as_ref(), drive, &OperationState::Tools(run.clone()), sources, recovery).await?;
        let Some(current) = current_tools(lane) else { return Ok(ProcedureResult::Continue); };
        let Some(call) = current.batch.calls.iter().find(|call| !matches!(call, DurableCall::Completed { .. })).cloned() else {
            return Err(session_invariant_error("Tool batch remained open after every call completed"));
        };
        if matches!(call, DurableCall::OutcomeReady { .. }) {
            return Err(session_invariant_error("Ready tool outcome was not materialized"));
        }
        if matches!(current.operation.control, Control::CancelRequested { .. }) {
            let tool_call = tool_call_for(sources, &call)?.clone();
            let outcome = if matches!(call, DurableCall::Planned { .. }) {
                aborted_outcome(tool_call, now_ms())
            } else {
                let checkpoint = read_checkpoint(lane.as_ref(), drive, &call).await?;
                interrupted_outcome(tool_call, checkpoint.as_ref(), now_ms())
            };
            publish_tool_outcome(lane.as_ref(), call.clone(), outcome, &drive.context).await?;
            continue;
        }
        let Some((tools, tools_by_name)) = execution else {
            return Err(session_invariant_error("Running tool batch is missing execution context"));
        };
        let task = match &call {
            DurableCall::Planned { .. } => start_tool_invocation(lane, drive, &current, sources, call.clone(), tools, recovery),
            _ => recover_tool_invocation(lane, drive, &current, sources, call.clone(), tools_by_name, false),
        };
        task.await?;
    }
    Err(session_invariant_error("Sequential tool batch exceeded its bounded transition count"))
}

async fn run_parallel(
    lane: &Arc<Lane>,
    drive: &Drive,
    run: &ToolsOperation,
    sources: &ToolBatchSource,
    tools: &[Arc<AgentHarnessTool<()>>],
    tools_by_name: &BTreeMap<String, Arc<AgentHarnessTool<()>>>,
    recovery: bool,
) -> Result<ProcedureResult, SessionError> {
    let cancelled = matches!(run.operation.control, Control::CancelRequested { .. });
    let mut jobs: Vec<ToolTask<'_>> = Vec::new();
    for call in run.batch.calls.clone() {
        if matches!(call, DurableCall::Completed { .. } | DurableCall::OutcomeReady { .. }) {
            continue;
        }
        let task = match &call {
            DurableCall::Planned { .. } => start_tool_invocation(lane, drive, run, sources, call.clone(), tools, recovery),
            _ => recover_tool_invocation(lane, drive, run, sources, call.clone(), tools_by_name, cancelled),
        };
        jobs.push(task);
    }
    for result in futures::future::join_all(jobs).await {
        result?;
    }
    materialize_ready(lane.as_ref(), drive, &OperationState::Tools(run.clone()), sources, recovery).await?;
    Ok(ProcedureResult::Continue)
}

/// Pinned `runTools`: execute, recover, stage, and source-order one complete durable tool batch.
pub async fn run_tools(
    lane: &Arc<Lane>,
    drive: &Drive,
    state: OperationState,
) -> Result<ProcedureResult, SessionError> {
    let OperationState::Tools(run) = state else {
        return Err(session_invariant_error("Tool execution requires a tools operation"));
    };
    let batch = run.batch.clone();
    let recovery = batch.calls.iter().any(|call| matches!(call, DurableCall::EffectPending { .. } | DurableCall::OutcomeReady { .. }));
    if recovery {
        let mut event = HarnessEvent::new(
            HarnessEventPayload::TurnStart { run_id: drive.operation_id.clone(), turn_id: batch.turn_id.clone() },
            Some(lane.name.clone()),
        );
        event.recovery = Some(true);
        lane.emit_batch(vec![event], &drive.context).await;
    }
    let sources = read_tool_batch_source(lane.as_ref(), drive, &batch).await?;
    materialize_ready(lane.as_ref(), drive, &OperationState::Tools(run.clone()), &sources, recovery).await?;
    let Some(current) = current_tools(lane) else { return Ok(ProcedureResult::Continue); };
    if matches!(current.operation.control, Control::CancelRequested { .. }) {
        return run_sequential(lane, drive, &current, &sources, None, recovery).await;
    }
    let config = lane.read_config();
    let active: std::collections::BTreeSet<String> = batch.configuration.active_tool_names.iter().cloned().collect();
    let tools: Vec<Arc<AgentHarnessTool<()>>> = config.tools.iter().filter(|tool| active.contains(tool.name())).cloned().collect();
    let tools_by_name: BTreeMap<String, Arc<AgentHarnessTool<()>>> = tools.iter().map(|tool| (tool.name().to_owned(), tool.clone())).collect();
    if run.operation.settings.tool_execution == ToolExecutionMode::Sequential {
        run_sequential(lane, drive, &current, &sources, Some((&tools, &tools_by_name)), recovery).await
    } else {
        run_parallel(lane, drive, &current, &sources, &tools, &tools_by_name, recovery).await
    }
}
