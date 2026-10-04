use crate::harness::compaction::branch_summarization::BranchPreparation;
use crate::harness::compaction::compaction::CompactionPreparation;
use crate::harness::compaction::utils::FileOperations;
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::{
    DurableFileOperations, DurableStructuralPreparation, ResultBoundary, SummaryTask,
    SummaryTaskReason,
};

pub fn durable_file_operations(operations: &FileOperations) -> DurableFileOperations {
    DurableFileOperations {
        read: operations.read.iter().cloned().collect(),
        written: operations.written.iter().cloned().collect(),
        edited: operations.edited.iter().cloned().collect(),
    }
}

pub fn durable_compaction_preparation(
    preparation: &CompactionPreparation,
) -> DurableStructuralPreparation {
    DurableStructuralPreparation::Compaction {
        messages_to_summarize: preparation.messages_to_summarize.clone(),
        turn_prefix_messages: preparation.turn_prefix_messages.clone(),
        retained_tail: preparation.retained_tail.clone(),
        is_split_turn: preparation.is_split_turn,
        tokens_before: preparation.tokens_before,
        previous_summary: preparation.previous_summary.clone(),
        file_ops: durable_file_operations(&preparation.file_ops),
        settings: preparation.settings,
    }
}

pub fn durable_branch_preparation(preparation: &BranchPreparation) -> DurableStructuralPreparation {
    DurableStructuralPreparation::BranchSummary {
        messages: preparation.messages.clone(),
        file_ops: durable_file_operations(&preparation.file_ops),
        total_tokens: preparation.total_tokens,
    }
}

pub fn file_operations(operations: &DurableFileOperations) -> FileOperations {
    FileOperations {
        read: operations.read.iter().cloned().collect(),
        written: operations.written.iter().cloned().collect(),
        edited: operations.edited.iter().cloned().collect(),
    }
}

pub fn compaction_preparation(
    preparation: &DurableStructuralPreparation,
) -> Result<CompactionPreparation, SessionError> {
    match preparation {
        DurableStructuralPreparation::Compaction {
            messages_to_summarize,
            turn_prefix_messages,
            retained_tail,
            is_split_turn,
            tokens_before,
            previous_summary,
            file_ops,
            settings,
        } => Ok(CompactionPreparation {
            messages_to_summarize: messages_to_summarize.clone(),
            turn_prefix_messages: turn_prefix_messages.clone(),
            retained_tail: retained_tail.clone(),
            is_split_turn: *is_split_turn,
            tokens_before: *tokens_before,
            previous_summary: previous_summary.clone(),
            file_ops: file_operations(file_ops),
            settings: *settings,
        }),
        DurableStructuralPreparation::BranchSummary { .. } => {
            Err(session_invariant_error("Expected compaction preparation"))
        }
    }
}

pub fn branch_preparation(
    preparation: &DurableStructuralPreparation,
) -> Result<BranchPreparation, SessionError> {
    match preparation {
        DurableStructuralPreparation::BranchSummary {
            messages,
            file_ops,
            total_tokens,
        } => Ok(BranchPreparation {
            messages: messages.clone(),
            file_ops: file_operations(file_ops),
            total_tokens: *total_tokens,
        }),
        DurableStructuralPreparation::Compaction { .. } => Err(session_invariant_error(
            "Expected branch summary preparation",
        )),
    }
}

pub fn summary_kind(task: &SummaryTask) -> &'static str {
    match task.boundary {
        ResultBoundary::CommitNavigation { .. } => "branch_summary",
        ResultBoundary::Finish | ResultBoundary::ResumeCheckpoint { .. } => "compaction",
    }
}

pub fn compaction_reason(task: &SummaryTask) -> Result<SummaryTaskReason, SessionError> {
    match (task.reason, &task.boundary) {
        (Some(reason), _) => Ok(reason),
        (None, ResultBoundary::Finish) => Ok(SummaryTaskReason::Manual),
        (
            None,
            ResultBoundary::ResumeCheckpoint { .. } | ResultBoundary::CommitNavigation { .. },
        ) => Err(session_invariant_error(format!(
            "In-run compaction task {} is missing its reason",
            task.task_id
        ))),
    }
}

pub fn navigation_boundary(task: &SummaryTask) -> Result<(&str, Option<&str>), SessionError> {
    match &task.boundary {
        ResultBoundary::CommitNavigation { target_id, label } => Ok((target_id, label.as_deref())),
        ResultBoundary::Finish | ResultBoundary::ResumeCheckpoint { .. } => Err(
            session_invariant_error(format!("Summary task {} is not a navigation", task.task_id)),
        ),
    }
}

pub fn effect_pending_from_ready(ready: &crate::harness::session::types::SummaryReadyOperation) -> crate::harness::session::types::SummaryEffectPendingOperation {
    crate::harness::session::types::SummaryEffectPendingOperation {
        operation: ready.operation.clone(), at: crate::harness::session::types::OperationMarker::SummaryEffectPending,
        pending: crate::harness::session::types::SummaryGenerationEffectPending { scope: ready.ready.scope.clone(), attempt: ready.ready.next_attempt, request: None, usage_ids: vec![] },
    }
}

pub fn ready_from_retry_wait(retry: &crate::harness::session::types::SummaryRetryWaitOperation) -> crate::harness::session::types::SummaryReadyOperation {
    crate::harness::session::types::SummaryReadyOperation { operation: retry.operation.clone(), at: crate::harness::session::types::OperationMarker::SummaryReady, ready: crate::harness::session::types::SummaryGenerationReady { scope: retry.retry.scope.clone(), next_attempt: retry.retry.retry_wait.next_attempt } }
}

pub async fn publish_attempt_intent(lane: &crate::harness::runtime::lane::Lane, drive: &crate::harness::runtime::types::Drive) -> Result<crate::harness::runtime::types::ContinueOperationResult<crate::harness::session::types::SummaryEffectPendingOperation>, SessionError> {
    lane.continue_operation(|_, current, _, _| Box::pin(async move {
        let crate::harness::session::types::OperationState::SummaryReady(ready) = current else { return Err(session_invariant_error("Expected summary.ready operation")); };
        let pending = effect_pending_from_ready(&ready);
        Ok(crate::harness::runtime::types::OperationCommand::Commit { decision: crate::harness::runtime::types::CommitDecision { writes: vec![], materialize: std::sync::Arc::new({ let pending = pending.clone(); move |_| pending.clone() }), events: None }, operation_state: Box::new(crate::harness::session::types::OperationState::SummaryEffectPending(pending)), lane: None })
    }), &drive.context).await
}

pub async fn publish_nested_request_intent(lane: &crate::harness::runtime::lane::Lane, drive: &crate::harness::runtime::types::Drive, index: usize, usage_id: String) -> Result<crate::harness::runtime::types::ContinueOperationResult<crate::harness::session::types::SummaryEffectPendingOperation>, SessionError> {
    lane.continue_operation(move |_, current, _, _| Box::pin(async move {
        let crate::harness::session::types::OperationState::SummaryEffectPending(mut pending) = current else { return Err(session_invariant_error("Expected summary.effect_pending operation")); };
        pending.pending.request = Some(crate::harness::session::types::SummaryGenerationRequest { index, usage_id });
        Ok(crate::harness::runtime::types::OperationCommand::Commit { decision: crate::harness::runtime::types::CommitDecision { writes: vec![], materialize: std::sync::Arc::new({ let pending = pending.clone(); move |_| pending.clone() }), events: None }, operation_state: Box::new(crate::harness::session::types::OperationState::SummaryEffectPending(pending)), lane: None })
    }), &drive.context).await
}

pub async fn publish_nested_request_outcome(lane: &crate::harness::runtime::lane::Lane, drive: &crate::harness::runtime::types::Drive, usage_id: String, usage: maho_ai::types::Usage) -> Result<(), SessionError> {
    let name = lane.name.clone();
    lane.settle_operation(move |_, current, _, _| Box::pin(async move {
        let crate::harness::session::types::OperationState::SummaryEffectPending(mut pending) = current else { return Err(session_invariant_error("Expected summary.effect_pending operation")); };
        pending.pending.usage_ids.push(usage_id.clone());
        pending.pending.request = None;
        let row = crate::harness::session::types::NewUsageRow { id: usage_id.clone(), usage, adjustment: false, entry_id: None, details: None };
        Ok(crate::harness::runtime::types::OperationCommand::Commit { decision: crate::harness::runtime::types::CommitDecision {
            writes: vec![crate::harness::session::commit::insert_usage(row)], materialize: std::sync::Arc::new(|_| ()), events: Some(std::sync::Arc::new(move |commit| vec![crate::harness::events::HarnessEvent::new(crate::harness::events::HarnessEventPayload::Usage { lane: name.clone(), row: crate::harness::session::types::UsageRow { id: usage_id.clone(), seq: commit.seqs[0], usage, adjustment: false, entry_id: None, details: None }, totals: commit.stats.usage }, Some(name.clone()))])),
        }, operation_state: Box::new(crate::harness::session::types::OperationState::SummaryEffectPending(pending)), lane: None })
    }), &drive.context).await
}

