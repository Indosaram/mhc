use std::sync::Arc;

use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::runtime::harness::{create_agent_harness, Harness};
use maho_agent::harness::runtime::lane::{Lane, NavigationOptions, PromptInput};
use maho_agent::harness::session::types::{LaneConfiguration, LaneModelRef, RunSettings, Session, SessionMetadata, ToolExecutionMode};
use maho_agent::harness::session::{MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions};
use maho_cli::experimental::mini::lane_service::{LaneService, LaneServiceOptions, SessionIdentity};
use maho_cli::experimental::mini::models_service::ModelsService;
use maho_cli::experimental::mini::runtime::ModelRuntimeHandle;
use maho_cli::experimental::mini::shared::protocol::{AuthType, CommandResult, ModelRef, ModelsState};
use maho_core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};

async fn fixture() -> (Arc<Lane>, Arc<ModelRuntimeHandle>) {
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
    let runtime = ModelRuntimeHandle::new(ModelRuntime::create(CreateModelRuntimeOptions::default()).await);
    (lane, runtime)
}

fn build_service(lane: Arc<Lane>, models: Arc<ModelRuntimeHandle>) -> LaneService {
    LaneService::new(LaneServiceOptions {
        lane,
        models,
        context: BACKGROUND_CONTEXT.clone(),
        session: SessionIdentity { id: "mini-worker".into(), cwd: "/cwd".into(), path: "memory:/mini-worker".into() },
        models_state: Arc::new(|| ModelsState { models: vec![], accounts: vec![], refreshing: false }),
        publish: Arc::new(|_, _, _| {}),
    })
}

