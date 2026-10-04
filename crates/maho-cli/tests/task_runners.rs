use std::collections::BTreeMap;

use maho_cli::cli::task_runners::{authenticated_rpc_options, native_rpc_options};
use senpi_task::runners::types::RpcRunnerSpec;

#[test]
fn team_bounds_use_configured_values_and_pinned_defaults() {
    use maho_cli::cli::task_session::configured_team_bounds;
    let defaults = configured_team_bounds(&serde_json::json!({})).unwrap();
    assert_eq!(defaults.max_wall_clock_minutes, 120);
    let configured = configured_team_bounds(&serde_json::json!({"task":{"team":{"max_members":3,"max_parallel_members":2,"max_wall_clock_minutes":17}}})).unwrap();
    assert_eq!((configured.max_members, configured.max_parallel_members, configured.max_wall_clock_minutes), (3, 2, 17));
    assert!(configured_team_bounds(&serde_json::json!({"task":{"team":{"max_members":9}}})).is_err());
    assert!(configured_team_bounds(&serde_json::json!({"task":{"team":{"max_wall_clock_minutes":0}}})).is_err());
}

#[test]
fn production_team_adapter_forwards_full_member_start_spec_to_manager() {
    use std::sync::{Arc, Mutex};
    use senpi_task::{manager::{TaskManager, types::{ManagedRunner, ManagedStartSpec, ManagedRunnerResult, ManagedRunners, TaskManagerOptions, PlanResolutionCode, PlanResolutionError}},
        team::runtime_types::{TeamMemberStartSpec, TeamRuntimeManagerPort, TeamStartResult}};
    struct UnexpectedRunner;
    impl ManagedRunner for UnexpectedRunner {
        fn start(&self, _: &ManagedStartSpec) -> ManagedRunnerResult { panic!("planner rejects before execution") }
    }
    let root = tempfile::tempdir().unwrap();
    let store = senpi_task::store::TaskRecordStore::new(&senpi_task::store::StateDirConfig { project_dir: root.path().into(), task_state_dir: None });
    let observed = Arc::new(Mutex::new(None)); let capture = observed.clone();
    let runner = Arc::new(UnexpectedRunner);
    let mut options = TaskManagerOptions::new(store, ManagedRunners { in_process: runner.clone(), process: runner }, Arc::new(move |spec| {
        *capture.lock().unwrap() = Some(spec.clone());
        Err(Box::new(PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, "fixture rejects model")))
    }), root.path().to_string_lossy());
    options.config.max_depth = 8;
    let adapter = maho_cli::cli::task_session::TeamManager(Arc::new(TaskManager::new(options)));
    let spec = TeamMemberStartSpec {
        name: Some("member".into()), description: Some("work".into()), prompt: "task".into(),
        parent_session_id: "parent".into(), root_session_id: Some("root".into()), depth: 3,
        execution_mode: Some(senpi_task::manager::execution_mode::ExecutionMode::Process),
        model: Some("provider/model".into()), category: Some("quick".into()), task_summary: Some("summary".into()),
        cwd: Some(root.path().to_string_lossy().into_owned()), extensions: Some(vec!["builtin:task".into()]),
        member_env: Some(BTreeMap::from([("SENPI_TASK_MEMBER".into(), "member".into())])), run_in_background: true,
        ..Default::default()
    };
    assert!(matches!(adapter.start(&spec).unwrap(), TeamStartResult::Rejected { kind, .. } if kind == "plan_unresolved"));
    let observed = observed.lock().unwrap(); let actual = observed.as_ref().unwrap();
    assert_eq!(actual.name, spec.name); assert_eq!(actual.description, spec.description); assert_eq!(actual.prompt, spec.prompt);
    assert_eq!(actual.parent_session_id, spec.parent_session_id); assert_eq!(actual.root_session_id, spec.root_session_id); assert_eq!(actual.depth, spec.depth);
    assert_eq!(actual.execution_mode, spec.execution_mode); assert_eq!(actual.model, spec.model); assert_eq!(actual.category, spec.category); assert_eq!(actual.subagent_type, spec.subagent_type);
    assert_eq!(actual.task_summary, spec.task_summary); assert_eq!(actual.cwd, spec.cwd); assert_eq!(actual.extensions, spec.extensions);
    assert_eq!(actual.member_env, spec.member_env); assert_eq!(actual.run_in_background, spec.run_in_background);
}