pub async fn commit_navigation(lane: &crate::harness::runtime::lane::Lane, drive: &crate::harness::runtime::types::Drive) -> Result<crate::harness::runtime::types::ProcedureResult, SessionError> {
    use crate::harness::runtime::types::{ContinueOperationResult, FinishDecision, LanePatch, OperationCommand, ProcedureResult};
    use crate::harness::session::types::{OperationState, TerminalStatus, Write};
    use crate::harness::session::values::{branch_tip, entry_label, set_value};
    let name = lane.name.clone();
    let context = drive.context.clone();
    let result = lane.continue_operation(move |_, current, meta, reader| Box::pin(async move {
        let OperationState::NavigationReadyToCommit(navigation) = &current else { return Err(session_invariant_error("Expected navigation.ready_to_commit operation")); };
        if let Some(target) = &navigation.target_id && !reader.get_entries(vec![target.clone()], &context).await?.contains_key(target) { return Err(session_invariant_error(format!("Navigation target {target} is missing"))); }
        if navigation.target_id == meta.source_tip_id { return Err(session_invariant_error("Navigation target must differ from its source tip")); }
        if navigation.target_id.is_none() && navigation.label.is_some() { return Err(session_invariant_error("Root navigation cannot set a label")); }
        let mut writes = vec![Write::Value(set_value(&branch_tip(&name), crate::harness::runtime::lane::encoded(&navigation.target_id)?))];
        if let (Some(target), Some(label)) = (&navigation.target_id, &navigation.label) { writes.push(Write::Value(set_value(&entry_label(target), crate::harness::runtime::lane::encoded(label)?))); }
        writes.extend(super::drive::terminal::operation_cleanup_writes(reader, &meta.operation_id, &current, &context).await?);
        let record = super::drive::terminal::operation_result_record(&meta, TerminalStatus::Completed, navigation.target_id.clone(), None).map_err(|error| session_invariant_error(error.to_string()))?;
        let event = crate::harness::events::HarnessEvent::new(crate::harness::events::HarnessEventPayload::NavigationEnd(crate::harness::events::NavigationEndPayload { run_id: meta.operation_id, from_tip_id: meta.source_tip_id, tip_id: navigation.target_id.clone(), ended_at: record.ended_at, status: "completed".into(), error: None }), Some(name));
        Ok(OperationCommand::Finish { decision: Box::new(FinishDecision { writes, record: record.clone(), lane: Some(LanePatch { tip_id: Some(navigation.target_id.clone()), ..Default::default() }), materialize: std::sync::Arc::new(move |_| ProcedureResult::Settled { outcome: record.clone() }), events: Some(std::sync::Arc::new(move |_| vec![event.clone()])) }) })
    }), &drive.context).await?;
    Ok(match result { ContinueOperationResult::CancelRequested => ProcedureResult::Continue, ContinueOperationResult::Result { value } => value })
}

// ---------------------------------------------------------------------------------------------
// Port of the remaining pinned `drive/structural.ts` orchestration: the structural decision,
// generation, retry-wait, recovery, and compaction-preparation procedures.
// ---------------------------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use maho_ai::models::ModelsRequestTransforms;
use maho_ai::types::{
    AssistantMessage, BoxFuture, CacheRetention, Context as AiContext, DeferredOption, SimpleStreamOptions,
    StopReason,
};
use maho_ai::utils::lazy::setup_error_message;
use maho_ai::utils::retry::{RetryPolicy, is_retryable_assistant_error, retry_delay_ms};

use crate::harness::compaction::branch_summarization::{
    generate_branch_summary_with_request, BranchSummaryResult, PreparedBranchSummaryOptions,
};
use crate::harness::compaction::compaction::{
    compact_with_request, prepare_compaction, should_compact, CompactGenerationOptions, CompactResult,
    SummaryRequest,
};
use crate::harness::context::{Context, with_abort_signal};
use crate::harness::events::{HarnessEvent, HarnessEventPayload, RunEndPayload};
use crate::harness::execution::effect_gate::GateRefusal;
use crate::harness::hooks::{HookInvocation, HookName, HookResult, HookRunError, apply_stream_options_patch};
use crate::harness::runtime::drive::boundary::{
    assistant_ready_at_boundary, boundary_placement_events, finish_run_boundary, normalized_retry_policy,
    BoundaryPlacement,
};
use crate::harness::runtime::drive::retry::{now_ms, retry_not_before_now, wait_until};
use crate::harness::runtime::drive::terminal::{operation_cleanup_writes, operation_result_record};
use crate::harness::runtime::lane::{ContinueOperationResult as LaneContinue, Lane, OperationCommand};
use crate::harness::runtime::transcript::{committed_entry_events, read_bounded_entries};
use crate::harness::runtime::types::{Drive, ProcedureResult, RuntimeDriveLane, StructuralPreparation};
use crate::harness::session::commit::{insert_entry, insert_usage};
use crate::harness::session::types::{
    Control, EntryKind, InboxItemKind, LaneConfiguration, LanePatch, NewEntry, NewUsageRow, OperationError,
    OperationScope, OperationState, PendingEntry, SessionReader, SummaryContext, SummaryDecidingOperation,
    SummaryEffectPendingOperation, SummaryGenerationReady, SummaryGenerationRetryWait, SummaryReadyOperation,
    SummaryRetryWaitOperation, TerminalStatus, UsageRow, Write,
};
use crate::harness::session::values::{branch_tip, delete_value, entry_label, pending_entry, set_value};
use crate::harness::types::AgentHarnessStreamOptions;

/// Pinned `summaryKind` decoded back from a durable preparation envelope.
fn durable_summary_kind(preparation: &DurableStructuralPreparation) -> &'static str {
    match preparation {
        DurableStructuralPreparation::Compaction { .. } => "compaction",
        DurableStructuralPreparation::BranchSummary { .. } => "branch_summary",
    }
}

/// The one summary task a structural operation carries, if any.
fn summary_task_of(state: &OperationState) -> Option<&SummaryTask> {
    match state {
        OperationState::SummaryDeciding(state) => Some(&state.task),
        OperationState::SummaryReady(state) => Some(&state.ready.scope.task),
        OperationState::SummaryEffectPending(state) => Some(&state.pending.scope.task),
        OperationState::SummaryRetryWait(state) => Some(&state.retry.scope.task),
        _ => None,
    }
}

enum StructuralPreparationView {
    Compaction(CompactionPreparation),
    Branch(BranchPreparation),
}

enum StructuralOutcome {
    Compaction { result_entry_id: String, result: CompactResult, from_hook: bool },
    BranchSummary { result_entry_id: String, result: BranchSummaryResult, from_hook: bool },
    Declined,
    Failed { error: OperationError },
}

enum StructuralPublication {
    Procedure(ProcedureResult),
    FinishPending(Vec<String>),
}

enum StructuralAttempt {
    Compaction { result: CompactResult, retryable: bool },
    BranchSummary { result: BranchSummaryResult, retryable: bool },
    Error { error: OperationError, retryable: bool },
    CancelRequested,
}

fn operation_error(code: impl Into<String>, message: impl Into<String>, details: Option<serde_json::Value>) -> OperationError {
    OperationError { code: code.into(), message: message.into(), details }
}

fn thinking_level_of(level: maho_ai::types::ModelThinkingLevel) -> Option<maho_ai::types::ThinkingLevel> {
    use maho_ai::types::{ModelThinkingLevel, ThinkingLevel};
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
    }
}

fn retry_policy_from_normalized(policy: &crate::harness::session::types::NormalizedRetryPolicy) -> RetryPolicy {
    RetryPolicy {
        enabled: true,
        max_retries: policy.max_attempts.saturating_sub(1),
        base_delay_ms: policy.base_delay_ms,
        max_agent_delay_ms: Some(policy.max_agent_delay_ms),
        random: None,
    }
}

fn summary_context(lane: &Lane, result_entry_id: &str, configuration: LaneConfiguration) -> SummaryContext {
    let mut stream_options = lane.read_config().stream_options.clone();
    stream_options.deferred = Some(DeferredOption::Enabled(false));
    SummaryContext {
        result_entry_id: result_entry_id.to_owned(),
        configuration,
        stream_options,
        retry_policy: normalized_retry_policy(lane),
    }
}