#[tokio::test]
async fn watch_pairs_a_snapshot_with_a_distinct_subscription() {
    let (lane, models) = fixture().await;
    let service = build_service(lane, models);
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
async fn prompt_reports_the_admission_error_through_the_facade() {
    let (lane, models) = fixture().await;
    let service = build_service(lane, models);
    assert_eq!(service.prompt("").await, CommandResult::Error("Acceptance must append at least one message".to_owned()));
    service.close();
}

#[tokio::test]
async fn unwatch_and_close_release_watch_subscriptions() {
    let (lane, models) = fixture().await;
    let service = build_service(lane, models);
    let watched = service.watch("presentation").await.expect("watch");
    service.start(&watched.subscription_id).await.expect("start");
    service.unwatch(&watched.subscription_id);
    assert_eq!(service.start(&watched.subscription_id).await.unwrap_err(), format!("Unknown subscription: {}", watched.subscription_id));
    let again = service.watch("presentation").await.expect("watch again");
    service.close();
    assert_eq!(service.start(&again.subscription_id).await.unwrap_err(), format!("Unknown subscription: {}", again.subscription_id));
}

#[tokio::test]
async fn set_model_rejects_an_unknown_identity() {
    let (lane, models) = fixture().await;
    let service = build_service(lane, models);
    let result = service.set_model(&ModelRef { provider: "absent".into(), model_id: "model".into() }).await;
    assert_eq!(result, CommandResult::Error("Unknown model: absent/model".to_owned()));
    service.close();
}

#[tokio::test]
async fn navigate_tree_reports_an_unknown_target_through_the_facade() {
    let (lane, models) = fixture().await;
    let service = build_service(lane, models);
    let result = service.navigate_tree(Some("missing".to_owned()), NavigationOptions::default()).await;
    assert_eq!(result, CommandResult::Error("Unknown target: missing".to_owned()));
    service.close();
}

#[tokio::test]
async fn abort_without_an_operation_reports_it() {
    let (lane, models) = fixture().await;
    let service = build_service(lane, models);
    assert_eq!(service.abort().await, CommandResult::Error("Lane \"main\" has no active operation".to_owned()));
    service.close();
}

#[tokio::test]
async fn models_login_rejects_an_unknown_provider() {
    let runtime = ModelRuntimeHandle::new(ModelRuntime::create(CreateModelRuntimeOptions::default()).await);
    let service = ModelsService::new(runtime, Arc::new(|_event| {})).await;
    match service.login("definitely-not-a-provider", AuthType::Oauth).await {
        CommandResult::Error(message) => assert!(message.contains("Unknown provider"), "unexpected error: {message}"),
        CommandResult::Ok => panic!("unknown provider accepted"),
    }
}

#[test]
fn wire_event_folds_through_the_shared_reducer() {
    use maho_agent::harness::events::{HarnessEvent, HarnessEventPayload};
    use maho_agent::harness::runtime::reducer::reduce_lane_snapshot;
    let event = HarnessEvent::new(HarnessEventPayload::RunStart { run_id: "op".into(), started_at: 7 }, Some("main".into()));
    let mut snapshot = serde_json::json!({"lane":"main","tipId":null,"operation":null,"transcript":[],"queues":[],"stats":{"messageCount":0}});
    assert_eq!(reduce_lane_snapshot(&mut snapshot, &serde_json::Value::from(&event)), None);
    assert_eq!(snapshot["operation"]["id"], "op");
    assert_eq!(snapshot["operation"]["kind"], "run");
    assert_eq!(snapshot["operation"]["startedAt"], 7);
}

async fn harness_fixture() -> (Harness, Arc<Lane>) {
    let session = Arc::new(StorageBackedSession::new(
        SessionMetadata { id: "mini-open".into(), created_at: 1, storage_version: 1, cwd: None, parent_session_id: None, legacy_parent_session_path: None },
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
    (harness, lane)
}

#[tokio::test]
async fn open_operations_reports_an_admitted_operation() {
    let (harness, lane) = harness_fixture().await;
    assert!(harness.open_operations().expect("open").is_empty());
    let settings = RunSettings {
        compaction: harness.get_compaction_settings().expect("compaction"),
        steering_mode: harness.get_steering_mode().expect("steering"),
        follow_up_mode: harness.get_follow_up_mode().expect("follow up"),
        tool_execution: ToolExecutionMode::Parallel,
    };
    lane.accept_prompt(PromptInput::Text { text: "hello".into(), images: vec![] }, Some("op".into()), settings, &BACKGROUND_CONTEXT).await.expect("admit").expect("admission");
    let open = harness.open_operations().expect("open");
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].operation_id, "op");
    assert_eq!(open[0].lane, "main");
    assert!(!open[0].aborting);
}

#[tokio::test]
async fn worker_model_runtime_resolves_the_initial_model_from_the_agent_models_json() {
    use maho_cli::experimental::mini::worker::model_runtime_options;
    use maho_core::model_resolver::{find_initial_model, InitialModelOptions};
    // Regression: the worker must load the agent dir's models.json. With
    // `CreateModelRuntimeOptions::default()` (no models path) the runtime loads no provider, the
    // worker resolves no initial model, exits before serving `worker.describe`, and the server
    // reports that to the presentation as "Connection closed".
    let directory = tempfile::tempdir().unwrap();
    let agent = directory.path().join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(
        agent.join("models.json"),
        serde_json::json!({ "providers": { "offline": {
            "api": "openai-completions",
            "baseUrl": "http://127.0.0.1:9/v1",
            "apiKey": "offline-fixture",
            "models": [{ "id": "offline", "reasoning": false, "input": ["text"], "contextWindow": 128000, "maxTokens": 4096 }],
        } } })
        .to_string(),
    )
    .unwrap();
    let runtime = ModelRuntime::create(model_runtime_options(&agent.to_string_lossy())).await;
    let initial = find_initial_model(
        InitialModelOptions { cli_provider: None, cli_model: None, scoped_models: &[], is_continuing: false, default_provider: None, default_model_id: None, model_thinking_levels: None },
        &runtime,
    )
    .await
    .unwrap();
    let model = initial.parsed.model.expect("the agent models.json provider must resolve an initial model");
    assert_eq!(model.provider, "offline");
    assert_eq!(model.id, "offline");
}