#[test]
fn native_child_transcript_preserves_exact_locator_on_create_and_resume() {
    use senpi_task::runners::in_process::{child_options::build_child_session_options,
        runner::ChildSpec, session_manager::ChildSessionManager};
    let dir = tempfile::tempdir().expect("isolated child transcript");
    let cwd = dir.path().to_string_lossy().into_owned();
    let session_dir = dir.path().join("children/child").to_string_lossy().into_owned();
    let spec = ChildSpec { cwd: cwd.clone(), session_dir: Some(session_dir.clone()), ..Default::default() };
    let locator = ChildSessionManager::create(&cwd, &session_dir).expect("child locator");
    let path = locator.session_file().to_path_buf();
    let options = build_child_session_options(&spec, locator, &[], &[]);

    let mut manager = maho_cli::cli::task_runners::native_child_session_manager(&options);
    manager.append_message(serde_json::json!({"role":"user","content":"child task","timestamp":1}));
    manager.append_message(serde_json::json!({"role":"assistant","content":[],"stopReason":"stop","timestamp":2}));
    let session_id = manager.session_id().to_owned();
    let before = std::fs::read(&path).expect("native transcript persisted at child locator");
    let resumed_options = build_child_session_options(&spec,
        ChildSessionManager::open(&path, &session_dir, &cwd), &[], &[]);

    let resumed = maho_cli::cli::task_runners::native_child_session_manager(&resumed_options);

    assert_eq!(manager.session_file(), Some(path.to_string_lossy().as_ref()));
    assert_eq!(resumed.session_file(), manager.session_file());
    assert_eq!(resumed.session_id(), session_id);
    assert_eq!(resumed.build_context(resumed.leaf_id()).messages.len(), 2);
    assert_eq!(std::fs::read(&path).expect("retained transcript"), before);
    assert_eq!(std::fs::read_dir(&session_dir).expect("child directory").count(), 1);
}

#[test]
fn native_child_settings_forward_retry_policy_without_disk_settings() {
    use senpi_task::runners::in_process::runtime_fallback_settings::RetryFallbackSettings;
    let retry = RetryFallbackSettings {
        model_fallback: true,
        chains: BTreeMap::from([("provider/selected".into(), vec!["provider/fallback:low".into()])]),
    };

    let settings = maho_cli::cli::task_runners::native_child_settings(&retry);
    let resolved = maho_core::retry_fallback::settings::resolve_retry_fallback_settings(settings.get_value("retry"));
    let disabled = maho_cli::cli::task_runners::native_child_settings(&RetryFallbackSettings::default());

    assert!(resolved.model_fallback);
    assert_eq!(resolved.chains["provider/selected"], ["provider/fallback:low"]);
    assert_eq!(settings.get().len(), 1);
    assert!(settings.settings_path(maho_core::settings_manager::SettingsScope::Global).is_none());
    assert!(settings.settings_path(maho_core::settings_manager::SettingsScope::Project).is_none());
    assert!(!maho_core::retry_fallback::settings::resolve_retry_fallback_settings(disabled.get_value("retry")).model_fallback);
}