fn usage_event(row: &UsageRow, lane: &str, totals: maho_ai::types::Usage) -> HarnessEvent {
    HarnessEvent::new(
        HarnessEventPayload::Usage { lane: lane.to_owned(), row: row.clone(), totals },
        Some(lane.to_owned()),
    )
}

fn request_stream_options(
    options: SimpleStreamOptions,
    stream_options: &AgentHarnessStreamOptions,
    context: &Context,
) -> SimpleStreamOptions {
    let mut options = options;
    options.stream.transport = stream_options.transport;
    options.stream.request.timeout_ms = stream_options.timeout_ms;
    options.stream.request.max_retries = stream_options.max_retries;
    options.stream.request.max_retry_delay_ms = stream_options.max_retry_delay_ms;
    options.stream.request.headers = stream_options.headers.as_ref().map(|headers| {
        headers.iter().map(|(key, value)| (key.clone(), value.as_str().map(str::to_owned))).collect()
    });
    options.stream.metadata = stream_options.metadata.clone();
    options.stream.cache_retention = Some(CacheRetention::None);
    options.deferred = Some(DeferredOption::Enabled(false));
    options.stream.request.signal = context.abort_signal();
    options.stream.request.telemetry_context = None;
    options
}

fn cancelled_message(model: &maho_ai::model::Model) -> AssistantMessage {
    let mut message = setup_error_message(model, "Structural generation was cancelled");
    message.stop_reason = StopReason::Aborted;
    message.error_message = Some("Structural generation was cancelled".to_owned());
    message
}

/// Pinned `readStructuralPreparation`: consume the durable preparation for a decision task.
async fn read_structural_preparation(
    lane: &Arc<Lane>,
    drive: &Drive,
    deciding: &SummaryDecidingOperation,
) -> Result<LaneContinue<StructuralPreparationView>, SessionError> {
    let this = lane.clone();
    let drive_context = drive.context.clone();
    let operation_id = drive.operation_id.clone();
    let _ = deciding;
    this.continue_operation(
        move |_state, current, _meta, reader| {
            Box::pin(async move {
                let OperationState::SummaryDeciding(deciding) = current else {
                    return Err(session_invariant_error("Structural decision requires a summary.deciding operation"));
                };
                let expected = summary_kind(&deciding.task);
                let stored = reader
                    .get_value(&operation_preparation(&operation_id, &deciding.task.task_id), &drive_context)
                    .await?;
                let Some(stored) = stored else {
                    return Err(session_invariant_error(format!("Structural task {} is missing its {expected} preparation", deciding.task.task_id)));
                };
                let durable: DurableStructuralPreparation = serde_json::from_value(stored.value)
                    .map_err(|error| session_invariant_error(error.to_string()))?;
                if durable_summary_kind(&durable) != expected {
                    return Err(session_invariant_error(format!("Structural task {} is missing its {expected} preparation", deciding.task.task_id)));
                }
                if let ResultBoundary::CommitNavigation { target_id, .. } = &deciding.task.boundary
                    && !reader.get_entries(vec![target_id.clone()], &drive_context).await?.contains_key(target_id) {
                    return Err(session_invariant_error(format!("Navigation target {target_id} is missing")));
                }
                let view = match &durable {
                    DurableStructuralPreparation::Compaction { .. } => StructuralPreparationView::Compaction(compaction_preparation(&durable)?),
                    DurableStructuralPreparation::BranchSummary { .. } => StructuralPreparationView::Branch(branch_preparation(&durable)?),
                };
                Ok(OperationCommand::Return { result: view })
            })
        },
        &drive.context,
    )
    .await
}

/// Pinned `publishStructuralReady`: move a decided task into a ready generation attempt.
async fn publish_structural_ready(
    lane: &Arc<Lane>,
    drive: &Drive,
    _deciding: &SummaryDecidingOperation,
) -> Result<ProcedureResult, SessionError> {
    let this = lane.clone();
    let result_entry_id = (lane.session.id_generator())(None);
    let published = this
        .continue_operation(
            move |state, current, _meta, _reader| {
                Box::pin(async move {
                    let OperationState::SummaryDeciding(deciding) = current else {
                        return Err(session_invariant_error("Structural decision requires a summary.deciding operation"));
                    };
                    let operation_state = OperationState::SummaryReady(SummaryReadyOperation {
                        operation: deciding.operation.clone(),
                        at: crate::harness::session::types::OperationMarker::SummaryReady,
                        ready: SummaryGenerationReady {
                            scope: crate::harness::session::types::SummaryGenerationScope {
                                task: deciding.task.clone(),
                                summary_context: summary_context(&this, &result_entry_id, state.configuration.clone()),
                            },
                            next_attempt: 1,
                        },
                    });
                    Ok(OperationCommand::Commit {
                        decision: crate::harness::runtime::lane::CommitDecision {
                            writes: Vec::new(),
                            materialize: Arc::new(|_| ProcedureResult::Continue),
                            events: None,
                        },
                        operation_state: Box::new(operation_state),
                        lane: None,
                    })
                })
            },
            &drive.context,
        )
        .await?;
    Ok(match published {
        LaneContinue::CancelRequested => ProcedureResult::Continue,
        LaneContinue::Result { value } => value,
    })
}

/// A local `planBoundaryInbox` that takes an owned `Context`, because the pinned `&Drive` argument
/// cannot cross the `continue_operation` planner's `Send + 'static` bound. Behaviour mirrors
/// `drive::boundary::plan_boundary_inbox` exactly, substituting the captured context for `drive.context`.
async fn plan_boundary_inbox_ctx(
    lane: &dyn RuntimeDriveLane,
    context: &Context,
    state: &crate::harness::runtime::types::LaneState,
    scope: &OperationScope,
    reader: &dyn SessionReader,
    mut tip_id: Option<String>,
    follow_up_when_no_trigger: bool,
) -> Result<BoundaryPlacement, SessionError> {
    let mut steer = state.inbox.iter().filter(|item| item.kind == InboxItemKind::Steer);
    let selected_steer: Vec<_> = match scope.settings.steering_mode {
        crate::types::QueueMode::All => steer.by_ref().collect(),
        crate::types::QueueMode::OneAtATime => steer.next().into_iter().collect(),
    };
    let mut selected: Vec<_> = state
        .inbox
        .iter()
        .filter(|item| item.kind == InboxItemKind::Write || selected_steer.iter().any(|s| s.entry_id == item.entry_id))
        .cloned()
        .collect();
    let load = |items: Vec<crate::harness::session::types::InboxItem>| async move {
        let mut loaded = Vec::new();
        for item in items {
            let kind = match item.kind {
                InboxItemKind::Steer => "steer",
                InboxItemKind::FollowUp => "followUp",
                InboxItemKind::NextRun => "nextRun",
                InboxItemKind::Write => "write",
            };
            let stored = reader
                .get_value(&pending_entry(&item.entry_id), context)
                .await?
                .ok_or_else(|| session_invariant_error(format!("Pending {kind} entry {} is missing its payload", item.entry_id)))?;
            let pending: PendingEntry = serde_json::from_value(stored.value).map_err(|error| session_invariant_error(error.to_string()))?;
            if item.kind != InboxItemKind::Write && !matches!(pending, PendingEntry::Message { .. }) {
                return Err(session_invariant_error(format!("Queued {kind} entry {} is not a message", item.entry_id)));
            }
            loaded.push((item, pending));
        }
        Ok::<_, SessionError>(loaded)
    };
    let mut pending = load(selected.clone()).await?;
    let config = lane.config();
    let projects = |value: &PendingEntry| match value {
        PendingEntry::Message { .. } => true,
        PendingEntry::Custom { custom_type, .. } => config.entry_projectors.contains_key(custom_type),
    };
    if follow_up_when_no_trigger && !pending.iter().any(|(_, value)| projects(value)) {
        let mut follow = state.inbox.iter().filter(|item| item.kind == InboxItemKind::FollowUp);
        let taken: Vec<_> = match scope.settings.follow_up_mode {
            crate::types::QueueMode::All => follow.by_ref().collect(),
            crate::types::QueueMode::OneAtATime => follow.next().into_iter().collect(),
        };
        selected.extend(taken.into_iter().cloned());
        selected.sort_by_key(|item| state.inbox.iter().position(|i| i.entry_id == item.entry_id));
        pending = load(selected.clone()).await?;
    }
    let mut entries = Vec::new();
    let mut trigger_entry_id = None;
    for (item, value) in pending {
        if projects(&value) {
            trigger_entry_id = Some(item.entry_id.clone());
        }
        let kind = match value {
            PendingEntry::Message { payload } => EntryKind::Message { message: payload, terminate: None },
            PendingEntry::Custom { custom_type, payload } => EntryKind::Custom { custom_type, data: payload },
        };
        entries.push(NewEntry { id: item.entry_id.clone(), parent_id: tip_id, kind });
        tip_id = Some(item.entry_id);
    }
    let inbox: Vec<_> = state.inbox.iter().filter(|item| !selected.iter().any(|s| s.entry_id == item.entry_id)).cloned().collect();
    let queues = if selected.is_empty() { None } else { Some(crate::harness::runtime::transcript::read_lane_queues(reader, &inbox, context).await?) };
    let mut writes: Vec<_> = entries.iter().cloned().map(insert_entry).collect();
    writes.extend(selected.iter().map(|item| Write::Value(delete_value(&pending_entry(&item.entry_id)))));
    if !entries.is_empty() {
        writes.push(Write::Value(set_value(&branch_tip(lane.name()), serde_json::json!(tip_id))));
    }
    Ok(BoundaryPlacement { entries, writes, tip_id, inbox, trigger_entry_id, queues })
}

