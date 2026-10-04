use maho_server::app_server::{registry::{MethodRegistry, RegistryConnection}, skills::register_skill_methods, thread_registry::ThreadRegistry};
use serde_json::json;
use std::sync::Arc;

fn empty_threads(directory: &std::path::Path) -> Arc<ThreadRegistry> {
    Arc::new(ThreadRegistry::new(directory.display().to_string(), None, None))
}

#[tokio::test]
async fn injected_skill_loader_retries_failure_and_caches_success_until_reload() {
    use maho_server::app_server::skills::register_skill_methods_with_loader;
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let recorded = calls.clone();
    let mut registry = MethodRegistry::default();
    register_skill_methods_with_loader(&mut registry,directory.path().display().to_string(),directory.path().display().to_string(),empty_threads(directory.path()),Arc::new(move |_,_| {
        if recorded.fetch_add(1,Ordering::SeqCst)==0 {Err("faux loader unavailable".into())} else {Ok(maho_core::skills::LoadSkillsResult {skills:Vec::new(),diagnostics:Vec::new()})}
    }));
    let connection = RegistryConnection {initialized:true,..Default::default()};
    let failed = registry.dispatch(connection.clone(),json!({"id":1,"method":"skills/list"})).await;
    assert_eq!(failed["result"]["data"][0]["errors"].as_array().unwrap().len(),1);
    for id in [2,3] {
        let response = registry.dispatch(connection.clone(),json!({"id":id,"method":"skills/list"})).await;
        assert_eq!(response["result"]["data"][0]["errors"],json!([]));
    }
    assert_eq!(calls.load(Ordering::SeqCst),2);
    registry.dispatch(connection,json!({"id":4,"method":"skills/list","params":{"forceReload":true}})).await;
    assert_eq!(calls.load(Ordering::SeqCst),3);
}

#[tokio::test]
async fn skills_list_validates_params_and_reports_missing_directories() {
    let directory = tempfile::tempdir().unwrap();
    let mut registry = MethodRegistry::default();
    register_skill_methods(&mut registry, directory.path().join("agent").display().to_string(), directory.path().display().to_string(), empty_threads(directory.path()));
    let connection = RegistryConnection { initialized: true, ..Default::default() };
    for params in [json!(1), json!({"cwds":null}), json!({"cwds":[1]}), json!({"forceReload":null})] {
        let response = registry.dispatch(connection.clone(), json!({"id":1,"method":"skills/list","params":params})).await;
        assert_eq!(response["error"]["code"], -32602);
    }
    let response = registry.dispatch(connection, json!({"id":2,"method":"skills/list","params":{"cwds":["absent"]}})).await;
    assert_eq!(response["result"]["data"][0]["errors"][0]["message"], "skill cwd does not exist");
}

#[tokio::test]
async fn skills_list_uses_native_loader_cache_and_explicit_reload() {
    let directory = tempfile::tempdir().unwrap();
    let agent = directory.path().join("agent");
    let skill_dir = agent.join("skills/example");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: example\ndescription: Example skill\n---\nBody\n").unwrap();
    let mut registry = MethodRegistry::default();
    register_skill_methods(&mut registry, agent.display().to_string(), directory.path().display().to_string(), empty_threads(directory.path()));
    let connection = RegistryConnection { initialized: true, ..Default::default() };
    let request = json!({"id":1,"method":"skills/list"});
    let initial = registry.dispatch(connection.clone(), request.clone()).await;
    assert_eq!(initial["result"]["data"][0]["skills"][0]["scope"], "user");
    assert_eq!(initial["result"]["data"][0]["skills"][0]["name"], "example");
    std::fs::remove_file(skill_dir.join("SKILL.md")).unwrap();
    let cached = registry.dispatch(connection.clone(), request).await;
    assert_eq!(cached["result"], initial["result"]);
    let reloaded = registry.dispatch(connection, json!({"id":2,"method":"skills/list","params":{"forceReload":true}})).await;
    assert_eq!(reloaded["result"]["data"][0]["skills"], json!([]));
}

#[tokio::test]
async fn loaded_thread_cwd_uses_its_live_session_skills_and_falls_back_only_when_absent() {
    use maho_core::{agent_session::{AgentSession, AgentSessionConfig}, model_runtime::{CreateModelRuntimeOptions, ModelRuntime}, session_manager::SessionManager, settings_manager::{InMemorySettingsStorage, SettingsManager}, source_info::{SourceInfo, SourceScope}};
    let directory = tempfile::tempdir().unwrap();
    let agent = directory.path().join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    let thread_cwd = directory.path().to_string_lossy().into_owned();
    let stream: maho_agent::types::StreamFn = Arc::new(|_, _, _| maho_ai::types::AssistantMessageEventStream::assistant());
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions { stream_fn: Some(stream), ..Default::default() }),
        session_manager: SessionManager::in_memory(&thread_cwd, None, None),
        settings_manager: SettingsManager::from_storage(Box::<InMemorySettingsStorage>::default(), false),
        cwd: thread_cwd.clone(), agent_dir: Some(agent.display().to_string()), fallback_now: None, retry_random: None,
        scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(),
        model_runtime: Some(ModelRuntime::create_sync(CreateModelRuntimeOptions { providers: Some(Vec::new()), ..Default::default() })),
        model_registry: None, uses_default_stream_function: Some(false), initial_active_tool_names: None, default_tool_names: None,
        eval_only_tool_names: None, allowed_tool_names: None, excluded_tool_names: None, base_tools_override: None,
        session_start_event: None, auto_title_sessions: Some(false),
    }).unwrap();
    let skill = maho_core::skills::Skill { name: "session-only".into(), description: "Per-session snapshot".into(), file_path: "/custom/session-only/SKILL.md".into(), base_dir: "/custom".into(), disable_model_invocation: false, source_info: SourceInfo { scope: SourceScope::User, ..Default::default() } };
    session.set_prompt_resources_with_diagnostics(Vec::new(), vec![skill], Vec::new());
    let threads = Arc::new(ThreadRegistry::new(agent.display().to_string(), None, None));
    threads.register_session(session, thread_cwd.clone(), None).await;
    let mut registry = MethodRegistry::default();
    register_skill_methods(&mut registry, agent.display().to_string(), directory.path().display().to_string(), threads.clone());
    let connection = RegistryConnection { initialized: true, ..Default::default() };
    let response = registry.dispatch(connection.clone(), json!({"id":1,"method":"skills/list","params":{"cwds":[thread_cwd]}})).await;
    assert_eq!(response["result"]["data"][0]["skills"][0]["name"], "session-only", "the loaded thread's live per-session snapshot must win over a disk load");
    assert_eq!(response["result"]["data"][0]["skills"][0]["description"], "Per-session snapshot");
    let unloaded = directory.path().join("unloaded");
    std::fs::create_dir_all(&unloaded).unwrap();
    let fallback = registry.dispatch(connection, json!({"id":2,"method":"skills/list","params":{"cwds":[unloaded.display().to_string()]}})).await;
    assert_eq!(fallback["result"]["data"][0]["skills"], json!([]), "a cwd with no loaded thread falls back to the fresh disk loader");
}
