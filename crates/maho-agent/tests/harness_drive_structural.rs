use maho_agent::harness::compaction::branch_summarization::BranchPreparation;
use maho_agent::harness::compaction::compaction::{
    CompactionPreparation, DEFAULT_COMPACTION_SETTINGS,
};
use maho_agent::harness::compaction::utils::FileOperations;
use maho_agent::harness::runtime::structural::*;
use maho_agent::harness::session::types::{ResultBoundary, SummaryTask, SummaryTaskReason};

#[test]
fn compaction_preparation_preserves_all_durable_fields() {
    let preparation = CompactionPreparation {
        messages_to_summarize: vec![],
        turn_prefix_messages: vec![],
        retained_tail: vec![],
        is_split_turn: true,
        tokens_before: 123,
        previous_summary: Some("previous".into()),
        file_ops: FileOperations {
            read: ["a".into()].into(),
            written: ["b".into()].into(),
            edited: ["c".into()].into(),
        },
        settings: DEFAULT_COMPACTION_SETTINGS,
    };
    let durable = durable_compaction_preparation(&preparation);
    let restored = compaction_preparation(&durable).unwrap();
    assert_eq!(durable_compaction_preparation(&restored), durable);
    assert!(branch_preparation(&durable).is_err());
}

#[test]
fn branch_preparation_preserves_files_and_token_count() {
    let preparation = BranchPreparation {
        messages: vec![],
        file_ops: FileOperations {
            read: ["a".into()].into(),
            written: ["b".into()].into(),
            edited: ["c".into()].into(),
        },
        total_tokens: 45,
    };
    let durable = durable_branch_preparation(&preparation);
    let restored = branch_preparation(&durable).unwrap();
    assert_eq!(durable_branch_preparation(&restored), durable);
    assert!(compaction_preparation(&durable).is_err());
}

#[test]
fn navigation_task_selects_branch_summary_and_preserves_label() {
    let task = SummaryTask {
        task_id: "task".into(),
        reason: None,
        custom_instructions: None,
        boundary: ResultBoundary::CommitNavigation {
            target_id: "target".into(),
            label: Some("label".into()),
        },
    };
    assert_eq!(summary_kind(&task), "branch_summary");
    assert_eq!(
        navigation_boundary(&task).unwrap(),
        ("target", Some("label"))
    );
    assert_eq!(
        compaction_reason(&task).unwrap_err().message,
        "In-run compaction task task is missing its reason"
    );
}

#[test]
fn standalone_compaction_defaults_to_manual_reason() {
    let task = SummaryTask {
        task_id: "task".into(),
        reason: None,
        custom_instructions: None,
        boundary: ResultBoundary::Finish,
    };
    assert_eq!(summary_kind(&task), "compaction");
    assert_eq!(compaction_reason(&task).unwrap(), SummaryTaskReason::Manual);
    assert_eq!(
        navigation_boundary(&task).unwrap_err().message,
        "Summary task task is not a navigation"
    );
}

