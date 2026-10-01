use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::runtime::harness::{Harness, create_agent_harness};
use maho_agent::harness::runtime::lane::{CommitDecision, LaneCommand};
use maho_agent::harness::session::types::{
    LaneConfiguration, LaneModelRef, Session, SessionMetadata, SessionReader, Write,
};
use maho_agent::harness::session::values::{lane_config, set_value};
use maho_agent::harness::session::{
    MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions,
};
use maho_ai::types::ModelThinkingLevel;
use std::sync::Arc;

fn seed() -> LaneConfiguration {
    LaneConfiguration {
        model: LaneModelRef {
            provider: "test".into(),
            model_id: "model".into(),
        },
        thinking_level: ModelThinkingLevel::Off,
        active_tool_names: vec![],
    }
}

async fn fixture() -> Harness {
    let storage = Arc::new(MemoryStorage::new(MemoryStorageOptions {
        now: Some(Arc::new(|| 10)),
    }));
    let session = Arc::new(StorageBackedSession::new(
        SessionMetadata {
            id: "runtime".into(),
            created_at: 1,
            storage_version: 1,
            cwd: None,
            parent_session_id: None,
            legacy_parent_session_path: None,
        },
        storage,
        StorageBackedSessionOptions::default(),
    ));
    session.attach();
    create_agent_harness(session, seed(), &BACKGROUND_CONTEXT)
        .await
        .expect("fresh harness attaches")
}

#[tokio::test]
async fn reads_and_replaces_configuration_from_owned_state() {
    let harness = fixture().await;
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    lane.set_model(
        LaneModelRef {
            provider: "test".into(),
            model_id: "updated".into(),
        },
        &BACKGROUND_CONTEXT,
    )
    .await
    .unwrap();
    lane.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    lane.set_active_tools(vec!["read".into()], &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(lane.get_thinking_level().unwrap(), ModelThinkingLevel::High);
    assert_eq!(lane.get_active_tools().unwrap(), vec!["read"]);
    let stored = harness
        .session
        .get_value(&lane_config("main"), &BACKGROUND_CONTEXT)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.value,
        serde_json::json!({ "model": { "provider": "test", "modelId": "updated" }, "thinkingLevel": "high", "activeToolNames": ["read"] })
    );
    harness.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn derives_concurrent_configuration_updates_from_latest_state() {
    let harness = fixture().await;
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let (model, thinking) = tokio::join!(
        lane.set_model(
            LaneModelRef {
                provider: "test".into(),
                model_id: "updated".into()
            },
            &BACKGROUND_CONTEXT
        ),
        lane.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT)
    );
    model.unwrap();
    thinking.unwrap();
    assert_eq!(lane.state().configuration.model.model_id, "updated");
    assert_eq!(
        lane.state().configuration.thinking_level,
        ModelThinkingLevel::High
    );
}