fn reason_str(reason: crate::harness::session::types::SummaryTaskReason) -> &'static str {
    use crate::harness::session::types::SummaryTaskReason;
    match reason {
        SummaryTaskReason::Manual => "manual",
        SummaryTaskReason::Threshold => "threshold",
        SummaryTaskReason::Overflow => "overflow",
    }
}

/// Pinned `publishStructuralOutcome`: commit a compaction/branch result, or a terminal failure.
async fn publish_structural_outcome(
    lane: &Arc<Lane>,
    drive: &Drive,
    state: &OperationState,
    outcome: StructuralOutcome,
) -> Result<ProcedureResult, SessionError> {
    let hook_usage_id = match &outcome {
        StructuralOutcome::Compaction { result, from_hook: true, .. } if result.usage.is_some() => Some((lane.session.id_generator())(None)),
        StructuralOutcome::BranchSummary { result, from_hook: true, .. } if result.usage.is_some() => Some((lane.session.id_generator())(None)),
        _ => None,
    };
    let this = lane.clone();
    let drive_context = drive.context.clone();
    let operation_id = drive.operation_id.clone();
    let lane_name = lane.name.clone();
    let outcome = Arc::new(outcome);
    let published = this
        .continue_operation(
            move |state, current, meta, reader| {
                let outcome = outcome.clone();
                let hook_usage_id = hook_usage_id.clone();
                let drive_context = drive_context.clone();
                let operation_id = operation_id.clone();
                let lane_name = lane_name.clone();
                let lane_ref = this.clone();
                Box::pin(async move {
                    let task = summary_task_of(&current)
                        .ok_or_else(|| session_invariant_error("Structural outcome requires a summary task"))?
                        .clone();
                    let expected = summary_kind(&task);
                    let is_compaction_result = matches!(&*outcome, StructuralOutcome::Compaction { .. });
                    let is_branch_result = matches!(&*outcome, StructuralOutcome::BranchSummary { .. });
                    if is_compaction_result || is_branch_result {
                        let actual = if is_compaction_result { "compaction" } else { "branch_summary" };
                        if actual != expected {
                            return Err(session_invariant_error(format!("Structural {actual} result does not match {expected} task {}", task.task_id)));
                        }
                    }
                    let mut writes: Vec<Write> = Vec::new();
                    let mut hook_row: Option<(NewUsageRow, usize)> = None;
                    let mut entry: Option<(NewEntry, usize)> = None;
                    let mut terminal_tip_id = state.tip_id.clone();
                    if let (Some(usage_id), StructuralOutcome::Compaction { result, .. }) = (&hook_usage_id, &*outcome)
                        && let Some(usage) = result.usage {
                        let row = NewUsageRow { id: usage_id.clone(), usage, entry_id: None, adjustment: false, details: None };
                        hook_row = Some((row.clone(), writes.len()));
                        writes.push(insert_usage(row));
                    }
                    if let (Some(usage_id), StructuralOutcome::BranchSummary { result, .. }) = (&hook_usage_id, &*outcome)
                        && let Some(usage) = result.usage {
                        let row = NewUsageRow { id: usage_id.clone(), usage, entry_id: None, adjustment: false, details: None };
                        hook_row = Some((row.clone(), writes.len()));
                        writes.push(insert_usage(row));
                    }
                    if let StructuralOutcome::Compaction { result_entry_id, result, from_hook } = &*outcome {
                        let new_entry = NewEntry { id: result_entry_id.clone(), parent_id: state.tip_id.clone(), kind: EntryKind::Compaction { summary: result.summary.clone(), retained_tail: result.retained_tail.clone(), tokens_before: result.tokens_before, details: result.details.clone(), usage: result.usage, from_hook: *from_hook } };
                        let index = writes.len();
                        writes.push(insert_entry(new_entry.clone()));
                        writes.push(Write::Value(set_value(&branch_tip(&lane_name), serde_json::json!(result_entry_id))));
                        terminal_tip_id = Some(result_entry_id.clone());
                        entry = Some((new_entry, index));
                    } else if let StructuralOutcome::BranchSummary { result_entry_id, result, from_hook } = &*outcome {
                        let ResultBoundary::CommitNavigation { target_id, label } = &task.boundary else {
                            return Err(session_invariant_error("Branch summary requires a navigation boundary"));
                        };
                        let new_entry = NewEntry { id: result_entry_id.clone(), parent_id: Some(target_id.clone()), kind: EntryKind::BranchSummary { from_id: meta.source_tip_id.clone(), summary: result.summary.clone(), details: Some(serde_json::json!({ "readFiles": result.read_files, "modifiedFiles": result.modified_files })), usage: result.usage, from_hook: *from_hook } };
                        writes.push(Write::Value(set_value(&branch_tip(&lane_name), serde_json::json!(target_id))));
                        let index = writes.len();
                        writes.push(insert_entry(new_entry.clone()));
                        writes.push(Write::Value(set_value(&branch_tip(&lane_name), serde_json::json!(result_entry_id))));
                        if let Some(label) = label {
                            writes.push(Write::Value(set_value(&entry_label(target_id), serde_json::json!(label))));
                        }
                        terminal_tip_id = Some(result_entry_id.clone());
                        entry = Some((new_entry, index));
                    }
                    let attempt = match &current {
                        OperationState::SummaryReady(ready) => Some(ready.ready.next_attempt),
                        OperationState::SummaryEffectPending(effect) => Some(effect.pending.attempt),
                        _ => None,
                    };
                    let kind = if is_compaction_result { "compaction" } else if is_branch_result { "branch_summary" } else if matches!(&*outcome, StructuralOutcome::Declined) { "declined" } else { "failed" };
                    let failure_message = match &*outcome { StructuralOutcome::Failed { error } => Some(error.message.clone()), _ => None };
                    let task_id = task.task_id.clone();
                    let compaction_end_reason = compaction_reason(&task).map_or("manual", reason_str).to_owned();
                    let compaction_end_entry_id = entry.as_ref().map(|(new_entry, _)| new_entry.id.clone());
                    let events = {
                        let lane_name = lane_name.clone();
                        let operation_id = operation_id.clone();
                        let hook_row = hook_row.clone();
                        let entry = entry.clone();
                        let failure_message = failure_message.clone();
                        let task_id = task_id.clone();
                        let compaction_end_reason = compaction_end_reason.clone();
                        let compaction_end_entry_id = compaction_end_entry_id.clone();
                        Arc::new(move |commit: &crate::harness::session::types::CommitResult| -> Vec<HarnessEvent> {
                            let mut events = Vec::new();
                            if let Some((row, index)) = &hook_row {
                                let row = UsageRow { id: row.id.clone(), seq: commit.seqs[*index], usage: row.usage, entry_id: row.entry_id.clone(), adjustment: row.adjustment, details: row.details.clone() };
                                events.push(usage_event(&row, &lane_name, commit.stats.usage));
                            }
                            if let Some((new_entry, index)) = &entry {
                                events.extend(committed_entry_events(std::slice::from_ref(new_entry), commit, &lane_name, Some(&operation_id), *index).unwrap_or_default());
                            }
                            if let Some(attempt) = attempt && attempt > 1 {
                                events.push(HarnessEvent::new(HarnessEventPayload::RetryEnd { run_id: operation_id.clone(), step: task_id.clone(), attempt, success: kind == "compaction" || kind == "branch_summary", final_error: failure_message.clone() }, Some(lane_name.clone())));
                            }
                            if kind == "compaction"
                                && let Some(result_entry_id) = &compaction_end_entry_id {
                                    events.push(HarnessEvent::new(HarnessEventPayload::CompactionEnd { run_id: operation_id.clone(), reason: compaction_end_reason.clone(), ended_at: commit.timestamp, status: "completed".into(), entry_id: Some(result_entry_id.clone()) }, Some(lane_name.clone())));
                            }
                            events
                        })
                    };
                    match &task.boundary {
                        ResultBoundary::ResumeCheckpoint { resume_after } => {
                            if is_branch_result {
                                return Err(session_invariant_error("Run compaction boundary received a branch summary"));
                            }
                            let is_declined = matches!(&*outcome, StructuralOutcome::Declined);
                            if is_compaction_result || (is_declined && task.reason == Some(crate::harness::session::types::SummaryTaskReason::Threshold)) {
                                let Some(terminal_tip) = terminal_tip_id.clone() else { return Err(session_invariant_error("Run compaction has no Branch tip")); };
                                let continuation = resume_after.continuation;
                                let placement = plan_boundary_inbox_ctx(lane_ref.as_ref(), &drive_context, &state, &current.operation_scope_of(), reader, Some(terminal_tip), is_declined && matches!(continuation, Continuation::MayFinish { .. })).await?;
                                if is_declined && placement.trigger_entry_id.is_none() && matches!(continuation, Continuation::MayFinish { .. }) {
                                    return Ok(OperationCommand::Return { result: StructuralPublication::FinishPending(placement.entries.iter().map(|item| item.id.clone()).collect()) });
                                }
                                let placement_write_index = writes.len();
                                writes.extend(placement.writes.clone());
                                let operation_state = if let Some(trigger) = &placement.trigger_entry_id {
                                    assistant_ready_at_boundary(lane_ref.as_ref(), &state, current.operation_scope_of(), trigger.clone(), false)
                                } else if let Continuation::NeedAssistant { overflow_recovery_used } = continuation {
                                    assistant_ready_at_boundary(lane_ref.as_ref(), &state, current.operation_scope_of(), resume_after.trigger_entry_id.clone(), overflow_recovery_used)
                                } else {
                                    OperationState::Checkpoint(crate::harness::session::types::CheckpointOperation { operation: current.operation_scope_of(), checkpoint: resume_after.clone(), at: crate::harness::session::types::OperationMarker::Checkpoint })
                                };
                                let (name, id) = (lane_name.clone(), operation_id.clone());
                                let placement_owned = placement.clone();
                                let events_arc = events.clone();
                                let tip = placement.tip_id.clone();
                                let inbox = placement.inbox.clone();
                                return Ok(OperationCommand::Commit {
                                    decision: crate::harness::runtime::lane::CommitDecision {
                                        writes,
                                        materialize: Arc::new(|_| ProcedureResult::Continue),
                                        events: Some(Arc::new(move |commit| {
                                            let mut events = if is_compaction_result {
                                                (events_arc)(commit)
                                            } else {
                                                vec![HarnessEvent::new(HarnessEventPayload::CompactionEnd { run_id: id.clone(), reason: "threshold".into(), ended_at: commit.timestamp, status: "declined".into(), entry_id: None }, Some(name.clone()))]
                                            };
                                            events.extend(boundary_placement_events(&placement_owned, commit, placement_write_index, &name, &id));
                                            events
                                        })),
                                    },
                                    operation_state: Box::new(operation_state),
                                    lane: Some(LanePatch { tip_id: Some(tip), inbox: Some(inbox), configuration: None }),
                                });
                            }
                            let Some(tip) = state.tip_id.clone() else { return Err(session_invariant_error("Failed run has no Branch tip")); };
                            let error = if is_declined { operation_error("compaction_declined", "Overflow compaction was declined", None) } else { failure_message.clone().map_or_else(|| operation_error("structural_failed", "Structural summary failed", None), |message| operation_error("structural_failed", message, None)) };
                            let cleanup = operation_cleanup_writes(reader, &operation_id, &current, &drive_context).await?;
                            let record = operation_result_record(&meta, TerminalStatus::Failed, Some(tip.clone()), Some(error.clone())).map_err(|e| session_invariant_error(e.to_string()))?;
                            writes.extend(cleanup);
                            let reason = compaction_reason(&task).map_or("manual", reason_str).to_owned();
                            let (name, id) = (lane_name.clone(), operation_id.clone());
                            let end = HarnessEvent::new(HarnessEventPayload::RunEnd(RunEndPayload { run_id: id.clone(), from_tip_id: meta.source_tip_id.clone(), tip_id: Some(tip), ended_at: record.ended_at, status: "failed".into(), error: Some(error) }), Some(name.clone()));
                            let compaction_end = HarnessEvent::new(HarnessEventPayload::CompactionEnd { run_id: id.clone(), reason, ended_at: record.ended_at, status: if is_declined { "declined".into() } else { "failed".into() }, entry_id: None }, Some(name.clone()));
                            let outcome_record = record.clone();
                            return Ok(OperationCommand::Finish { decision: Box::new(crate::harness::runtime::lane::FinishDecision { writes, record, lane: None, materialize: Arc::new(move |_| ProcedureResult::Settled { outcome: outcome_record.clone() }), events: Some(Arc::new(move |_| vec![compaction_end.clone(), end.clone()])) }) });
                        }
                        ResultBoundary::Finish => {
                            if is_branch_result {
                                return Err(session_invariant_error("Compaction finish boundary received a branch summary"));
                            }
                            if !is_compaction_result && state.tip_id.is_none() {
                                return Err(session_invariant_error("Standalone compaction has no Branch tip"));
                            }
                            let status = if is_compaction_result { TerminalStatus::Completed } else if is_declined { TerminalStatus::Declined } else { TerminalStatus::Failed };
                            let error = if matches!(status, TerminalStatus::Failed) { failure_message.clone().map(|message| operation_error("structural_failed", message, None)) } else { None };
                            let cleanup = operation_cleanup_writes(reader, &operation_id, &current, &drive_context).await?;
                            let record = operation_result_record(&meta, status, terminal_tip_id.clone(), error).map_err(|e| session_invariant_error(e.to_string()))?;
                            writes.extend(cleanup);
                            let (name, id) = (lane_name.clone(), operation_id.clone());
                            let events_arc = events.clone();
                            let lane_patch = if is_compaction_result { Some(LanePatch { tip_id: Some(terminal_tip_id.clone()), inbox: None, configuration: None }) } else { None };
                            let outcome_record = record.clone();
                            return Ok(OperationCommand::Finish { decision: Box::new(crate::harness::runtime::lane::FinishDecision { writes, record, lane: lane_patch, materialize: Arc::new(move |_| ProcedureResult::Settled { outcome: outcome_record.clone() }), events: Some(Arc::new(move |commit| (events_arc)(commit))) }) });
                        }
                        ResultBoundary::CommitNavigation { .. } => {
                            if is_compaction_result {
                                return Err(session_invariant_error("Navigation boundary received a compaction result"));
                            }
                            let status = if is_branch_result { TerminalStatus::Completed } else if is_declined { TerminalStatus::Declined } else { TerminalStatus::Failed };
                            let error = if matches!(status, TerminalStatus::Failed) { failure_message.clone().map(|message| operation_error("structural_failed", message, None)) } else { None };
                            let cleanup = operation_cleanup_writes(reader, &operation_id, &current, &drive_context).await?;
                            let record = operation_result_record(&meta, status, terminal_tip_id.clone(), error).map_err(|e| session_invariant_error(e.to_string()))?;
                            writes.extend(cleanup);
                            let (name, id) = (lane_name.clone(), operation_id.clone());
                            let from_tip = meta.source_tip_id.clone();
                            let tip = terminal_tip_id.clone();
                            let ended_at = record.ended_at;
                            let status_text = if is_branch_result { "completed" } else if is_declined { "declined" } else { "failed" };
                            let lane_patch = if is_branch_result { Some(LanePatch { tip_id: Some(terminal_tip_id.clone()), inbox: None, configuration: None }) } else { None };
                            let outcome_record = record.clone();
                            return Ok(OperationCommand::Finish { decision: Box::new(crate::harness::runtime::lane::FinishDecision { writes, record, lane: lane_patch, materialize: Arc::new(move |_| ProcedureResult::Settled { outcome: outcome_record.clone() }), events: Some(Arc::new(move |_| vec![HarnessEvent::new(HarnessEventPayload::NavigationEnd(crate::harness::events::NavigationEndPayload { run_id: id.clone(), from_tip_id: from_tip.clone(), tip_id: tip.clone(), ended_at, status: status_text.into(), error: None }), Some(name.clone()))])) }) });
                        }
                    }
                })
            },
            &drive.context,
        )
        .await?;
    match published {
        LaneContinue::CancelRequested => Ok(ProcedureResult::Continue),
        LaneContinue::Result { value } => match value {
            StructuralPublication::Procedure(result) => Ok(result),
            StructuralPublication::FinishPending(entry_ids) => {
                finish_run_boundary(lane.as_ref(), drive, state, true, &entry_ids, Vec::new()).await
            }
        },
    }
}

