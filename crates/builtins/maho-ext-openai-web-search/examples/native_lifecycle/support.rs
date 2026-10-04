use maho_ext_api::*;
use maho_ext_host::*;
use maho_ext_openai_web_search::OpenAiWebSearch;
use serde_json::json;
use std::{path::Path, sync::{Arc, Mutex}};

struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str { "session" }
    fn session_file(&self) -> Option<&Path> { None }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}
struct TestRegistry;
impl ModelRegistry for TestRegistry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
}
#[derive(Default)]
struct TestUi { status: Mutex<Option<String>>, widget: Mutex<bool> }
impl ExtensionUi for TestUi {
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> { Some(self) }
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, key: &str, value: Option<&str>) { assert_eq!(key, "openai-web-search"); *self.status.lock().expect("status") = value.map(str::to_owned); }
    fn set_widget(&self, key: &str, value: Option<WidgetContent>, _: ExtensionWidgetOptions) { assert_eq!(key, "openai-web-search"); *self.widget.lock().expect("widget") = value.is_some(); }
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("UI not available".into()) }) }
    fn theme(&self) -> Theme { Theme::default() }
}
impl ExtensionUiFactories for TestUi {
    fn set_widget_factory(&self, _: &str, _: Option<TuiComponentFactory>, _: ExtensionWidgetOptions) {}
    fn set_header_factory(&self, _: Option<TuiComponentFactory>) {}
    fn set_footer_factory(&self, _: Option<FooterComponentFactory>) {}
    fn custom_factory(&self, _: CustomComponentFactory, _: CustomUiFactoryOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("UI not available".into()) }) }
}
fn context() -> ExtensionContext {
    ExtensionContext { ui: Arc::new(TestUi::default()), mode: ExtensionMode::Print, has_ui: false, cwd: "/tmp".into(), agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(TestSession), model_registry: Arc::new(TestRegistry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions { cwd: "/tmp".into(), ..Default::default() }),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None }

}

fn model(native: bool) -> Model {
    serde_json::from_value(json!({"id":"test","name":"Test","api":"openai-responses","provider":"openai","baseUrl":if native {"https://api.openai.com/v1"} else {"https://api.kimi.com/coding"},"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1000,"maxTokens":100})).expect("model")
}
pub async fn run(native: bool, ui_enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
    let ui = Arc::new(TestUi { status: Mutex::new(Some("stale".into())), widget: Mutex::new(true) });
    let mut ctx = context();
    ctx.model = Some(model(native)); ctx.has_ui = ui_enabled; ctx.ui = ui.clone();
    let loaded = maho_ext_host::loader::load_extensions(vec![maho_ext_host::loader::NativeExtensionFactory { path:"<builtin:openai-web-search>".into(), source_info:SourceInfo::default(), extension:Box::new(OpenAiWebSearch) }], std::path::Path::new("/tmp"), ExtensionSessionProfile::default());
    assert!(loaded.errors.is_empty());
    let mut runner = ExtensionRunner::new(loaded.extensions, loaded.runtime, loaded.events, ctx);
    runner.emit(ExtensionEvent::SessionStart(SessionStartEvent {reason:SessionReason::Startup, initial_model_provenance:None, previous_session_file:None})).await?;
    assert_eq!(ui.status.lock().expect("status").is_none(), ui_enabled);
    assert_eq!(!*ui.widget.lock().expect("widget"), ui_enabled);
    let payload = runner.emit_before_provider_request(json!({"tools":[{"name":"other"}]}),None).await?;
    let injected = payload["tools"].as_array().expect("tools").iter().any(|tool| tool["type"] == "web_search_preview");
    let enabled = maho_ext_openai_web_search::parse_enabled(std::env::var("PI_OPENAI_WEB_SEARCH").ok().as_deref());
    assert_eq!(injected, native && enabled);
    let prompt = runner.emit_before_agent_start(BeforeAgentStartEvent {prompt:"hello".into(), images:None, system_prompt:"base".into(), system_prompt_options:Default::default()}).await?;
    assert_eq!(prompt.is_some(),native && enabled);
    runner.emit(ExtensionEvent::SessionShutdown(SessionShutdownEvent {reason:SessionReason::Quit,target_session_file:None,signal:None})).await?;
    assert!(runner.errors.is_empty());
    runner.runtime.invalidate("native proof complete");
    println!("{}",json!({"native":native,"ui":ui_enabled,"enabled":enabled,"injected":injected,"pass":true}));
    Ok(())
}
