//! Shared test context for tests that need an `ExtensionContext` but no live host.
use maho_ext_api::{
    ExtensionContext, ExtensionMode, ExtensionUi, ExtensionUiDialogOptions, JsonValue, Model,
    ModelRegistry, NotificationType, SessionEntry, SessionManager, Theme, ToolSessionManager,
    UiFuture,
};
use std::{path::Path, sync::Arc};

pub(crate) struct TestSession(pub(crate) String);
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        &self.0
    }
    fn session_file(&self) -> Option<&std::path::Path> {
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

pub(crate) struct TestModels;
impl ModelRegistry for TestModels {
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
    fn get_api_key_for_provider<'a>(
        &'a self,
        _: &'a str,
    ) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

pub(crate) struct TestUi;
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
    fn set_widget(
        &self,
        _: &str,
        _: Option<maho_ext_api::WidgetContent>,
        _: maho_ext_api::ExtensionWidgetOptions,
    ) {
    }
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        _: maho_ext_api::ComponentFactory,
        _: maho_ext_api::CustomUiOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Ok(JsonValue::Null) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

/// A print-mode context over `session` in `cwd`.
pub(crate) fn context(session: Arc<dyn SessionManager>, cwd: &Path) -> ExtensionContext {
    build(Arc::new(TestUi), session, cwd, ExtensionMode::Print, false)
}

/// A TUI-mode context over `session` in `cwd`, driving `ui`.
pub(crate) fn context_with_ui(
    ui: Arc<dyn ExtensionUi>,
    session: Arc<dyn SessionManager>,
    cwd: &Path,
) -> ExtensionContext {
    build(ui, session, cwd, ExtensionMode::Tui, true)
}

fn build(
    ui: Arc<dyn ExtensionUi>,
    session: Arc<dyn SessionManager>,
    cwd: &Path,
    mode: ExtensionMode,
    has_ui: bool,
) -> ExtensionContext {
    ExtensionContext {
        ui,
        mode,
        has_ui,
        cwd: cwd.to_path_buf(),
        agent_dir: cwd.join(".omo"),
        session_manager: session,
        model_registry: Arc::new(TestModels),
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
        get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(maho_ext_api::BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        logger: None,
        idle_coordinator: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}