/// Pinned `runStructuralDecision`: consume the durable preparation and run the decision hook.
async fn run_structural_decision(
    lane: &Arc<Lane>,
    drive: &Drive,
    deciding: &SummaryDecidingOperation,
) -> Result<ProcedureResult, SessionError> {
    let preparation = read_structural_preparation(lane, drive, deciding).await?;
    let preparation = match preparation {
        LaneContinue::CancelRequested => return Ok(ProcedureResult::Continue),
        LaneContinue::Result { value } => value,
    };
    let deciding_state = OperationState::SummaryDeciding(deciding.clone());
    match &deciding.task.boundary {
        ResultBoundary::CommitNavigation { target_id, .. } => {
            let StructuralPreparationView::Branch(preparation) = &preparation else {
                return Err(session_invariant_error("Navigation task has invalid durable preparation"));
            };
            let mut invocation = HookInvocation::new(lane.name.clone(), drive.operation_id.clone());
            invocation.target_id = Some(target_id.clone());
            invocation.preparation = Some(serde_json::to_value(durable_branch_preparation(preparation)).map_err(|error| session_invariant_error(error.to_string()))?);
            invocation.custom_instructions = deciding.task.custom_instructions.clone();
            let hook = lane.hooks.run_with_gate(HookName::BeforeNavigation, invocation, &drive.gate, &drive.context).await.map_err(|error| session_invariant_error(error.to_string()))?;
            match hook {
                HookResult::Structural { decline: Some(true), .. } => return publish_structural_outcome(lane, drive, &deciding_state, StructuralOutcome::Declined).await,
                HookResult::Structural { value: Some(value), .. } => {
                    let result: BranchSummaryResult = serde_json::from_value(value).map_err(|error| session_invariant_error(error.to_string()))?;
                    let result_entry_id = (lane.session.id_generator())(None);
                    return publish_structural_outcome(lane, drive, &deciding_state, StructuralOutcome::BranchSummary { result_entry_id, result, from_hook: true }).await;
                }
                _ => {}
            }
            publish_structural_ready(lane, drive, deciding).await
        }
        _ => {
            let StructuralPreparationView::Compaction(preparation) = &preparation else {
                return Err(session_invariant_error("Compaction task has invalid durable preparation"));
            };
            let mut invocation = HookInvocation::new(lane.name.clone(), drive.operation_id.clone());
            invocation.preparation = Some(serde_json::to_value(durable_compaction_preparation(preparation)).map_err(|error| session_invariant_error(error.to_string()))?);
            invocation.custom_instructions = deciding.task.custom_instructions.clone();
            let hook = lane.hooks.run_with_gate(HookName::BeforeCompaction, invocation, &drive.gate, &drive.context).await.map_err(|error| session_invariant_error(error.to_string()))?;
            match hook {
                HookResult::Structural { decline: Some(true), .. } => return publish_structural_outcome(lane, drive, &deciding_state, StructuralOutcome::Declined).await,
                HookResult::Structural { value: Some(value), .. } => {
                    let result: CompactResult = serde_json::from_value(value).map_err(|error| session_invariant_error(error.to_string()))?;
                    let result_entry_id = (lane.session.id_generator())(None);
                    return publish_structural_outcome(lane, drive, &deciding_state, StructuralOutcome::Compaction { result_entry_id, result, from_hook: true }).await;
                }
                _ => {}
            }
            publish_structural_ready(lane, drive, deciding).await
        }
    }
}