#[test]
fn native_child_options_preserve_parent_handles_and_isolated_policy() {
    use std::sync::Arc;
    use senpi_task::runners::in_process::{child_options::{build_child_session_options, HostHandle},
        runner::ChildSpec, session_manager::ChildSessionManager};
    let dir = tempfile::tempdir().expect("child options");
    let cwd = dir.path().to_string_lossy().into_owned();
    let credentials = Arc::new(maho_core::auth_storage::AuthStorage::in_memory(Default::default()));
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions {
            credentials: Some(credentials.clone()), providers: Some(Vec::new()), ..Default::default()
        });
    let model = maho_ai::providers::faux::faux_provider(Default::default()).get_model(Some("faux-1")).expect("model");
    let spec = ChildSpec {
        cwd: cwd.clone(), agent_dir: Some("child-agent".into()),
        auth_storage: Some(credentials.clone()), model_runtime: Some(Arc::new(runtime.clone())),
        model_registry: Some(Arc::new(maho_cli::cli::task_runners::NativeChildModelRegistry(
            maho_core::model_registry::ModelRegistry::new(runtime)))), model: Some(Arc::new(model.clone())),
        thinking_level: Some("off".into()), tool_allowlist: Some(vec!["read".into()]),
        tool_denylist: Some(vec!["bash".into()]), ..Default::default()
    };
    let mut child = build_child_session_options(&spec, ChildSessionManager::create(&cwd, &cwd).expect("locator"), &[], &[]);

    let options = maho_cli::cli::task_runners::native_child_sdk_options(&child, Vec::new()).expect("native options");

    assert!(options.minimal_resources);
    assert!(Arc::ptr_eq(options.auth_storage.as_ref().expect("credentials"), &credentials));
    assert!(Arc::ptr_eq(&options.model_registry.as_ref().expect("registry").auth_storage, &credentials));
    assert_eq!(options.model, Some(model));
    assert_eq!(options.thinking_selection.expect("off selection").level, maho_ai::types::ModelThinkingLevel::Off);
    assert_eq!(options.tools, spec.tool_allowlist);
    assert_eq!(options.exclude_tools, spec.tool_denylist);
    assert_eq!(options.agent_dir, spec.agent_dir);
    child.model = Some(Arc::new("foreign-model".to_owned()) as HostHandle);
    assert!(maho_cli::cli::task_runners::native_child_sdk_options(&child, Vec::new()).is_err());
}

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

#[tokio::test]
async fn converted_child_options_drive_native_provider_and_restore_exact_transcript() {
    use std::sync::Arc;
    use senpi_task::runners::in_process::{child_options::build_child_session_options,
        runner::ChildSpec, session_manager::ChildSessionManager};
    let dir = tempfile::tempdir().expect("isolated native child");
    let cwd = dir.path().to_string_lossy().into_owned();
    let session_dir = dir.path().join("children/child").to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    provider.set_responses(vec![maho_ai::providers::faux::faux_assistant_message("native child response", Default::default()).into()]);
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions { providers: Some(Vec::new()), ..Default::default() });
    runtime.register_native_provider(provider.provider.clone());
    let registry = maho_core::model_registry::ModelRegistry::new(runtime.clone());
    registry.auth_storage.set("faux", Some(serde_json::json!({"type":"api_key","key":"native-child-fixture"}))).expect("faux auth");
    let spec = ChildSpec {
        cwd: cwd.clone(), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        auth_storage: Some(registry.auth_storage.clone()),
        model_registry: Some(Arc::new(maho_cli::cli::task_runners::NativeChildModelRegistry(registry))),
        model_runtime: Some(Arc::new(runtime)), model: Some(Arc::new(provider.get_model(Some("faux-1")).expect("model"))),
        tool_allowlist: Some(Vec::new()), ..Default::default()
    };
    let locator = ChildSessionManager::create(&cwd, &session_dir).expect("locator");
    let path = locator.session_file().to_path_buf();
    let child = build_child_session_options(&spec, locator, &[], &[]);
    let options = maho_cli::cli::task_runners::native_child_sdk_options(&child, Vec::new()).expect("child SDK options");

    let created = tokio::time::timeout(std::time::Duration::from_secs(5), maho_core::sdk::create_agent_session(options))
        .await.expect("bounded native construction").expect("child session");
    let prompt = tokio::time::timeout(std::time::Duration::from_secs(5), created.session.prompt("native child task", Default::default())).await;
    let text = created.session.get_last_assistant_text();
    created.session.dispose().await;
    prompt.expect("bounded native provider turn").expect("child turn");
    let restored_child = build_child_session_options(&spec, ChildSessionManager::open(&path, &session_dir, &cwd), &[], &[]);
    let restored = maho_cli::cli::task_runners::native_child_session_manager(&restored_child);

    assert_eq!(text.as_deref(), Some("native child response"));
    assert_eq!(provider.get_call_log().len(), 1);
    assert_eq!(restored.session_file(), Some(path.to_string_lossy().as_ref()));
    assert_eq!(restored.build_context(restored.leaf_id()).messages.len(), 2);
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
    let options = authenticated_rpc_options(options, std::sync::Arc::new(|| None));
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

    let resolve = maho_cli::cli::task_runners::live_parent_registry(session.weak_accessor());
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
    let admitted_id = Arc::new(std::sync::Mutex::new(None));
    let observed_id = admitted_id.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "task-admission".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            let executed = executed.clone();
            let observed_id = observed_id.clone();
            api.register_tool(maho_ext_api::ToolDefinition::new("guarded", "guarded fixture",
                serde_json::json!({"type":"object","properties":{}}), Arc::new(move |_| {
                    executed.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(maho_ext_api::ToolResult::text("executed")) })
                })));
            api.on(maho_ext_api::EventKind::ToolCall, Arc::new(move |event, _| {
                let observed_id = observed_id.clone();
                Box::pin(async move {
                if let maho_ext_api::ExtensionEvent::ToolCall(event) = event {
                    *observed_id.lock().expect("observed call ID") = Some(event.tool_call_id.clone());
                }
                Ok(maho_ext_api::EventResult::ToolCall(maho_ext_api::ToolCallEventResult {
                    block: Some(true), reason: Some("fixture admission denied".into()), ..Default::default()
                }))
            }) }));
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
    let resolve = maho_cli::cli::task_runners::live_parent_tools(session.weak_accessor(), tokio::runtime::Handle::current());
    let tool = resolve().into_iter().find(|tool| tool.name() == "guarded").expect("live tool");
    let definition = maho_cli::cli::task_runners::native_shared_parent_tool_definition("guarded", session.weak_accessor())
        .expect("native shared definition");
    assert_eq!(definition.parameters, session.get_tool_definition("guarded").expect("parent definition").parameters);
    let async_result = (definition.execute)(maho_tools::definition::ToolCall {
        id: "async-child-call", params: serde_json::json!({}), signal: Default::default(), on_update: None, context: None,
    }).await;
    assert!(async_result.is_err());
    assert_eq!(admitted_id.lock().expect("observed call ID").as_deref(), Some("async-child-call"));
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
    assert_eq!(admitted_id.lock().expect("observed call ID").as_deref(), Some("child-call"));
}

