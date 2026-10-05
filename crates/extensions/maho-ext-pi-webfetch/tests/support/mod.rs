use maho_ext_api::*;

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
pub struct TestUi { pub cleared: Mutex<Vec<String>> }
impl ExtensionUi for TestUi {
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> { Some(self) }
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, key: &str, text: Option<&str>) { if text.is_none() { self.cleared.lock().expect("UI receipt").push(format!("status:{key}")); } }
    fn set_widget(&self, key: &str, content: Option<WidgetContent>, _: ExtensionWidgetOptions) { if content.is_none() { self.cleared.lock().expect("UI receipt").push(format!("widget:{key}")); } }
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
pub fn context() -> ExtensionContext {
    ExtensionContext { ui: Arc::new(TestUi::default()), mode: ExtensionMode::Print, has_ui: false, cwd: "/tmp".into(), agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(TestSession), model_registry: Arc::new(TestRegistry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions { cwd: "/tmp".into(), ..Default::default() }),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default() }
}


#[derive(Default)]
pub struct SessionActions { active: Mutex<Vec<String>>, name: Mutex<Option<String>>, fast: Mutex<bool>, entries: Mutex<Vec<(String, Option<JsonValue>)>> }
impl ExtensionSessionActions for SessionActions {
    fn set_session_name(&self, name: &str) -> Result<(), ExtensionFailure> { *self.name.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(name.into()); Ok(()) }
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> { Ok(self.name.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()) }
    fn set_label(&self, _: &str, _: Option<&str>) -> Result<(), ExtensionFailure> { Err("not used".into()) }
    fn execute_tool<'a>(&'a self, name: &'a str, _: JsonValue, _: ExecuteToolOptions) -> ExecuteToolFuture<'a> {
        Box::pin(async move {
            let active = self.get_active_tools().unwrap_or_default();
            Err(ExecuteToolError { code: ExecuteToolErrorCode::InactiveTool, tool_name: name.into(), message: "inactive".into(), active_tools: active })
        })
    }
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> { Ok(self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()) }
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure> { *self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = names; Ok(()) }
    fn refresh_tools(&self) -> Result<(), ExtensionFailure> { self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(("refresh".into(), None)); Ok(()) }
    fn register_removed_tool_hint(&self, name: &str, hint: &str) -> Result<(), ExtensionFailure> { self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((name.into(), Some(JsonValue::String(hint.into())))); Ok(()) }
    fn register_lazy_tool_activator(&self, activator: LazyToolActivator) -> Result<(), ExtensionFailure> { self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(("activator".into(), Some(JsonValue::Bool(activator("hidden"))))); Ok(()) }
    fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure> { Ok(vec![]) }
    fn set_model(&self, _: Model) -> ExtensionFuture<'_, bool> { Box::pin(async { Err("not used".into()) }) }
    fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure> { Ok(ThinkingLevel::Low) }
    fn set_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> { Ok(()) }
    fn set_session_model(&self, _: Model) -> ExtensionFuture<'_, bool> { Box::pin(async { Err("not used".into()) }) }
    fn set_session_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> { Ok(()) }
    fn set_session_fast_mode(&self, enabled: bool) -> Result<(), ExtensionFailure> { *self.fast.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = enabled; Ok(()) }
    fn exec<'a>(&'a self, _: &'a str, _: &'a [String], _: &'a Path, _: ExecOptions) -> ExtensionFuture<'a, ExecResult> { Box::pin(async { Err("not used".into()) }) }
}

pub fn registered_tool() -> maho_agent::types::AgentTool {
 let loaded = maho_ext_host::loader::load_extensions(vec![maho_ext_host::loader::NativeExtensionFactory { path: "pi-webfetch".into(), source_info: SourceInfo { path: "pi-webfetch".into(), ..Default::default() }, extension: Box::new(maho_ext_pi_webfetch::WebfetchExtension) }], Path::new("/tmp"), Default::default());
 assert!(loaded.errors.is_empty());
 loaded.runtime.bind_session_actions(Arc::new(SessionActions::default()));
 maho_ext_host::wrapper::wrap_registered_tool(loaded.extensions[0].tools[0].clone(), loaded.runtime, Arc::new(|| Ok(context())))
}