fn retry_wait_from_effect(effect: &SummaryEffectPendingOperation, error_message: &str) -> SummaryRetryWaitOperation {
    let policy = retry_policy_from_normalized(&effect.pending.scope.summary_context.retry_policy);
    SummaryRetryWaitOperation {
        operation: effect.operation.clone(),
        at: crate::harness::session::types::OperationMarker::SummaryRetryWait,
        retry: SummaryGenerationRetryWait {
            scope: effect.pending.scope.clone(),
            retry_wait: crate::harness::session::types::RetryWait {
                next_attempt: effect.pending.attempt.saturating_add(1),
                not_before: retry_not_before_now(&policy, effect.pending.attempt),
                error_message: error_message.to_owned(),
            },
        },
    }
}

/// Pinned `performStructuralAttempt`'s `SummaryRequest`: one nested provider request per attempt.
struct HarnessStructuralRequest<'a> {
    lane: &'a Arc<Lane>,
    drive: &'a Drive,
    effect: &'a SummaryEffectPendingOperation,
    model: &'a maho_ai::model::Model,
    cancelled: Arc<AtomicBool>,
    request_index: Mutex<usize>,
    last_response: Mutex<Option<AssistantMessage>>,
}

impl SummaryRequest for HarnessStructuralRequest<'_> {
    fn request<'a>(
        &'a self,
        ai_context: &'a AiContext,
        options: SimpleStreamOptions,
        request_context: &'a Context,
    ) -> BoxFuture<'a, AssistantMessage> {
        Box::pin(async move {
            let mut base_options = self.effect.pending.scope.summary_context.stream_options.clone();
            base_options.deferred = Some(DeferredOption::Enabled(false));
            let mut invocation = HookInvocation::new(self.lane.name.clone(), self.drive.operation_id.clone());
            invocation.model = Some(self.model.clone());
            invocation.step = Some(summary_kind(&self.effect.pending.scope.task).to_owned());
            invocation.attempt = Some(self.effect.pending.attempt);
            invocation.stream_options = base_options.clone();
            let before_request = match self.lane.hooks.run_with_gate(HookName::BeforeRequest, invocation, &self.drive.gate, &self.drive.context).await {
                Ok(result) => result,
                Err(HookRunError::Gate(GateRefusal::Abort(abort))) => {
                    self.cancelled.store(true, Ordering::SeqCst);
                    abort.cancellation.wait().await;
                    return cancelled_message(self.model);
                }
                Err(_) => {
                    self.cancelled.store(true, Ordering::SeqCst);
                    return cancelled_message(self.model);
                }
            };
            let mut stream_options = match before_request {
                HookResult::BeforeRequest { stream_options } => apply_stream_options_patch(&base_options, &stream_options),
                _ => base_options,
            };
            stream_options.deferred = Some(DeferredOption::Enabled(false));
            let usage_id = (self.lane.session.id_generator())(None);
            let index = {
                let mut index = self.request_index.lock().unwrap_or_else(|error| error.into_inner());
                let current = *index;
                *index += 1;
                current
            };
            let intent = publish_nested_request_intent(self.lane.as_ref(), self.drive, index, usage_id.clone()).await;
            if !matches!(intent, Ok(LaneContinue::Result { .. })) {
                self.cancelled.store(true, Ordering::SeqCst);
                return cancelled_message(self.model);
            }
            let admitted_context = with_abort_signal(self.drive.gate.signal(), request_context);
            let mut request_options = request_stream_options(options, &stream_options, &admitted_context);
            // Pinned `before_payload` hook: the request's `onPayload` runs the gate-guarded hook.
            {
                let gate = self.drive.gate.clone();
                let hook_context = self.drive.context.clone();
                let lane_name = self.lane.name.clone();
                let operation_id = self.drive.operation_id.clone();
                let hooks = self.lane.hooks.clone();
                request_options.stream.request.async_on_payload = Some(Arc::new(
                    move |payload: serde_json::Value, model: maho_ai::model::Model, _metadata: Option<maho_ai::types::ProviderRequestMetadata>| {
                        let gate = gate.clone();
                        let context = hook_context.clone();
                        let lane_name = lane_name.clone();
                        let operation_id = operation_id.clone();
                        let hooks = hooks.clone();
                        Box::pin(async move {
                            let mut invocation = HookInvocation::new(lane_name, operation_id);
                            invocation.model = Some(model);
                            invocation.payload = Some(payload);
                            match hooks.run_with_gate(HookName::BeforePayload, invocation, &gate, &context).await {
                                Ok(HookResult::BeforePayload { payload }) => Ok(Some(payload)),
                                Ok(_) => Ok(None),
                                Err(error) => Err(error.to_string()),
                            }
                        })
                    },
                ));
            }
            let response = match self.lane.models.complete_simple(self.model, ai_context, Some(request_options), ModelsRequestTransforms::default()).await {
                Ok(message) => message,
                Err(_) => {
                    self.cancelled.store(true, Ordering::SeqCst);
                    return cancelled_message(self.model);
                }
            };
            let _ = publish_nested_request_outcome(self.lane.as_ref(), self.drive, usage_id, response.usage).await;
            *self.last_response.lock().unwrap_or_else(|error| error.into_inner()) = Some(response.clone());
            response
        })
    }
}