#[tokio::test]
async fn native_shared_definition_cancels_an_entered_parent_tool() {
    use std::sync::Arc;
    struct ExecutionLifetime(tokio::sync::mpsc::UnboundedSender<()>);
    impl Drop for ExecutionLifetime {
        fn drop(&mut self) { let _ = self.0.send(()); }
    }
    let dir = tempfile::tempdir().expect("isolated shared cancellation");
    let cwd = dir.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    let (dropped, mut drops) = tokio::sync::mpsc::unbounded_channel();
    let tool = maho_ext_api::ToolDefinition::new("pending_child_tool", "pending child fixture",
        serde_json::json!({"type":"object","properties":{}}), Arc::new(move |_| {
            let entered = entered.clone();
            let lifetime = ExecutionLifetime(dropped.clone());
            Box::pin(async move {
                let _lifetime = lifetime;
                entered.send(()).expect("entry observer");
                std::future::pending().await
            })
        }));
    let created = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(provider.get_model(Some("faux-1")).expect("model")),
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        tools: Some(vec!["pending_child_tool".into()]), custom_tools: vec![tool], minimal_resources: true,
        ..Default::default()
    }).await.expect("native parent");
    let session = Arc::new(created.session);
    let definition = maho_cli::cli::task_runners::native_shared_parent_tool_definition(
        "pending_child_tool", session.weak_accessor()).expect("shared definition");
    let signal = maho_tools::definition::AbortSignal::default();

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!((definition.execute)(maho_tools::definition::ToolCall {
            id: "cancelled-child-call", params: serde_json::json!({}), signal: signal.clone(), on_update: None, context: None,
        }), async {
            entries.recv().await.expect("actual parent tool entered");
            signal.abort();
        })
    }).await;
    let dropped = tokio::time::timeout(std::time::Duration::from_secs(5), drops.recv()).await;
    session.dispose().await;

    assert!(outcome.expect("bounded shared cancellation").0.is_err());
    dropped.expect("bounded parent executor drop").expect("executor lifetime ended");
}

