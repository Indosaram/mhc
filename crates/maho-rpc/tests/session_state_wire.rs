//! Wire coverage for the `RpcSessionState` projection (`buildRpcSessionState`), consumed by
//! lane 35's `RemoteSessionState::from_rpc`. `ordered` stays routed to the core owner: the
//! session exposes no public queued-input-order accessor.

use maho_core::{
    model_runtime::{CreateModelRuntimeOptions, ModelRuntime},
    sdk::{CreateAgentSessionOptions, NoToolsMode, create_agent_session},
    session_manager::SessionManager,
    settings_manager::{InMemorySettingsStorage, SettingsManager},
};
use maho_rpc::{
    connection_handler::{build_rpc_session_state, handle_input_line, handle_session_command},
    rpc_types::{RpcCommand, RpcResponseResult},
    session_attribution::SessionActivityRegistry,
    session_registry::{OpenAdmission, RpcSessionLaunchProfile, RpcSessionRegistry},
};
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

#[tokio::test]
async fn native_session_dispatch_get_state_correlates_and_reports_real_state() {
    let temp = tempfile::tempdir().unwrap();
    let session = session_in(&temp).await;
    let cwd = session.cwd();

    let wire_res = handle_input_line(&session, r#"{"id":"state-1","sessionId":"sess-1","type":"get_state"}"#)
        .await
        .unwrap()
        .unwrap();
    assert!(wire_res.ends_with('\n'));
    let wire_json: serde_json::Value = serde_json::from_str(&wire_res).unwrap();
    assert_eq!(wire_json["type"], "response");
    assert_eq!(wire_json["id"], "state-1");
    assert_eq!(wire_json["sessionId"], "sess-1");
    assert_eq!(wire_json["command"], "get_state");
    assert_eq!(wire_json["success"], true);
    assert_eq!(wire_json["data"]["isStreaming"], json!(false));
    assert_eq!(wire_json["data"]["isCompacting"], json!(false));
    assert_eq!(wire_json["data"]["isBashRunning"], json!(false));
    assert_eq!(wire_json["data"]["retryAttempt"], json!(0));
    assert_eq!(wire_json["data"]["cwd"], json!(cwd));
    assert_eq!(wire_json["data"]["projectTrusted"], json!(false));

    let command: RpcCommand = serde_json::from_value(json!({"id":"state-2","type":"get_state"})).unwrap();
    let response = handle_session_command(&session, &command).await.unwrap();
    let res_val = serde_json::to_value(&response).unwrap();
    assert_eq!(res_val["id"], "state-2");
    assert_eq!(res_val["command"], "get_state");
    assert_eq!(res_val["success"], true);
    assert_eq!(res_val["data"]["isStreaming"], json!(false));
    assert_eq!(res_val["data"]["isCompacting"], json!(false));

    session.dispose().await;
}

#[tokio::test]
async fn registered_session_dispatch_get_state_correlates_and_preserves_runtime_state() {
    let temp = tempfile::tempdir().unwrap();
    let session = session_in(&temp).await;
    let cwd = session.cwd();
    let models = session.model_registry().clone();
    let services = maho_core::agent_session_services::AgentSessionServices {
        cwd: cwd.clone(),
        agent_dir: session.agent_dir(),
        auth_storage: std::sync::Arc::clone(&models.auth_storage),
        model_registry: models,
        settings_manager: SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), false),
        diagnostics: vec![],
    };
    let runtime = std::sync::Arc::new(maho_core::agent_session_runtime::AgentSessionRuntime::new(
        session.clone(),
        services,
        vec![],
        None,
        None,
    ));
    let mut registry = RpcSessionRegistry::default();
    let OpenAdmission::Create(handle) = registry.admit_open(
        RpcSessionLaunchProfile {
            runtime: maho_core::agent_session_runtime::AgentSessionLaunchProfile { cwd: cwd.clone(), ..Default::default() },
            session_path: None,
            durable_session_id: None,
            session_kind: None,
            session_context: None,
        },
        false,
        0.0,
    ).unwrap() else { panic!("expected create admission") };
    registry.install_runtime(&handle, runtime).unwrap();

    let command: RpcCommand = serde_json::from_value(json!({"id":"reg-state-1","type":"get_state"})).unwrap();
    let activity = SessionActivityRegistry::default();
    let response = registry.dispatch_command(&handle, &command, "get_state", 10.0, &activity).await.unwrap().unwrap();
    assert_eq!(response.id.as_deref(), Some("reg-state-1"));
    assert_eq!(response.session_id.as_deref(), Some(handle.as_str()));
    assert_eq!(response.command, "get_state");
    let RpcResponseResult::Success { data: Some(state) } = response.result else { panic!("expected success with state data") };
    assert_eq!(state["isStreaming"], json!(false));
    assert_eq!(state["isCompacting"], json!(false));
    assert_eq!(state["isBashRunning"], json!(false));
    assert_eq!(state["retryAttempt"], json!(0));
    assert_eq!(state["cwd"], json!(cwd));
    assert_eq!(state["projectTrusted"], json!(false));

    session.dispose().await;
}