fn fixture() -> (maho_agent::harness::runtime::lane::Lane, maho_agent::harness::runtime::types::Drive) {
    use std::sync::Arc;
    use maho_agent::harness::session::types::*;
    use maho_agent::harness::session::{MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions};
    let session = Arc::new(StorageBackedSession::new(SessionMetadata { id: "structural".into(), created_at: 1, storage_version: 1, cwd: None, parent_session_id: None, legacy_parent_session_path: None }, Arc::new(MemoryStorage::new(MemoryStorageOptions { now: Some(Arc::new(|| 10)) })), StorageBackedSessionOptions::default()));
    session.attach();
    let configuration = LaneConfiguration { model: LaneModelRef { provider: "test".into(), model_id: "model".into() }, thinking_level: maho_ai::types::ModelThinkingLevel::Off, active_tool_names: vec![] };
    let ready = SummaryReadyOperation { operation: OperationScope { control: Control::Running, latest_assistant_entry_id: None, settings: RunSettings { compaction: DEFAULT_COMPACTION_SETTINGS, steering_mode: maho_agent::types::QueueMode::All, follow_up_mode: maho_agent::types::QueueMode::All, tool_execution: ToolExecutionMode::Parallel } }, ready: SummaryGenerationReady { scope: SummaryGenerationScope { task: SummaryTask { task_id: "task".into(), reason: None, custom_instructions: None, boundary: ResultBoundary::Finish }, summary_context: SummaryContext { result_entry_id: "result".into(), configuration: configuration.clone(), stream_options: Default::default(), retry_policy: NormalizedRetryPolicy { max_attempts: 3, base_delay_ms: 1, max_agent_delay_ms: 10 } } }, next_attempt: 1 }, at: OperationMarker::SummaryReady };
    let lane = maho_agent::harness::runtime::lane::Lane::new("main".into(), session, maho_agent::harness::runtime::types::LaneState { tip_id: None, configuration, inbox: vec![], last_operation_id: None, operation: Some(Operation { meta: OperationMeta { operation_id: "op".into(), lane: "main".into(), source_tip_id: None, started_at: 1, intent: OperationIntent::Compaction { custom_instructions: None } }, state: OperationState::SummaryReady(ready) }) }, maho_agent::harness::events::HarnessEventBus::new());
    let drive = maho_agent::harness::runtime::types::Drive::new(&maho_agent::harness::agent_harness::DriveOptions { operation_id: "op".into(), wait_for_retry: None, poll_deferred: None }, &maho_agent::harness::context::BACKGROUND_CONTEXT);
    (lane, drive)
}

#[tokio::test]
async fn structural_attempt_publishes_durable_intent_before_request() {
    use maho_agent::harness::session::types::{OperationState, SessionReader};
    let (lane, drive) = fixture();
    publish_attempt_intent(&lane, &drive).await.unwrap();
    let OperationState::SummaryEffectPending(pending) = lane.state().operation.unwrap().state else { panic!("pending summary"); };
    assert_eq!(pending.pending.attempt, 1);
    assert!(pending.pending.usage_ids.is_empty());
    assert!(pending.pending.request.is_none());
    let stored = lane.session.get_value(&maho_agent::harness::session::values::operation_state("op"), &drive.context).await.unwrap().unwrap();
    assert_eq!(stored.value["at"], "summary.effect_pending");
}

#[tokio::test]
async fn nested_structural_request_has_own_durable_intent() {
    use maho_agent::harness::session::types::OperationState;
    let (lane, drive) = fixture();
    publish_attempt_intent(&lane, &drive).await.unwrap();
    publish_nested_request_intent(&lane, &drive, 2, "usage".into()).await.unwrap();
    let OperationState::SummaryEffectPending(pending) = lane.state().operation.unwrap().state else { panic!("pending summary"); };
    let request = pending.pending.request.unwrap();
    assert_eq!(request.index, 2);
    assert_eq!(request.usage_id, "usage");
}

#[tokio::test]
async fn nested_request_settlement_records_usage_and_clears_intent() {
    use maho_agent::harness::session::types::{OperationState, SessionReader};
    let (lane, drive) = fixture();
    publish_attempt_intent(&lane, &drive).await.unwrap();
    publish_nested_request_intent(&lane, &drive, 0, "usage".into()).await.unwrap();
    publish_nested_request_outcome(&lane, &drive, "usage".into(), maho_ai::types::Usage { input: 9, total_tokens: 9, ..Default::default() }).await.unwrap();
    let OperationState::SummaryEffectPending(pending) = lane.state().operation.unwrap().state else { panic!("pending summary"); };
    assert!(pending.pending.request.is_none());
    assert_eq!(pending.pending.usage_ids, vec!["usage"]);
    assert_eq!(lane.session.get_stats(&drive.context).await.unwrap().usage.input, 9);
}
