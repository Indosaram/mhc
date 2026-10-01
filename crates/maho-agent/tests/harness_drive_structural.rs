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