/// Pinned `performStructuralAttempt`: run one compaction or branch-summary provider attempt.
async fn perform_structural_attempt(
    lane: &Arc<Lane>,
    drive: &Drive,
    effect: &SummaryEffectPendingOperation,
    model: &maho_ai::model::Model,
    preparation: &StructuralPreparationView,
) -> Result<StructuralAttempt, SessionError> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let request = HarnessStructuralRequest {
        lane,
        drive,
        effect,
        model,
        cancelled: cancelled.clone(),
        request_index: Mutex::new(0),
        last_response: Mutex::new(None),
    };
    let retryable = |request: &HarnessStructuralRequest<'_>| {
        request.last_response.lock().unwrap_or_else(|error| error.into_inner()).as_ref().is_some_and(is_retryable_assistant_error)
    };
    if summary_kind(&effect.pending.scope.task) == "compaction" {
        let StructuralPreparationView::Compaction(preparation) = preparation else {
            return Err(session_invariant_error("Compaction summary has invalid durable preparation"));
        };
        let options = CompactGenerationOptions {
            model: model.clone(),
            custom_instructions: effect.pending.scope.task.custom_instructions.clone(),
            thinking_level: thinking_level_of(effect.pending.scope.summary_context.configuration.thinking_level),
        };
        let outcome = compact_with_request(preparation, &options, &request, &drive.context).await;
        if cancelled.load(Ordering::SeqCst) {
            return Ok(StructuralAttempt::CancelRequested);
        }
        return Ok(match outcome {
            Ok(result) => StructuralAttempt::Compaction { result, retryable: retryable(&request) },
            Err(error) => StructuralAttempt::Error { error: operation_error(error.code.as_str(), error.message, None), retryable: retryable(&request) },
        });
    }
    let StructuralPreparationView::Branch(preparation) = preparation else {
        return Err(session_invariant_error("Branch summary has invalid durable preparation"));
    };
    let options = PreparedBranchSummaryOptions { custom_instructions: effect.pending.scope.task.custom_instructions.clone(), replace_instructions: false };
    let outcome = generate_branch_summary_with_request(preparation, &options, &request, &drive.context).await;
    if cancelled.load(Ordering::SeqCst) {
        return Ok(StructuralAttempt::CancelRequested);
    }
    Ok(match outcome {
        Ok(result) => StructuralAttempt::BranchSummary { result, retryable: retryable(&request) },
        Err(error) => StructuralAttempt::Error { error: operation_error(error.code.as_str(), error.message, None), retryable: retryable(&request) },
    })
}

async fn read_attempt_preparation(
    lane: &Arc<Lane>,
    drive: &Drive,
    ready: &SummaryReadyOperation,
) -> Result<LaneContinue<StructuralPreparationView>, SessionError> {
    let this = lane.clone();
    let drive_context = drive.context.clone();
    let operation_id = drive.operation_id.clone();
    let _ = ready;
    this.continue_operation(
        move |_state, current, _meta, reader| {
            Box::pin(async move {
                let OperationState::SummaryReady(ready) = current else {
                    return Err(session_invariant_error("Structural attempt requires a summary.ready operation"));
                };
                let expected = summary_kind(&ready.ready.scope.task);
                let stored = reader
                    .get_value(&operation_preparation(&operation_id, &ready.ready.scope.task.task_id), &drive_context)
                    .await?;
                let Some(stored) = stored else {
                    return Err(session_invariant_error(format!("Structural task {} has invalid durable preparation", ready.ready.scope.task.task_id)));
                };
                let durable: DurableStructuralPreparation = serde_json::from_value(stored.value)
                    .map_err(|error| session_invariant_error(error.to_string()))?;
                if durable_summary_kind(&durable) != expected {
                    return Err(session_invariant_error(format!("Structural task {} has invalid durable preparation", ready.ready.scope.task.task_id)));
                }
                let view = match &durable {
                    DurableStructuralPreparation::Compaction { .. } => StructuralPreparationView::Compaction(compaction_preparation(&durable)?),
                    DurableStructuralPreparation::BranchSummary { .. } => StructuralPreparationView::Branch(branch_preparation(&durable)?),
                };
                Ok(OperationCommand::Return { result: view })
            })
        },
        &drive.context,
    )
    .await
}

async fn publish_attempt_result(
    lane: &Arc<Lane>,
    drive: &Drive,
    state: &OperationState,
    effect: &SummaryEffectPendingOperation,
    result: StructuralAttempt,
) -> Result<ProcedureResult, SessionError> {
    match result {
        StructuralAttempt::CancelRequested => Ok(ProcedureResult::Continue),
        StructuralAttempt::Compaction { result, .. } => {
            publish_structural_outcome(lane, drive, state, StructuralOutcome::Compaction { result_entry_id: effect.pending.scope.summary_context.result_entry_id.clone(), result, from_hook: false }).await
        }
        StructuralAttempt::BranchSummary { result, .. } => {
            publish_structural_outcome(lane, drive, state, StructuralOutcome::BranchSummary { result_entry_id: effect.pending.scope.summary_context.result_entry_id.clone(), result, from_hook: false }).await
        }
        StructuralAttempt::Error { error, retryable } => {
            let control_running = lane.state().operation.as_ref().is_some_and(|operation| matches!(operation.state.operation_scope_of().control, Control::Running));
            if error.code == "aborted" && control_running {
                return Err(session_invariant_error("Structural provider response is aborted while durable control is running"));
            }
            if retryable && effect.pending.attempt < effect.pending.scope.summary_context.retry_policy.max_attempts {
                let retry_wait = retry_wait_from_effect(effect, &error.message);
                let policy = retry_policy_from_normalized(&effect.pending.scope.summary_context.retry_policy);
                let event = HarnessEvent::new(
                    HarnessEventPayload::RetryScheduled { run_id: drive.operation_id.clone(), step: effect.pending.scope.task.task_id.clone(), attempt: retry_wait.retry.retry_wait.next_attempt, max_attempts: effect.pending.scope.summary_context.retry_policy.max_attempts, delay_ms: retry_delay_ms(&policy, effect.pending.attempt), not_before: retry_wait.retry.retry_wait.not_before, error_message: error.message },
                    Some(lane.name.clone()),
                );
                let published = lane
                    .continue_operation(
                        move |_state, _current, _meta, _reader| {
                            Box::pin(async move {
                                Ok(OperationCommand::Commit {
                                    decision: crate::harness::runtime::lane::CommitDecision { writes: Vec::new(), materialize: Arc::new(|_| ProcedureResult::Continue), events: Some(Arc::new(move |_| vec![event.clone()])) },
                                    operation_state: Box::new(OperationState::SummaryRetryWait(retry_wait)),
                                    lane: None,
                                })
                            })
                        },
                        &drive.context,
                    )
                    .await?;
                return Ok(match published { LaneContinue::CancelRequested => ProcedureResult::Continue, LaneContinue::Result { value } => value });
            }
            publish_structural_outcome(lane, drive, state, StructuralOutcome::Failed { error }).await
        }
    }
}

/// Pinned `runStructuralGeneration`: prepare, then run one ready attempt.
async fn run_structural_generation(
    lane: &Arc<Lane>,
    drive: &Drive,
    ready: &SummaryReadyOperation,
) -> Result<ProcedureResult, SessionError> {
    let preparation = read_attempt_preparation(lane, drive, ready).await?;
    let preparation = match preparation {
        LaneContinue::CancelRequested => return Ok(ProcedureResult::Continue),
        LaneContinue::Result { value } => value,
    };
    let identity = ready.ready.scope.summary_context.configuration.model.clone();
    let Some(model) = lane.models.get_model(&identity.provider, &identity.model_id) else {
        return publish_structural_outcome(lane, drive, &OperationState::SummaryReady(ready.clone()), StructuralOutcome::Failed { error: operation_error("model_unavailable", "The configured model is unavailable in this process", Some(serde_json::json!(identity))) }).await;
    };
    let intent = publish_attempt_intent(lane.as_ref(), drive).await?;
    let intent = match intent {
        LaneContinue::CancelRequested => return Ok(ProcedureResult::Continue),
        LaneContinue::Result { value } => value,
    };
    let result = perform_structural_attempt(lane, drive, &intent, &model, &preparation).await?;
    publish_attempt_result(lane, drive, &OperationState::SummaryEffectPending(intent.clone()), &intent, result).await
}

