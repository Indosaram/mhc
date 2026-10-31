//! senpi-mcp-16 (commands.ts): `/mcp test` and `/mcp reconnect` failure notifications pass through
//! `redactResolvedReferenceValues`, so a failure that quotes a resolved reference value never reaches the UI.

use crate::reference_support::resolve_env;
use maho_ext_api::*;
use maho_ext_mcp::host_registry::HostMcpRegistry;
use std::path::Path;
use std::sync::{Arc, Mutex};

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

/// Records every notification the `/mcp` command emits.
#[derive(Default)]
pub struct RecordingUi {
    pub notifications: Mutex<Vec<(String, NotificationType)>>,
}
impl ExtensionUi for RecordingUi {
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
    fn notify(&self, message: &str, kind: NotificationType) {
        self.notifications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((message.to_owned(), kind));
    }
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

fn context(ui: Arc<RecordingUi>, root: &Path) -> ExtensionContext {
    ExtensionContext {
        ui,
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: root.into(),
        agent_dir: root.into(),
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
            cwd: root.into(),
            ..Default::default()
        }),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

#[tokio::test]
async fn test_and_reconnect_failures_are_redacted_before_notify() {
    // A server name equal to a resolved value makes the failure text quote it ("Unknown MCP server: <name>").
    let secret = resolve_env("VALUE", "synthetic3858-notify-alpha");
    let root = tempfile::tempdir().expect("root");
    let registered = LoadedExtension::new("builtin:mcp", root.path().into(), SourceInfo::default());
    let mut api = ExtensionApi::new(
        registered,
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    let service = maho_ext_mcp::index::register_mcp_lifecycle(
        &mut api,
        Arc::new(HostMcpRegistry::default()),
        3862,
    );
    let command = api
        .registered
        .commands
        .iter()
        .find(|command| command.name == "mcp")
        .expect("/mcp command")
        .handler
        .clone();
    let ui = Arc::new(RecordingUi::default());
    let ctx = context(ui.clone(), root.path());
    (command)(&format!("test {secret}"), &ctx)
        .await
        .expect("/mcp test");
    (command)(&format!("reconnect {secret}"), &ctx)
        .await
        .expect("/mcp reconnect");

    let notifications = ui
        .notifications
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(notifications.len(), 2, "{notifications:?}");
    for (message, kind) in &notifications {
        assert_eq!(*kind, NotificationType::Error);
        assert!(!message.contains(&secret), "{message}");
        assert!(message.contains("<redacted>"), "{message}");
    }
    assert!(notifications[0].0.starts_with("MCP test <redacted> failed"));
    assert!(
        notifications[1]
            .0
            .starts_with("MCP reconnect <redacted> failed")
    );
    service.lock().await.dispose().await.expect("dispose");
}
