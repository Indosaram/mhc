//! Wire coverage for the `RpcSessionState` projection (`buildRpcSessionState`), consumed by
//! lane 35's `RemoteSessionState::from_rpc`. `ordered` stays routed to the core owner: the
//! session exposes no public queued-input-order accessor.

use maho_core::{
    model_runtime::{CreateModelRuntimeOptions, ModelRuntime},
    sdk::{CreateAgentSessionOptions, NoToolsMode, create_agent_session},
    session_manager::SessionManager,
    settings_manager::{InMemorySettingsStorage, SettingsManager},
};
use maho_rpc::connection_handler::build_rpc_session_state;
use serde_json::json;

async fn session_in(temp: &tempfile::TempDir) -> maho_core::agent_session::AgentSession {
    let cwd = temp.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let model = provider.get_model(Some("faux-1")).expect("the faux provider registers faux-1");
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        models_path: Some(temp.path().join("models.json")),
        auth_path: Some(temp.path().join("auth.json")),
        providers: Some(vec![provider.provider]),
        ..Default::default()
    });
    create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(cwd.clone()),
        model_runtime: Some(runtime),
        model: Some(model),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        settings_manager: Some(SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), false)),
        no_tools: Some(NoToolsMode::All),
        auto_title_sessions: Some(false),
        ..Default::default()
    })
    .await
    .expect("the session is created")
    .session
}

#[tokio::test]
async fn state_reads_the_project_trust_store_not_a_cached_setting() {
    let temp = tempfile::tempdir().unwrap();
    let session = session_in(&temp).await;
    assert_eq!(build_rpc_session_state(&session, None)["projectTrusted"], json!(false));
    maho_core::ProjectTrustStore::new(&session.agent_dir())
        .set(&session.cwd(), Some(true))
        .unwrap();
    assert_eq!(build_rpc_session_state(&session, None)["projectTrusted"], json!(true));
    maho_core::ProjectTrustStore::new(&session.agent_dir())
        .set(&session.cwd(), Some(false))
        .unwrap();
    assert_eq!(build_rpc_session_state(&session, None)["projectTrusted"], json!(false));
    session.dispose().await;
}

#[tokio::test]
async fn state_publishes_the_session_manager_usage_totals() {
    let temp = tempfile::tempdir().unwrap();
    let session = session_in(&temp).await;
    assert_eq!(
        build_rpc_session_state(&session, None)["usageTotals"],
        json!({"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"cost":0.0})
    );
    session.with_session_manager_mut(|manager| {
        manager.append_message(json!({
            "role":"assistant",
            "content":"hi",
            "usage":{"input":10,"output":5,"cacheRead":2,"cacheWrite":1,"cost":{"total":0.25}}
        }));
    });
    assert_eq!(
        build_rpc_session_state(&session, None)["usageTotals"],
        json!({"input":10,"output":5,"cacheRead":2,"cacheWrite":1,"cost":0.25})
    );
    session.dispose().await;
}

#[tokio::test]
async fn state_publishes_the_queued_input_order() {
    let temp = tempfile::tempdir().unwrap();
    let session = session_in(&temp).await;
    assert_eq!(build_rpc_session_state(&session, None)["ordered"], json!([]));
    session.steer("queued", None, Default::default()).await.unwrap();
    let ordered = &build_rpc_session_state(&session, None)["ordered"];
    assert_eq!(ordered[0]["text"], "queued");
    assert_eq!(ordered[0]["mode"], "steer");
    assert!(ordered[0]["enqueueOrder"].is_number());
    session.dispose().await;
}
