use std::{path::Path, sync::Arc};

use maho_ext_api::*;
use crate::agent_session::AgentSession;

struct SessionView {
    session: Arc<dyn Fn() -> Option<AgentSession> + Send + Sync>,
    id: String,
    file: Option<std::path::PathBuf>,
    actions: Arc<dyn ExtensionContextActions>,
}

impl ToolSessionManager for SessionView {
    fn session_id(&self) -> &str { &self.id }
    fn session_file(&self) -> Option<&Path> { self.file.as_deref() }
}

fn entry(value: JsonValue) -> SessionEntry {
    SessionEntry {
        id: value["id"].as_str().unwrap_or_default().to_owned(),
        parent_id: value["parentId"].as_str().map(str::to_owned),
        timestamp: value["timestamp"].as_str().unwrap_or_default().to_owned(),
        kind: value["type"].as_str().unwrap_or_default().to_owned(), data: value,
    }
}

impl SessionManager for SessionView {
    fn get_session_dir(&self) -> Option<std::path::PathBuf> {
        (self.session)().map(|session| session.with_session_manager(|manager| manager.session_dir().into()))
    }
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

pub(crate) fn create(session: &AgentSession) -> ExtensionContext {
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
        session_manager: Arc::new(SessionView { session: session.weak_accessor(), id: session.session_id(),
            file: session.session_file().map(Into::into), actions }),
        model_registry: Arc::new(crate::agent_session::ExtensionModelRegistryView::new(session, Default::default())),
        model: Some(session.model()), thinking_level: None, service_tier: session.service_tier(),
        effective_service_tier: session.effective_service_tier(), scoped_models: Vec::new(), goal_store_file: goal_store_file(session),
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

/// The pinned `goalPathsFromContext` session-directory path:
/// `<sessionDir>/extensions/goal/<encodeURIComponent(sessionId)>.json`. The reader treats a missing
/// file as "no goal", so this is the real path (not a session-file proxy).
fn goal_store_file(session: &AgentSession) -> Option<std::path::PathBuf> {
    let dir = session.with_session_manager(|manager| manager.session_dir().to_owned());
    if dir.is_empty() {
        return None;
    }
    Some(
        std::path::Path::new(&dir)
            .join("extensions/goal")
            .join(format!("{}.json", encode_uri_component(&session.session_id()))),
    )
}

/// `encodeURIComponent`: keep the unreserved set and percent-encode the rest (UTF-8 bytes).
pub(crate) fn encode_uri_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::encode_uri_component;

    #[test]
    fn encode_uri_component_keeps_the_unreserved_set_and_escapes_the_rest() {
        assert_eq!(encode_uri_component("abc-_.!~*'()"), "abc-_.!~*'()");
        assert_eq!(encode_uri_component("a/b c"), "a%2Fb%20c");
        assert_eq!(encode_uri_component("세션"), "%EC%84%B8%EC%85%98");
    }
}