/// Pinned `runStructuralRetryWait`: consume one structural retry wait without a provider effect.
async fn run_structural_retry_wait(
    lane: &Arc<Lane>,
    drive: &Drive,
    retry: &SummaryRetryWaitOperation,
) -> Result<ProcedureResult, SessionError> {
    if now_ms() < retry.retry.retry_wait.not_before {
        if !drive.wait_for_retry {
            return Ok(ProcedureResult::Waiting { outcome: crate::harness::agent_harness::DriveOutcome::WaitingRetry { operation_id: drive.operation_id.clone(), not_before: retry.retry.retry_wait.not_before } });
        }
        drive.gate.admit(|| ()).map_err(|error| session_invariant_error(error.to_string()))?;
        wait_until(retry.retry.retry_wait.not_before, &drive.gate.signal()).await.map_err(|error| session_invariant_error(error.message))?;
    }
    let step = retry.retry.scope.task.task_id.clone();
    let attempt = retry.retry.retry_wait.next_attempt;
    let run_id = drive.operation_id.clone();
    let ready = SummaryReadyOperation { operation: retry.operation.clone(), at: crate::harness::session::types::OperationMarker::SummaryReady, ready: SummaryGenerationReady { scope: retry.retry.scope.clone(), next_attempt: attempt } };
    let published = lane
        .continue_operation(
            move |_state, _current, _meta, _reader| {
                Box::pin(async move {
                    let event = HarnessEvent::new(HarnessEventPayload::RetryStart { run_id: run_id.clone(), step: step.clone(), attempt }, None);
                    Ok(OperationCommand::Commit {
                        decision: crate::harness::runtime::lane::CommitDecision { writes: Vec::new(), materialize: Arc::new(|_| ProcedureResult::Continue), events: Some(Arc::new(move |_| vec![event.clone()])) },
                        operation_state: Box::new(OperationState::SummaryReady(ready)),
                        lane: None,
                    })
                })
            },
            &drive.context,
        )
        .await?;
    Ok(match published { LaneContinue::CancelRequested => ProcedureResult::Continue, LaneContinue::Result { value } => value })
}

/// Pinned `recoverStructuralGeneration`: convert an orphaned attempt into a retry or a failure.
async fn recover_structural_generation(
    lane: &Arc<Lane>,
    drive: &Drive,
    effect: &SummaryEffectPendingOperation,
) -> Result<ProcedureResult, SessionError> {
    let error = operation_error("structural_interrupted", "Structural summary attempt was interrupted and its external outcome is unknown", None);
    if effect.pending.attempt >= effect.pending.scope.summary_context.retry_policy.max_attempts {
        return publish_structural_outcome(lane, drive, &OperationState::SummaryEffectPending(effect.clone()), StructuralOutcome::Failed { error }).await;
    }
    let retry_wait = retry_wait_from_effect(effect, &error.message);
    let policy = retry_policy_from_normalized(&effect.pending.scope.summary_context.retry_policy);
    let event = HarnessEvent::new(
        HarnessEventPayload::RetryScheduled { run_id: drive.operation_id.clone(), step: effect.pending.scope.task.task_id.clone(), attempt: retry_wait.retry.retry_wait.next_attempt, max_attempts: effect.pending.scope.summary_context.retry_policy.max_attempts, delay_ms: retry_delay_ms(&policy, effect.pending.attempt), not_before: retry_wait.retry.retry_wait.not_before, error_message: error.message },
        Some(lane.name.clone()),
    );
    let published = lane
        .continue_operation(
            move |_state, _current, _meta, _reader| {
                Box::pin(async move {
                    let mut event = event.clone();
                    event.recovery = Some(true);
                    Ok(OperationCommand::Commit {
                        decision: crate::harness::runtime::lane::CommitDecision { writes: Vec::new(), materialize: Arc::new(|_| ProcedureResult::Continue), events: Some(Arc::new(move |_| vec![event.clone()])) },
                        operation_state: Box::new(OperationState::SummaryRetryWait(retry_wait)),
                        lane: None,
                    })
                })
            },
            &drive.context,
        )
        .await?;
    Ok(match published { LaneContinue::CancelRequested => ProcedureResult::Continue, LaneContinue::Result { value } => value })
}

/// Pinned `drive.ts` structural dispatch for one durable structural operation state.
pub async fn run_structural(
    lane: &Arc<Lane>,
    drive: &Drive,
    state: OperationState,
) -> Result<ProcedureResult, SessionError> {
    match state {
        OperationState::SummaryDeciding(deciding) => run_structural_decision(lane, drive, &deciding).await,
        OperationState::SummaryReady(ready) => run_structural_generation(lane, drive, &ready).await,
        OperationState::SummaryEffectPending(effect) => recover_structural_generation(lane, drive, &effect).await,
        OperationState::SummaryRetryWait(retry) => run_structural_retry_wait(lane, drive, &retry).await,
        OperationState::NavigationReadyToCommit(_) => commit_navigation(lane.as_ref(), drive),
        _ => Err(session_invariant_error("Structural execution requires a structural operation")),
    }
}

/// Pinned `prepareCompactionThreshold`: prepare threshold compaction unless a newer one guards it.
pub async fn prepare_compaction_threshold(
    lane: &Arc<Lane>,
    drive: &Drive,
    state: &OperationState,
) -> Result<LaneContinue<Option<StructuralPreparation>>, SessionError> {
    let OperationState::Checkpoint(checkpoint) = state else {
        return Err(session_invariant_error("Threshold compaction requires a checkpoint operation"));
    };
    let settings = checkpoint.operation.settings.compaction;
    let identity = lane.state().configuration.model;
    let Some(model) = lane.models.get_model(&identity.provider, &identity.model_id) else {
        return Ok(LaneContinue::Result { value: None });
    };
    if !settings.enabled {
        return Ok(LaneContinue::Result { value: None });
    }
    let path = match read_bounded_entries(lane.as_ref(), drive, state).await? {
        LaneContinue::CancelRequested => return Ok(LaneContinue::CancelRequested),
        LaneContinue::Result { value } => value,
    };
    let trigger_index = path.iter().position(|entry| entry.id == checkpoint.checkpoint.trigger_entry_id);
    let mut newest_compaction_index = None;
    for index in (0..path.len()).rev() {
        if matches!(path[index].kind, EntryKind::Compaction { .. }) {
            newest_compaction_index = Some(index);
            break;
        }
    }
    if let Some(newest) = newest_compaction_index
        && trigger_index.is_some_and(|trigger| newest >= trigger) {
        return Ok(LaneContinue::Result { value: None });
    }
    let Some(_trigger_index) = trigger_index else {
        return Err(session_invariant_error(format!("Checkpoint trigger {} is missing from its Branch", checkpoint.checkpoint.trigger_entry_id)));
    };
    let prepared = prepare_compaction(&path, settings).map_err(|error| session_invariant_error(error.to_string()))?;
    let Some(prepared) = prepared else {
        return Ok(LaneContinue::Result { value: None });
    };
    if !should_compact(prepared.tokens_before.max(0) as u64, model.context_window, &settings) {
        return Ok(LaneContinue::Result { value: None });
    }
    let task_id = (lane.session.id_generator())(None);
    Ok(LaneContinue::Result {
        value: Some(StructuralPreparation { task_id, preparation: serde_json::to_value(durable_compaction_preparation(&prepared)).map_err(|error| session_invariant_error(error.to_string()))? }),
    })
}

/// Pinned `prepareOverflowCompaction`: prepare one overflow compaction before response settlement.
pub async fn prepare_overflow_compaction(
    lane: &Arc<Lane>,
    drive: &Drive,
    state: &OperationState,
) -> Result<Option<StructuralPreparation>, SessionError> {
    let OperationState::AssistantEffectPending(generation) = state else {
        return Ok(None);
    };
    if generation.assistant.generation_context.overflow_recovery_used {
        return Ok(None);
    }
    let path = match read_bounded_entries(lane.as_ref(), drive, state).await? {
        LaneContinue::CancelRequested => return Ok(None),
        LaneContinue::Result { value } => value,
    };
    let prepared = prepare_compaction(&path, generation.operation.settings.compaction).map_err(|error| session_invariant_error(error.to_string()))?;
    let Some(prepared) = prepared else {
        return Ok(None);
    };
    let task_id = (lane.session.id_generator())(None);
    Ok(Some(StructuralPreparation { task_id, preparation: serde_json::to_value(durable_compaction_preparation(&prepared)).map_err(|error| session_invariant_error(error.to_string()))? }))
}
