use maho_agent::harness::runtime::tools::*;
use maho_agent::types::AgentToolResult;
use maho_ai::types::{ContentBlock, ToolCall};

fn call() -> ToolCall {
    ToolCall {
        id: "call".into(),
        name: "read".into(),
        arguments: serde_json::Map::new(),
        thought_signature: None,
        namespace: None,
        incomplete: None,
        error_message: None,
    }
}

#[test]
fn interrupted_effect_preserves_durable_checkpoint_and_marks_unknown_outcome() {
    let mut checkpoint = AgentToolResult::text("latest durable output");
    checkpoint.details = serde_json::json!({"checkpoint": 1});
    let result = interrupted_outcome(call(), Some(&checkpoint), 10);
    assert_eq!(
        result.message.content,
        vec![
            ContentBlock::text("latest durable output"),
            ContentBlock::text(INTERRUPTION_MARKER)
        ]
    );
    assert_eq!(result.message.details, Some(checkpoint.details));
    assert!(result.message.is_error);
    assert!(!result.terminate);
    assert_eq!(result.message.timestamp, 10);
}

#[test]
fn interrupted_effect_without_checkpoint_emits_only_interruption_marker() {
    let result = interrupted_outcome(call(), None, 10);
    assert_eq!(
        result.message.content,
        vec![ContentBlock::text(INTERRUPTION_MARKER)]
    );
    assert_eq!(result.message.details, None);
}

#[test]
fn cancelled_planned_call_has_nonterminating_error_outcome() {
    let result = aborted_outcome(call(), 10);
    assert_eq!(
        result.message.content,
        vec![ContentBlock::text(
            "Tool execution was cancelled before completion."
        )]
    );
    assert!(result.message.is_error);
    assert!(!result.terminate);
    assert_eq!(result.message.tool_call_id, "call");
}

#[test]
fn genuine_length_response_has_truncation_error_outcome() {
    let result = truncated_outcome(call(), 10);
    assert_eq!(
        result.message.content,
        vec![ContentBlock::text(
            "Tool call \"read\" was not executed because the assistant response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments."
        )]
    );
    assert!(result.message.is_error);
    assert!(!result.terminate);
}

#[test]
fn memo_names_reject_empty_and_address_separator() {
    assert!(validate_memo_name("checkpoint").is_ok());
    assert_eq!(
        validate_memo_name("").unwrap_err().message,
        "Tool invocation memo name must not be empty"
    );
    assert_eq!(
        validate_memo_name("bad:name").unwrap_err().message,
        "Tool invocation memo name must not contain ':'"
    );
}

fn lane(
    control: maho_agent::harness::session::types::Control,
) -> maho_agent::harness::runtime::lane::Lane {
    use maho_agent::harness::session::types::*;
    use maho_agent::harness::session::{
        MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions,
    };
    use std::sync::Arc;
    let session = Arc::new(StorageBackedSession::new(
        SessionMetadata {
            id: "runtime".into(),
            created_at: 1,
            storage_version: 1,
            cwd: None,
            parent_session_id: None,
            legacy_parent_session_path: None,
        },
        Arc::new(MemoryStorage::new(MemoryStorageOptions {
            now: Some(Arc::new(|| 10)),
        })),
        StorageBackedSessionOptions::default(),
    ));
    session.attach();
    let configuration = LaneConfiguration {
        model: LaneModelRef {
            provider: "test".into(),
            model_id: "model".into(),
        },
        thinking_level: maho_ai::types::ModelThinkingLevel::Off,
        active_tool_names: vec!["read".into()],
    };
    maho_agent::harness::runtime::lane::Lane::new("main".into(), session, maho_agent::harness::runtime::types::LaneState {
        tip_id: None, configuration: configuration.clone(), inbox: vec![], last_operation_id: None,
        operation: Some(Operation { meta: OperationMeta { operation_id: "op".into(), lane: "main".into(), source_tip_id: None, started_at: 1, intent: OperationIntent::Run { prompt_entry_ids: vec![] } }, state: OperationState::Tools(ToolsOperation {
            operation: OperationScope { control, latest_assistant_entry_id: None, settings: RunSettings { compaction: maho_agent::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS, steering_mode: maho_agent::types::QueueMode::All, follow_up_mode: maho_agent::types::QueueMode::All, tool_execution: ToolExecutionMode::Sequential } },
            batch: ToolBatch { assistant_entry_id: "assistant".into(), configuration, turn_id: "turn".into(), calls: vec![maho_agent::harness::session::types::ToolCall::Planned { source_index: 0, result_entry_id: "result".into() }] }, at: OperationMarker::Tools,
        }) }),
    }, maho_agent::harness::events::HarnessEventBus::new())
}

#[tokio::test]
async fn publishes_intent_before_effect_and_persists_arguments() {
    use maho_agent::harness::context::BACKGROUND_CONTEXT;
    use maho_agent::harness::session::types::{
        Control, SessionReader, ToolCall as DurableCall, ToolCallReplay,
    };
    use maho_agent::harness::session::values::operation_tool_args;
    let lane = lane(Control::Running);
    let call = DurableCall::Planned {
        source_index: 0,
        result_entry_id: "result".into(),
    };
    let result = publish_tool_intent(
        &lane,
        call,
        serde_json::json!({"path": "saved"}),
        ToolCallReplay::Safe,
        &BACKGROUND_CONTEXT,
    )
    .await
    .unwrap();
    assert!(matches!(
        result,
        maho_agent::harness::runtime::types::ContinueOperationResult::Result {
            value: DurableCall::EffectPending {
                replay: ToolCallReplay::Safe,
                ..
            }
        }
    ));
    assert_eq!(
        lane.session
            .get_value(&operation_tool_args("op", "turn", 0), &BACKGROUND_CONTEXT)
            .await
            .unwrap()
            .unwrap()
            .value,
        serde_json::json!({"path": "saved"})
    );
}

