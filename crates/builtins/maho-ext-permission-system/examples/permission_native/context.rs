use maho_core::agent_session::AgentSession;
use maho_ext_api::*;
use std::{path::Path, sync::Arc};

struct SessionView {
    id: String,
    actions: Arc<dyn ExtensionContextActions>,
}
impl ToolSessionManager for SessionView {
    fn session_id(&self) -> &str { &self.id }
    fn session_file(&self) -> Option<&Path> { None }
}
impl SessionManager for SessionView {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
    fn extension_context_actions(&self) -> Option<&dyn ExtensionContextActions> { Some(self.actions.as_ref()) }
}
struct Registry;
impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
}
pub struct DecisionUi(pub bool);
impl ExtensionUi for DecisionUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async move { self.0.then(|| "Allow always".to_owned()) })
    }
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
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err(ExtensionFailure::new("No custom UI in native proof")) }) }
    fn theme(&self) -> Theme { Theme::default() }
}
pub fn create(session: &AgentSession) -> ExtensionContext {
    let actions = session.extension_context_actions();
    ExtensionContext {
        ui: Arc::new(DecisionUi(false)), mode: ExtensionMode::Print, has_ui: false,
        cwd: session.cwd().into(), agent_dir: session.agent_dir().into(),
        session_manager: Arc::new(SessionView { id: session.session_id(), actions }),
        model_registry: Arc::new(Registry), model: Some(session.model()), thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true), is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(String::new), get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None,
    }
}
