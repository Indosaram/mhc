use std::{path::Path, sync::{Arc, Mutex}};

use maho_codemode::tool::image_resize::{EvalImageSdk, EvalImageContent, ImageFuture, ResizedImage};
use maho_ext_api::*;

struct Images;

impl EvalImageSdk for Images {
    fn resize_image<'a>(&'a self, _: Vec<u8>, _: &'a str, _: Option<usize>) -> ImageFuture<'a, Option<ResizedImage>> {
        Box::pin(async { panic!("completion-handle test must not resize images") })
    }

    fn convert_to_png<'a>(&'a self, _: &'a str, _: &'a str) -> ImageFuture<'a, Option<EvalImageContent>> {
        Box::pin(async { panic!("completion-handle test must not convert images") })
    }
}

struct Session(std::path::PathBuf);

impl ToolSessionManager for Session {
    fn session_id(&self) -> &str { "completion-handle-session" }
    fn session_file(&self) -> Option<&Path> { Some(&self.0) }
}

impl SessionManager for Session {
    fn get_entries(&self) -> Vec<SessionEntry> { vec![] }
    fn get_branch(&self) -> Vec<SessionEntry> { vec![] }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}

struct Registry;

impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> { vec![] }
    fn get_available(&self) -> Vec<Model> { vec![] }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

struct Ui;

impl ExtensionUi for Ui {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("headless".into()) }) }
    fn theme(&self) -> Theme { Theme::default() }
}

fn context(root: &Path, session_file: &Path) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(Ui), mode: ExtensionMode::Print, has_ui: false, cwd: root.into(), agent_dir: root.join("agent"),
        session_manager: Arc::new(Session(session_file.into())), model_registry: Arc::new(Registry), model: None,
        thinking_level: None, service_tier: None, effective_service_tier: None, scoped_models: vec![], goal_store_file: None,
        loaded_extension_paths: vec![], signal: None, steering_signal: None, is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default), registered_mcp_servers: vec![],
        update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

#[derive(Default)]
struct Host { tools: Mutex<Vec<RegisteredTool>> }

impl ExtensionActions for Host {
    fn send_message(&self, _: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> { Ok(()) }
    fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> { Ok(()) }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> { Ok(()) }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(self.tools.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.iter().map(|tool| ToolInfo {
            name: tool.definition.name.clone(), label: tool.definition.label.clone(), description: tool.definition.description.clone(),
            parameters: tool.definition.parameters.clone(), prompt_guidelines: tool.definition.prompt_guidelines.clone(),
            source_info: tool.source_info.clone(), exposure: ToolExposure::Direct, search_text: None, search_keywords: vec![],
            search_group: None, allow_lazy_activation: false, kernel_prelude: tool.definition.kernel_prelude.clone(),
        }).collect())
    }
}

impl ExtensionSessionActions for Host {
    fn set_session_name(&self, _: &str) -> Result<(), ExtensionFailure> { Ok(()) }
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> { Ok(None) }
    fn set_label(&self, _: &str, _: Option<&str>) -> Result<(), ExtensionFailure> { Ok(()) }
    fn execute_tool<'a>(&'a self, name: &'a str, _: JsonValue, _: ExecuteToolOptions) -> ExecuteToolFuture<'a> {
        Box::pin(async move { Err(ExecuteToolError { code: ExecuteToolErrorCode::UnknownTool, tool_name: name.into(), message: "unknown fixture tool".into(), active_tools: vec![] }) })
    }
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> { Ok(vec!["eval".into()]) }
    fn set_active_tools(&self, _: Vec<String>) -> Result<(), ExtensionFailure> { Ok(()) }
    fn refresh_tools(&self) -> Result<(), ExtensionFailure> { Ok(()) }
    fn install_registered_tool(&self, tool: RegisteredTool) -> Result<(), ExtensionFailure> {
        let mut tools = self.tools.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
        if let Some(old) = tools.iter_mut().find(|old| old.definition.name == tool.definition.name) { *old = tool; }
        else { tools.push(tool); }
        Ok(())
    }
    fn register_removed_tool_hint(&self, _: &str, _: &str) -> Result<(), ExtensionFailure> { Ok(()) }
    fn register_lazy_tool_activator(&self, _: LazyToolActivator) -> Result<(), ExtensionFailure> { Ok(()) }
    fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure> { Ok(vec![]) }
    fn set_model(&self, _: Model) -> ExtensionFuture<'_, bool> { Box::pin(async { Ok(true) }) }
    fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure> { Ok(ThinkingLevel::Minimal) }
    fn set_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> { Ok(()) }
    fn set_session_model(&self, _: Model) -> ExtensionFuture<'_, bool> { Box::pin(async { Ok(true) }) }
    fn set_session_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> { Ok(()) }
    fn set_session_fast_mode(&self, _: bool) -> Result<(), ExtensionFailure> { Ok(()) }
    fn exec<'a>(&'a self, _: &'a str, _: &'a [String], _: &'a Path, _: ExecOptions) -> ExtensionFuture<'a, ExecResult> {
        Box::pin(async { panic!("completion-handle test must not exec processes") })
    }
}

