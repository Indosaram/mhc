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
