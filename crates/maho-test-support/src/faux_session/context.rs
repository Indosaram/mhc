use std::{path::Path, sync::Arc};

use maho_core::agent_session::AgentSession;
use maho_ext_api::*;

struct SessionView {
    session: Arc<dyn Fn() -> Option<AgentSession> + Send + Sync>,
    id: String,
    actions: Arc<dyn ExtensionContextActions>,
}

impl ToolSessionManager for SessionView {
    fn session_id(&self) -> &str { &self.id }
    fn session_file(&self) -> Option<&Path> { None }
}

fn entry(value: JsonValue) -> SessionEntry {
    SessionEntry {
        id: value["id"].as_str().unwrap_or_default().to_owned(),
        parent_id: value["parentId"].as_str().map(str::to_owned),
        timestamp: value["timestamp"].as_str().unwrap_or_default().to_owned(),
        kind: value["type"].as_str().unwrap_or_default().to_owned(),
        data: value,
    }
}

impl SessionManager for SessionView {
    fn get_entries(&self) -> Vec<SessionEntry> {
        (self.session)().map_or_else(Vec::new, |session| session.with_session_manager(|manager| manager.entries().into_iter().map(entry).collect()))
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        (self.session)().map_or_else(Vec::new, |session| session.with_session_manager(|manager| manager.branch(None).into_iter().map(entry).collect()))
    }
    fn get_leaf_id(&self) -> Option<String> {
        (self.session)().and_then(|session| session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)))
    }
    fn get_session_name(&self) -> Option<String> { (self.session)().and_then(|session| session.session_name()) }
    fn extension_context_actions(&self) -> Option<&dyn ExtensionContextActions> { Some(self.actions.as_ref()) }
}

struct Registry(maho_core::model_registry::ModelRegistry);

impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> { self.0.get_all() }
    fn get_available(&self) -> Vec<Model> { self.0.get_available() }
    fn find(&self, provider: &str, id: &str) -> Option<Model> { self.0.find(provider, id) }
    fn has_configured_auth(&self, model: &Model) -> bool { self.0.has_configured_auth(model) }
    fn get_api_key_for_provider<'a>(&'a self, provider: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async move { Ok(self.0.get_api_key_for_provider(provider).await) })
    }
}

struct HeadlessUi;

impl ExtensionUi for HeadlessUi {
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
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err(ExtensionFailure::new("UI not available")) })
    }
    fn theme(&self) -> Theme { Theme::default() }
}

pub(super) fn create(session: &AgentSession) -> ExtensionContext {
    let actions = session.extension_context_actions();
    let idle = actions.clone();
    let wait = session.weak_accessor();
    let trusted = actions.clone();
    let compacting = actions.clone();
    let prompt = actions.clone();
    let options = actions.clone();
    ExtensionContext {
        ui: Arc::new(HeadlessUi), mode: ExtensionMode::Print, has_ui: false,
        cwd: session.cwd().into(), agent_dir: session.agent_dir().into(),
        session_manager: Arc::new(SessionView { session: session.weak_accessor(), id: session.session_id(), actions }),
        model_registry: Arc::new(Registry(session.model_registry().clone())),
        model: Some(session.model()), thinking_level: None, service_tier: session.service_tier(),
        effective_service_tier: session.effective_service_tier(), scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(move || idle.is_idle()),
        wait_for_idle_fn: Arc::new(move || { let session = wait(); Box::pin(async move { if let Some(session) = session { session.wait_for_idle().await } }) }),
        is_project_trusted_fn: Arc::new(move || trusted.is_project_trusted()),
        is_compacting_fn: Arc::new(move || compacting.is_compacting()),
        get_system_prompt_fn: Arc::new(move || prompt.get_system_prompt()),
        get_system_prompt_options_fn: Arc::new(move || options.get_system_prompt_options()),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default(),
    }
}