#[tokio::test]
async fn returns_future_value_without_holding_session_line() {
    let harness = fixture().await;
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    let pending = lane
        .command(
            move |_, _| Box::pin(async move { Ok(LaneCommand::Return { result: receive }) }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap();
    lane.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(lane.get_thinking_level().unwrap(), ModelThinkingLevel::High);
    send.send(()).unwrap();
    pending.await.unwrap();
}

#[tokio::test]
async fn expected_rejection_does_not_fault_lane() {
    let harness = fixture().await;
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let result = lane
        .command::<(), _>(
            |_, _| {
                Box::pin(async {
                    Ok(LaneCommand::Reject {
                        error: "declined".into(),
                    })
                })
            },
            &BACKGROUND_CONTEXT,
        )
        .await;
    assert_eq!(result.unwrap_err().message, "declined");
    assert_eq!(lane.get_tip_id().unwrap(), None);
    lane.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
}

#[tokio::test]
async fn passes_bounded_reads_and_commit_metadata_through_command() {
    let harness = fixture().await;
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let observer = lane.clone();
    let commit = lane
        .command(
            move |mut state, reader| {
                Box::pin(async move {
                    let stored = reader
                        .get_value(&lane_config("main"), &BACKGROUND_CONTEXT)
                        .await?
                        .unwrap();
                    assert_eq!(stored.value, serde_json::to_value(seed()).unwrap());
                    state.configuration.thinking_level = ModelThinkingLevel::High;
                    Ok(LaneCommand::Commit {
                        decision: CommitDecision {
                            writes: vec![Write::Value(set_value(
                                &lane_config("main"),
                                serde_json::to_value(&state.configuration).unwrap(),
                            ))],
                            materialize: Arc::new(move |commit| {
                                assert_eq!(
                                    observer.state().configuration.thinking_level,
                                    ModelThinkingLevel::High
                                );
                                commit.clone()
                            }),
                            events: None,
                        },
                        next: Box::new(state),
                    })
                })
            },
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap();
    assert_eq!(commit.seqs.len(), 1);
    assert_eq!(commit.timestamp, 10);
}

#[tokio::test]
async fn sealing_rejects_later_reads_and_commands() {
    let harness = fixture().await;
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    harness.close(&BACKGROUND_CONTEXT).await;
    assert!(lane.get_tip_id().is_err());
    assert!(
        lane.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn attaches_without_implicit_main_lane() {
    let harness = fixture().await;
    assert!(harness.lanes().unwrap().is_empty());
    assert!(
        harness
            .session
            .branch("main", &BACKGROUND_CONTEXT)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn atomically_acquires_one_lane_under_concurrency() {
    let harness = fixture().await;
    let (left, right) = tokio::join!(
        harness.lane("main", None, &BACKGROUND_CONTEXT),
        harness.lane("main", None, &BACKGROUND_CONTEXT)
    );
    assert!(Arc::ptr_eq(&left.unwrap(), &right.unwrap()));
    assert_eq!(harness.lanes().unwrap().len(), 1);
}

#[tokio::test]
async fn validates_create_at_only_for_missing_lane() {
    let harness = fixture().await;
    assert!(
        harness
            .lane("missing", Some("unknown".into()), &BACKGROUND_CONTEXT)
            .await
            .is_err()
    );
    let first = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let second = harness
        .lane("main", Some("unknown".into()), &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert!(harness.lane("", None, &BACKGROUND_CONTEXT).await.is_err());
    assert!(
        harness
            .lane("bad\0name", None, &BACKGROUND_CONTEXT)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn attaches_to_data_only_branch_without_moving_tip() {
    let harness = fixture().await;
    harness
        .session
        .create_branch("data", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let lane = harness
        .lane("data", Some("ignored".into()), &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(lane.get_tip_id().unwrap(), None);
    assert_eq!(lane.state().configuration, seed());
}

#[tokio::test]
async fn restores_complete_lanes_without_main() {
    let harness = fixture().await;
    harness
        .lane("other", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let restored = create_agent_harness(harness.session.clone(), seed(), &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(restored.lanes().unwrap()[0].0, "other");
}

#[tokio::test]
async fn rejects_partial_durable_lane_state() {
    let harness = fixture().await;
    harness
        .session
        .set_value(
            &lane_config("partial"),
            serde_json::to_value(seed()).unwrap(),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap();
    assert!(
        create_agent_harness(harness.session.clone(), seed(), &BACKGROUND_CONTEXT)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn persists_name_and_label_updates_and_deletions() {
    let harness = fixture().await;
    harness
        .set_name(Some("named".into()), &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    harness
        .set_label("entry", Some("label".into()), &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(
        harness
            .get_name(&BACKGROUND_CONTEXT)
            .await
            .unwrap()
            .as_deref(),
        Some("named")
    );
    assert_eq!(
        harness
            .get_label("entry", &BACKGROUND_CONTEXT)
            .await
            .unwrap()
            .as_deref(),
        Some("label")
    );
    harness.set_name(None, &BACKGROUND_CONTEXT).await.unwrap();
    harness
        .set_label("entry", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(harness.get_name(&BACKGROUND_CONTEXT).await.unwrap(), None);
    assert_eq!(
        harness
            .get_label("entry", &BACKGROUND_CONTEXT)
            .await
            .unwrap(),
        None
    );
}