#[tokio::test]
async fn replay_clears_old_checkpoint_and_returns_persisted_arguments() {
    use maho_agent::harness::context::BACKGROUND_CONTEXT;
    use maho_agent::harness::session::types::{Control, Session, ToolCall as DurableCall, ToolCallReplay, Write};
    use maho_agent::harness::session::values::{pending_tool_output, set_value};
    let lane = lane(Control::Running);
    let pending = DurableCall::EffectPending { source_index: 0, result_entry_id: "result".into(), replay: ToolCallReplay::Safe };
    publish_tool_intent(&lane, pending.clone(), serde_json::json!({"path":"saved"}), ToolCallReplay::Safe, &BACKGROUND_CONTEXT).await.unwrap();
    let checkpoint = maho_agent::types::AgentToolResult { content: vec![maho_ai::types::ContentBlock::text("durable")], details: serde_json::json!({"offset":2}), usage: None, added_tool_names: None, terminate: None, is_error: None };
    let mutation = lane.session.begin_mutation(&BACKGROUND_CONTEXT).await.unwrap();
    mutation.commit(vec![Write::Value(set_value(&pending_tool_output("op", "result"), serde_json::to_value(&checkpoint).unwrap()))], &BACKGROUND_CONTEXT).await.unwrap();
    mutation.end(&BACKGROUND_CONTEXT).await;
    let drive = maho_agent::harness::runtime::types::Drive::new(&maho_agent::harness::agent_harness::DriveOptions { operation_id: "op".into(), wait_for_retry: None, poll_deferred: None }, &BACKGROUND_CONTEXT);
    assert_eq!(read_checkpoint(&lane, &drive, &pending).await.unwrap().unwrap().details, checkpoint.details);
    let maho_agent::harness::session::types::OperationState::Tools(run) = lane.state().operation.unwrap().state else { panic!("tools"); };
    let arguments = clear_replay_checkpoint(&lane, &drive, &run.batch, &pending, &call()).await.unwrap();
    assert_eq!(arguments, serde_json::json!({"path":"saved"}));
    assert!(read_checkpoint(&lane, &drive, &pending).await.unwrap().is_none());
}

#[tokio::test]
async fn cancellation_diverts_intent_without_invoking_planner() {
    use maho_agent::harness::context::BACKGROUND_CONTEXT;
    use maho_agent::harness::session::types::{
        Control, SessionReader, ToolCall as DurableCall, ToolCallReplay,
    };
    use maho_agent::harness::session::values::operation_tool_args;
    let lane = lane(Control::CancelRequested { requested_at: 2 });
    let result = publish_tool_intent(
        &lane,
        DurableCall::Planned {
            source_index: 0,
            result_entry_id: "result".into(),
        },
        serde_json::json!({}),
        ToolCallReplay::Safe,
        &BACKGROUND_CONTEXT,
    )
    .await
    .unwrap();
    assert!(matches!(
        result,
        maho_agent::harness::runtime::types::ContinueOperationResult::CancelRequested
    ));
    assert!(
        lane.session
            .get_value(&operation_tool_args("op", "turn", 0), &BACKGROUND_CONTEXT)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cancelled_settlement_clears_memos_and_checkpoint_without_terminating() {
    use maho_agent::harness::context::BACKGROUND_CONTEXT;
    use maho_agent::harness::session::types::{
        Control, OperationState, SessionReader, ToolCall as DurableCall,
    };
    use maho_agent::harness::session::values::*;
    let lane = lane(Control::CancelRequested { requested_at: 2 });
    lane.session
        .set_value(
            &operation_tool_memo("op", "result", "saved"),
            serde_json::json!(1),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap();
    lane.session
        .set_value(
            &pending_tool_output("op", "result"),
            serde_json::json!({}),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap();
    let mut outcome = aborted_outcome(call(), 10);
    outcome.terminate = true;
    publish_tool_outcome(
        &lane,
        DurableCall::Planned {
            source_index: 0,
            result_entry_id: "result".into(),
        },
        outcome,
        &BACKGROUND_CONTEXT,
    )
    .await
    .unwrap();
    let OperationState::Tools(run) = lane.state().operation.unwrap().state else {
        panic!("tools");
    };
    assert!(matches!(
        run.batch.calls[0],
        DurableCall::OutcomeReady {
            terminate: false,
            ..
        }
    ));
    assert!(
        lane.session
            .get_value(
                &operation_tool_memo("op", "result", "saved"),
                &BACKGROUND_CONTEXT
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        lane.session
            .get_value(&pending_tool_output("op", "result"), &BACKGROUND_CONTEXT)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        lane.session
            .get_value(&pending_entry("result"), &BACKGROUND_CONTEXT)
            .await
            .unwrap()
            .is_some()
    );
}
