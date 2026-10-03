use std::collections::BTreeMap;

use maho_cli::cli::task_runners::{authenticated_rpc_options, native_rpc_options};
use senpi_task::runners::types::RpcRunnerSpec;

#[test]
fn native_rpc_spawn_preserves_isolation_and_explicit_member_profile() {
    let dir = tempfile::tempdir().expect("isolated task state");
    let executable = dir.path().join("mhc");
    let agent_dir = dir.path().join("agent").to_string_lossy().into_owned();
    let options = native_rpc_options(executable.clone(), &agent_dir, BTreeMap::from([
        ("SENPI_TASK_MEMBER".into(), "stale-parent".into()),
        ("MAHO_CODING_AGENT_SESSION_DIR".into(), "parent-transcript".into()),
    ]), Vec::new());
    let spec = RpcRunnerSpec {
        task_id: "isolated-child".into(), cwd: dir.path().to_string_lossy().into_owned(),
        state_dir: dir.path().join("state").to_string_lossy().into_owned(),
        model: Some("provider/model".into()),
        extensions: Some(vec!["native-team".into()]),
        member_env: Some(BTreeMap::from([("SENPI_TASK_MEMBER".into(), "child-member".into())])),
        ..Default::default()
    };

    let descriptor = options.build_spawn.expect("native spawn builder")(&spec);

    assert_eq!(descriptor.command, executable.to_string_lossy());
    assert_eq!(descriptor.cwd, spec.cwd);
    assert_eq!(descriptor.env["MAHO_CODING_AGENT_DIR"], agent_dir);
    assert_eq!(descriptor.env["SENPI_TASK_MEMBER"], "child-member");
    assert_eq!(descriptor.env["MAHO_CODING_AGENT_SESSION_DIR"], descriptor.env["SENPI_CODING_AGENT_SESSION_DIR"]);
    assert!(std::path::Path::new(&descriptor.env["MAHO_CODING_AGENT_SESSION_DIR"])
        .starts_with(dir.path().join("state/sessions/isolated-child")));
    assert_eq!(descriptor.args, ["--mode", "rpc", "--no-extensions", "--extension", "native-team", "--model", "provider/model"]);
}

#[test]
fn native_rpc_spawn_removes_inherited_member_identity() {
    let dir = tempfile::tempdir().expect("isolated task state");
    let options = native_rpc_options(dir.path().join("mhc"), "native-agent", BTreeMap::from([
        ("SENPI_TASK_MEMBER".into(), "parent-member".into()),
        ("SENPI_TASK_MEMBER_TASK_ID".into(), "parent-task".into()),
        ("SENPI_TASK_TEAM_CONFIG".into(), "parent-config".into()),
    ]), Vec::new());
    let spec = RpcRunnerSpec {
        task_id: "child".into(), cwd: dir.path().to_string_lossy().into_owned(),
        state_dir: dir.path().to_string_lossy().into_owned(), ..Default::default()
    };

    let descriptor = options.build_spawn.expect("native spawn builder")(&spec);

    for name in ["SENPI_TASK_MEMBER", "SENPI_TASK_MEMBER_TASK_ID", "SENPI_TASK_TEAM_CONFIG"] {
        assert!(!descriptor.env.contains_key(name));
    }
}

