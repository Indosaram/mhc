use maho_ext_api::*;
use maho_ext_host::*;
use std::{path::Path, sync::Arc};

struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        "session"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}
struct TestRegistry;
impl ModelRegistry for TestRegistry {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}
struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}
fn context() -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: "/tmp".into(),
        agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(TestSession),
        model_registry: Arc::new(TestRegistry),
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: Vec::new(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions {
            cwd: "/tmp".into(),
            ..Default::default()
        }),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default(),
    }
}

#[tokio::test]
async fn extension_request_uses_real_registered_runner() {
    use maho_server::app_server::registry::{
        ExtensionRequestTarget, JsonRpcError, MethodRegistry, RegistryConnection, register_extension_request_method,
    };
    let runtime = ExtensionRuntime::default();
    let events = EventBus::default();
    let mut api = ExtensionApi::new(
        LoadedExtension::new("echo", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        events.clone(),
        runtime.clone(),
    );
    api.rpc_handle("echo", Arc::new(|data| Box::pin(async move { Ok(data) })))
        .unwrap();
    let runner = Arc::new(ExtensionRunner::new(
        vec![api.registered],
        runtime,
        events,
        context(),
    ));
    let selected = runner.clone();
    let mut registry = MethodRegistry::default();
    register_extension_request_method(
        &mut registry,
        Arc::new(move |id: String| {
            let selected = selected.clone();
            Box::pin(async move {
                if id == "thread" {
                    Ok(selected as Arc<dyn ExtensionRequestTarget>)
                } else {
                    Err(JsonRpcError::new(-32603, "Thread not found"))
                }
            })
        }),
    );
    let connection = RegistryConnection {
        initialized: true,
        experimental_api: false,
        ..Default::default()
    };
    let request = serde_json::json!({"id":1,"method":"extension_request","params":{"threadId":"thread","name":"echo","data":{"value":true}}});
    assert_eq!(
        registry.dispatch(connection.clone(), request.clone()).await["result"],
        serde_json::json!({"value":true})
    );
    assert_eq!(registry.dispatch(connection.clone(),serde_json::json!({"id":2,"method":"extension_request","params":{"threadId":"","name":"echo"}})).await["error"]["code"],-32602);
    runner.invalidate("replaced");
    assert_eq!(
        registry.dispatch(connection, request).await["error"]["code"],
        -32603
    );
}

struct FailingCommand;
impl Extension for FailingCommand {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_command("fail", None, None, Arc::new(|_args, _ctx: &ExtensionContext| Box::pin(async { Err::<(), ExtensionFailure>("boom".into()) })));
    }
}

fn bound_factory(buses: Arc<std::sync::Mutex<Vec<(maho_core::agent_session::AgentSession, EventBus)>>>) -> maho_server::app_server::thread_registry::SessionFactory {
    Arc::new(move |options| {
        let buses = buses.clone();
        Box::pin(async move {
            use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
            use maho_core::agent_session::{AgentSession, AgentSessionConfig};
            let cwd = options.cwd.clone().unwrap_or_default();
            let provider = faux_provider(RegisterFauxProviderOptions {api:Some("faux".into()),models:Some(vec![maho_ai::providers::faux::FauxModelDefinition {id:"faux-1".into(),..Default::default()}]),..Default::default()});
            let model = provider.get_model(Some("faux-1")).ok_or("Missing faux model")?;
            let streams = faux_streams(provider.core.clone());
            let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| streams.stream_simple(model,context,options.map(|options|options.simple)));
            let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {providers:Some(vec![provider.provider.clone()]),..Default::default()});
            let session = AgentSession::new(AgentSessionConfig {
                agent:maho_agent::Agent::new(maho_agent::AgentOptions {initial_state:Some(maho_agent::agent::PartialAgentState {model:Some(model),..Default::default()}),stream_fn:Some(stream_fn),..Default::default()}),
                session_manager:options.session_manager.ok_or("Missing session manager")?,settings_manager:maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),false),
                cwd:cwd.clone(),agent_dir:Some(cwd),fallback_now:Some(Arc::new(||0.0)),retry_random:Some(Arc::new(||0.5)),scoped_models:Vec::new(),favorite_models:Vec::new(),flag_values:Default::default(),custom_tools:Vec::new(),model_runtime:Some(runtime),model_registry:None,uses_default_stream_function:Some(false),initial_active_tool_names:None,default_tool_names:None,eval_only_tool_names:None,allowed_tool_names:None,excluded_tool_names:None,base_tools_override:None,session_start_event:None,auto_title_sessions:Some(false),
            }).map_err(|error|error.to_string())?;
            let runner = ExtensionRunner::from_static(vec![Box::new(FailingCommand)], session.extension_context(Arc::new(TestUi)));
            let events = runner.events.clone();
            session.set_extension_runner(runner).await;
            let bindings = options.extension_bindings.as_ref().map(|factory| factory(&session)).unwrap_or_default();
            session.bind_extensions(bindings).await;
            // Published during binding: must arrive as an initial notification, not be lost.
            events.emit("senpi:extension-rpc-event", &serde_json::json!({"name":"during_bind","data":1}));
            buses.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((session.clone(), events));
            Ok(session)
        })
    })
}

async fn next(receive: &mut tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>, method: &str) -> serde_json::Value {
    tokio::time::timeout(std::time::Duration::from_secs(5), async { loop {
        let message = receive.recv().await.expect("connection alive");
        if message["method"] == method { break message; }
    } }).await.expect("notification within deadline")
}

#[tokio::test]
async fn app_server_relays_extension_events_errors_and_shutdown_to_the_thread() {
    use maho_server::app_server::{envelope::classify_incoming, runtime::AppServerRuntime};
    let directory = tempfile::tempdir().unwrap();
    let buses = Arc::new(std::sync::Mutex::new(Vec::new()));
    let path = directory.path().display().to_string();
    let runtime = AppServerRuntime::new(path.clone(), path.clone(), "1".into(), Some(path), Some(bound_factory(buses.clone()))).await;
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    runtime.core.write().await.add_connection("qa".into(), Arc::new(move |message| { send.send(message).unwrap(); Box::pin(async { Ok(()) }) }));
    runtime.core.read().await.receive("qa", classify_incoming(serde_json::json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"}}}))).await.unwrap();
    runtime.core.read().await.receive("qa", classify_incoming(serde_json::json!({"id":2,"method":"thread/start","params":{}}))).await.unwrap();
    let initial = next(&mut receive, "extension_event").await;
    assert_eq!(initial["params"]["name"], "during_bind");
    let (session, events) = buses.lock().unwrap_or_else(std::sync::PoisonError::into_inner).first().cloned().expect("session created");
    let thread = session.session_id();
    assert_eq!(initial["params"]["threadId"], thread);
    events.emit("senpi:extension-rpc-event", &serde_json::json!({"name":"memory_updated","data":{"n":2}}));
    let live = next(&mut receive, "extension_event").await;
    assert_eq!(live["params"], serde_json::json!({"type":"extension_event","name":"memory_updated","data":{"n":2},"threadId":thread}));
    session.prompt("/fail", maho_core::agent_session::PromptOptions::default()).await.expect("command handled");
    let error = next(&mut receive, "error").await;
    assert_eq!(error["params"]["extensionPath"], "command:fail");
    assert_eq!(error["params"]["error"], "boom");
    maho_ext_api::ExtensionContextActions::shutdown(&*session.extension_context_actions());
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), runtime.shutdown_request.requested()).await.expect("shutdown requested"), "extension shutdown");
    runtime.dispose().await;
}