#[tokio::test]
async fn native_shared_definition_forwards_parent_tool_updates() {
    use std::sync::{Arc, Mutex};
    let dir = tempfile::tempdir().expect("shared updates");
    let cwd = dir.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let tool = maho_ext_api::ToolDefinition::new("updating_child_tool", "update fixture",
        serde_json::json!({"type":"object","properties":{}}), Arc::new(|call| Box::pin(async move {
            if let Some(update) = call.on_update { update(maho_ext_api::ToolResult::text(call.id))?; }
            Ok(maho_ext_api::ToolResult::text("settled"))
        })));
    let created = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(provider.get_model(Some("faux-1")).expect("model")),
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        tools: Some(vec!["updating_child_tool".into()]), custom_tools: vec![tool], minimal_resources: true,
        ..Default::default()
    }).await.expect("native parent");
    let session = Arc::new(created.session);
    let definition = maho_cli::cli::task_runners::native_shared_parent_tool_definition(
        "updating_child_tool", session.weak_accessor()).expect("shared definition");
    let updates = Arc::new(Mutex::new(Vec::new()));
    let observed = updates.clone();

    let result = (definition.execute)(maho_tools::definition::ToolCall {
        id: "updating-child-call", params: serde_json::json!({}), signal: Default::default(), context: None,
        on_update: Some(Arc::new(move |result| { observed.lock().expect("updates").push(result); Ok(()) })),
    }).await;
    session.dispose().await;

    assert_eq!(result.expect("shared result"), maho_ext_api::ToolResult::text("settled"));
    assert_eq!(*updates.lock().expect("updates"), vec![maho_ext_api::ToolResult::text("updating-child-call")]);
    let retired = (definition.execute)(maho_tools::definition::ToolCall {
        id: "retired-child-call", params: serde_json::json!({}), signal: Default::default(), context: None,
        on_update: None,
    }).await;
    assert!(retired.is_err());
    assert_eq!(updates.lock().expect("retained updates").len(), 1);
}

#[tokio::test]
async fn shared_definition_normalizes_arguments_once_through_child_sdk() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    let dir = tempfile::tempdir().expect("shared normalization");
    let cwd = dir.path().to_string_lossy().into_owned();
    let model = maho_ai::providers::faux::faux_provider(Default::default()).get_model(Some("faux-1")).expect("model");
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut tool = maho_ext_api::ToolDefinition::new("normalizing_child_tool", "normalizing fixture",
        serde_json::json!({"type":"object","properties":{"value":{"type":"integer"}},"required":["value"]}),
        Arc::new(|call| Box::pin(async move { Ok(maho_ext_api::ToolResult::text(call.params["value"].to_string())) })));
    tool.prepare_arguments = Some(Arc::new(move |mut params| {
        observed.fetch_add(1, Ordering::SeqCst);
        params["value"] = serde_json::json!(params["value"].as_i64().expect("integer input") + 1);
        Ok(params)
    }));
    let parent = Arc::new(maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), model: Some(model.clone()), minimal_resources: true,
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        tools: Some(vec![tool.name.clone()]), custom_tools: vec![tool], ..Default::default()
    }).await.expect("parent").session);
    let shared = maho_cli::cli::task_runners::native_shared_parent_tool_definition("normalizing_child_tool", parent.weak_accessor()).expect("shared definition");
    let child = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), model: Some(model), minimal_resources: true,
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        tools: Some(vec![shared.name.clone()]), custom_tools: vec![shared], ..Default::default()
    }).await.expect("child").session;

    let result = child.execute_tool("normalizing_child_tool", serde_json::json!({"value":1}), Default::default()).await;
    child.dispose().await;
    parent.dispose().await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(maho_ai::utils::text::content_text(&result.expect("shared execution").content, ""), "2");
}

#[test]
fn resolved_omo_config_returns_a_live_resolved_root_not_an_envelope() {
    let dir = tempfile::tempdir().expect("isolated config cwd");
    let config = maho_cli::cli::task_runners::resolved_omo_config(dir.path(), &BTreeMap::new());
    assert!(config.get("global").is_none(), "no frozen global envelope: {config}");
    assert!(config.get("project").is_none(), "no frozen project envelope: {config}");
}