#[test]
fn retired_parent_rejects_admission_before_catalog_probe() {
    let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = called.clone();
    let options = senpi_task::runners::rpc_process::RpcProcessRunnerOptions {
        model_admission: Some(std::sync::Arc::new(move |_| {
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })),
        ..Default::default()
    };
    let options = authenticated_rpc_options(options, std::sync::Weak::new());
    let spec = RpcRunnerSpec { model: Some("provider/model".into()), ..Default::default() };

    let result = options.model_admission.expect("authenticated admission")(&spec);

    assert_eq!(result.expect_err("retired parent").kind,
        senpi_task::runners::RunnerFailureKind::ModelUnavailable);
    assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn child_registry_retains_native_credentials_and_retires_with_parent() {
    let dir = tempfile::tempdir().expect("isolated native parent");
    let cwd = dir.path().to_string_lossy().into_owned();
    let credentials = std::sync::Arc::new(maho_core::auth_storage::AuthStorage::in_memory(Default::default()));
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions {
            credentials: Some(credentials.clone()), providers: Some(Vec::new()), ..Default::default()
        });
    runtime.register_provider("task-fixture", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider {
            api: Some("openai-completions".into()), api_key: Some("fixture-key".into()),
            base_url: Some("http://127.0.0.1:1/v1".into()),
            models: Some(vec![maho_core::model_config_schema::ModelsJsonModel { id: "selected".into(), ..Default::default() }]),
            ..Default::default()
        }, ..Default::default()
    }).expect("configured native provider");
    let model = runtime.get_model("task-fixture", "selected").expect("fixture model");
    let created = tokio::time::timeout(std::time::Duration::from_secs(5),
        maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(model.clone()), model_runtime: Some(runtime), tools: Some(Vec::new()),
            session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
            ..Default::default()
        })).await.expect("bounded native construction").expect("native SDK parent");
    let session = std::sync::Arc::new(created.session);

    let resolve = maho_cli::cli::task_runners::live_parent_registry(std::sync::Arc::downgrade(&session));
    let registry = resolve().expect("live parent registry");
    let child_credentials = registry.auth_storage().downcast::<maho_core::auth_storage::AuthStorage>().expect("native credential type");
    let child_model = registry.find("task-fixture", "selected").expect("resolved model")
        .downcast::<maho_ai::types::Model>().expect("native model type");
    session.dispose().await;
    drop(session);

    assert!(std::sync::Arc::ptr_eq(&credentials, &child_credentials));
    assert_eq!(*child_model, model);
    assert!(resolve().is_none());
}

#[tokio::test]
async fn shared_parent_tool_obeys_registered_admission_hooks() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    let dir = tempfile::tempdir().expect("isolated tool parent");
    let cwd = dir.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions { providers: Some(Vec::new()), ..Default::default() });
    runtime.register_native_provider(provider.provider.clone());
    let executions = Arc::new(AtomicUsize::new(0));
    let executed = executions.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "task-admission".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            let executed = executed.clone();
            api.register_tool(maho_ext_api::ToolDefinition::new("guarded", "guarded fixture",
                serde_json::json!({"type":"object","properties":{}}), Arc::new(move |_| {
                    executed.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(maho_ext_api::ToolResult::text("executed")) })
                })));
            api.on(maho_ext_api::EventKind::ToolCall, Arc::new(|_, _| Box::pin(async {
                Ok(maho_ext_api::EventResult::ToolCall(maho_ext_api::ToolCallEventResult {
                    block: Some(true), reason: Some("fixture admission denied".into()), ..Default::default()
                }))
            })));
            Box::pin(async { Ok(()) })
        }),
    };
    let created = tokio::time::timeout(std::time::Duration::from_secs(5),
        maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(provider.get_model(Some("faux-1")).expect("model")), model_runtime: Some(runtime),
            session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
            tools: Some(vec!["guarded".into()]), extension_factories: vec![factory], ..Default::default()
        })).await.expect("bounded construction").expect("native parent");
    let session = Arc::new(created.session);
    let resolve = maho_cli::cli::task_runners::live_parent_tools(Arc::downgrade(&session), tokio::runtime::Handle::current());
    let tool = resolve().into_iter().find(|tool| tool.name() == "guarded").expect("live tool");
    let (sender, receiver) = tokio::sync::oneshot::channel();

    let worker = std::thread::spawn(move || {
        let result = tool.execute("child-call", &serde_json::json!({}));
        let _ = sender.send(result);
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), receiver).await;
    session.dispose().await;

    tokio::time::timeout(std::time::Duration::from_secs(5),
        tokio::task::spawn_blocking(move || worker.join()))
        .await.expect("bounded worker cleanup").expect("join task").expect("tool worker");
    assert!(result.expect("bounded tool admission").expect("worker result").is_err());
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}