#[tokio::test]
async fn real_eval_completion_handle_survives_cells_but_not_session_switch() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".omo")).unwrap();
    std::fs::write(root.path().join(".omo/codemode.json"), r#"{"languages":{"js":true}}"#).unwrap();

    let host = Arc::new(Host::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(host.clone());
    runtime.bind_session_actions(host.clone());
    let mut api = ExtensionApi::new(LoadedExtension::new("codemode", root.path().into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), runtime);
    let (entered, mut entered_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (release, released) = tokio::sync::oneshot::channel();
    let released = Arc::new(Mutex::new(Some(released)));
    maho_codemode::register(&mut api, maho_codemode::CodemodeExtensionOptions {
        image_sdk: Arc::new(Images),
        complete: Arc::new(move |request, _| {
            let entered = entered.clone();
            let release = released.lock().unwrap().take();
            Box::pin(async move {
                entered.send(request.prompt).unwrap();
                if let Some(release) = release { release.await.unwrap(); }
                Ok(serde_json::json!({"text":"fixture completion fulfilled"}))
            })
        }),
        home_dir: root.path().into(), environment: Default::default(),
        js_runtime: maho_codemode::tool::types::EvalRuntimeInfo { name: "bun".into(), version: "1.4.0".into(), path: None },
    }).unwrap();

    let session_a = root.path().join("session-a.jsonl");
    let ctx_a = context(root.path(), &session_a);
    let mut start = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None });
    tokio::time::timeout(std::time::Duration::from_secs(10), (api.registered.handlers[&EventKind::SessionStart][0])(&mut start, &ctx_a)).await.unwrap().unwrap();
    let execute = host.tools.lock().unwrap().last().unwrap().definition.execute.clone();
    let first = execute(maho_tools::definition::ToolCall {
        id: "completion-handle-create",
        params: serde_json::json!({"language":"js","code":"globalThis.savedCompletion = await completion('fixture', {handle:true}); print(JSON.stringify(savedCompletion))","summary":"create completion handle","on_timeout":"error"}),
        signal: Default::default(), on_update: None, context: Some(&ctx_a),
    });
    let first = tokio::time::timeout(std::time::Duration::from_secs(10), first).await.unwrap().unwrap();
    let saved: serde_json::Value = serde_json::from_str(first.details.as_ref().unwrap()["cells"][0]["output"].as_str().unwrap()).unwrap();
    assert!(saved["handle"].as_str().unwrap().starts_with("completion://"));
    let saved_ref = serde_json::json!({"kind":"completion","id":saved["id"],"run_epoch":saved["run_epoch"]});

    let second_context = ctx_a.clone();
    let second_execute = execute.clone();
    let second = tokio::spawn(async move { second_execute(maho_tools::definition::ToolCall {
        id: "completion-handle-wait",
        params: serde_json::json!({"language":"js","code":"print(JSON.stringify(await wait([savedCompletion])))","summary":"await completion handle","on_timeout":"error"}),
        signal: Default::default(), on_update: None, context: Some(&second_context),
    }).await });
    let prompt = tokio::time::timeout(std::time::Duration::from_secs(10), entered_rx.recv()).await.unwrap().unwrap();
    assert_eq!(prompt, "fixture");
    release.send(()).unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(10), second).await.unwrap().unwrap().unwrap();
    let output = second.details.as_ref().unwrap()["cells"][0]["output"].as_str().unwrap();
    assert!(output.contains("fixture completion fulfilled"), "completion output was not returned: {output}");

    let mut before_switch = ExtensionEvent::SessionBeforeSwitch { reason: SessionReason::New, target_session_file: Some(root.path().join("session-b.jsonl").display().to_string()) };
    tokio::time::timeout(std::time::Duration::from_secs(10), (api.registered.handlers[&EventKind::SessionBeforeSwitch][0])(&mut before_switch, &ctx_a)).await.unwrap().unwrap();

    let session_b = root.path().join("session-b.jsonl");
    let ctx_b = context(root.path(), &session_b);
    let mut restart = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Resume, initial_model_provenance: None, previous_session_file: Some(session_a.display().to_string()) });
    tokio::time::timeout(std::time::Duration::from_secs(10), (api.registered.handlers[&EventKind::SessionStart][0])(&mut restart, &ctx_b)).await.unwrap().unwrap();
    let execute_b = host.tools.lock().unwrap().last().unwrap().definition.execute.clone();
    let stale = execute_b(maho_tools::definition::ToolCall {
        id: "completion-handle-old-session",
        params: serde_json::json!({"language":"js","code":format!("await wait([{}])", saved_ref),"summary":"reject old completion ref","on_timeout":"error"}),
        signal: Default::default(), on_update: None, context: Some(&ctx_b),
    });
    let stale = tokio::time::timeout(std::time::Duration::from_secs(10), stale).await.unwrap().unwrap();
    assert_eq!(stale.details.as_ref().unwrap()["isError"], true);
    let details = stale.details.as_ref().expect("stale handle result details");
    let output = details["cells"][0]["output"].as_str().unwrap_or_else(|| panic!("stale handle cell output missing: {details}"));
    assert!(output.contains("eval_handle_not_found"), "stale handle result: {details}");

    let mut shutdown = ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None });
    tokio::time::timeout(std::time::Duration::from_secs(10), (api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown, &ctx_b)).await.unwrap().unwrap();
}
