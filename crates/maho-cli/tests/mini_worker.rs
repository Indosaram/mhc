use std::sync::Arc;

use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::runtime::harness::create_agent_harness;
use maho_agent::harness::runtime::lane::Lane;
use maho_agent::harness::session::types::{LaneConfiguration, LaneModelRef, RunSettings, Session, SessionMetadata, ToolExecutionMode};
use maho_agent::harness::session::{MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions};
use maho_cli::experimental::mini::lane_service::{LaneService, LaneServiceOptions, SessionIdentity};
use maho_cli::experimental::mini::runtime::ModelRuntimeHandle;
use maho_cli::experimental::mini::shared::protocol::{CommandResult, ModelRef, ModelsState};
use maho_core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};

async fn fixture() -> (Arc<Lane>, Arc<ModelRuntimeHandle>, RunSettings) {
    let session = Arc::new(StorageBackedSession::new(
        SessionMetadata { id: "mini-worker".into(), created_at: 1, storage_version: 1, cwd: None, parent_session_id: None, legacy_parent_session_path: None },
        Arc::new(MemoryStorage::new(MemoryStorageOptions { now: Some(Arc::new(|| 42)) })),
        StorageBackedSessionOptions::default(),
    ));
    session.attach();
    let session: Arc<dyn Session> = session;
    let harness = create_agent_harness(
        session,
        LaneConfiguration { model: LaneModelRef { provider: "test".into(), model_id: "model".into() }, thinking_level: maho_ai::types::ModelThinkingLevel::Off, active_tool_names: vec![] },
        &BACKGROUND_CONTEXT,
    )
    .await
    .expect("harness");
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await.expect("lane");
    let settings = RunSettings {
        compaction: harness.get_compaction_settings().expect("compaction"),
        steering_mode: harness.get_steering_mode().expect("steering"),
        follow_up_mode: harness.get_follow_up_mode().expect("follow up"),
        tool_execution: ToolExecutionMode::Parallel,
    };
    let runtime = ModelRuntimeHandle::new(ModelRuntime::create(CreateModelRuntimeOptions::default()).await);
    (lane, runtime, settings)
}

fn build_service(lane: Arc<Lane>, models: Arc<ModelRuntimeHandle>, settings: RunSettings) -> LaneService {
    LaneService::new(LaneServiceOptions {
        lane,
        models,
        context: BACKGROUND_CONTEXT.clone(),
        session: SessionIdentity { id: "mini-worker".into(), cwd: "/cwd".into(), path: "memory:/mini-worker".into() },
        settings,
        models_state: Arc::new(|| ModelsState { models: vec![], accounts: vec![], refreshing: false }),
        publish: Arc::new(|_, _, _| {}),
    })
}

#[tokio::test]
async fn watch_pairs_a_snapshot_with_a_distinct_subscription() {
    let (lane, models, settings) = fixture().await;
    let service = build_service(lane, models, settings);
    let first = service.watch("presentation-a").await.expect("first watch");
    let second = service.watch("presentation-b").await.expect("second watch");
    assert_ne!(first.subscription_id, second.subscription_id);
    assert_eq!(first.snapshot.session_id, "mini-worker");
    assert_eq!(first.snapshot.cwd, "/cwd");
    assert_eq!(first.snapshot.session_path, "memory:/mini-worker");
    assert_eq!(first.snapshot.lane["lane"], "main");
    assert!(first.snapshot.lane["transcript"].is_array());
    assert!(first.snapshot.models.models.is_empty());
    service.start(&first.subscription_id).await.expect("start");
    assert_eq!(service.start("missing").await.unwrap_err(), "Unknown subscription: missing");
    service.unwatch(&first.subscription_id);
    service.unwatch(&second.subscription_id);
    service.close();
}

#[tokio::test]
async fn prompt_admits_an_operation_a_fresh_snapshot_reflects() {
    let (lane, models, settings) = fixture().await;
    let service = build_service(lane, models, settings);
    assert_eq!(service.prompt("hello").await, CommandResult::Ok);
    let snapshot = service.watch("presentation").await.expect("watch").snapshot;
    assert!(!snapshot.lane["operation"].is_null());
    service.close();
}

#[tokio::test]
async fn set_model_rejects_an_unknown_identity() {
    let (lane, models, settings) = fixture().await;
    let service = build_service(lane, models, settings);
    let result = service.set_model(&ModelRef { provider: "absent".into(), model_id: "model".into() }).await;
    assert_eq!(result, CommandResult::Error("Unknown model: absent/model".to_owned()));
    service.close();
}

#[tokio::test]
async fn abort_without_an_operation_reports_it() {
    let (lane, models, settings) = fixture().await;
    let service = build_service(lane, models, settings);
    assert_eq!(service.abort().await, CommandResult::Error("No active operation to abort".to_owned()));
    service.close();
}

#[test]
fn wire_event_folds_through_the_shared_reducer() {
    use maho_agent::harness::events::{HarnessEvent, HarnessEventPayload};
    use maho_agent::harness::runtime::reducer::reduce_lane_snapshot;
    use maho_cli::experimental::mini::shared::wire::harness_event_value;
    let event = HarnessEvent::new(HarnessEventPayload::RunStart { run_id: "op".into(), started_at: 7 }, Some("main".into()));
    let mut snapshot = serde_json::json!({"lane":"main","tipId":null,"operation":null,"transcript":[],"queues":[],"stats":{"messageCount":0}});
    assert_eq!(reduce_lane_snapshot(&mut snapshot, &harness_event_value(&event)), None);
    assert_eq!(snapshot["operation"]["id"], "op");
    assert_eq!(snapshot["operation"]["kind"], "run");
    assert_eq!(snapshot["operation"]["startedAt"], 7);
}
