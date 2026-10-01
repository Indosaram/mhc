use maho_ext_api::{EventBus, EventKind, Extension, ExtensionApi, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension, SourceInfo};
use maho_ext_api::*;
use std::{path::Path, sync::Arc};
struct FixtureSession;
impl ToolSessionManager for FixtureSession { fn session_id(&self) -> &str { "fixture" } fn session_file(&self) -> Option<&Path> { None } }
impl SessionManager for FixtureSession {
    fn get_entries(&self) -> Vec<SessionEntry> { vec![] } fn get_branch(&self) -> Vec<SessionEntry> { vec![] }
    fn get_leaf_id(&self) -> Option<String> { None } fn get_session_name(&self) -> Option<String> { None }
}
struct FixtureRegistry;
impl ModelRegistry for FixtureRegistry {
    fn get_all(&self) -> Vec<Model> { vec![] } fn get_available(&self) -> Vec<Model> { vec![] }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None } fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
}
struct FixtureUi;
impl ExtensionUi for FixtureUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: NotificationType) {} fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {} fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {} fn set_title(&self, _: &str) {} fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {} fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("fixture UI unavailable".into()) }) }
    fn theme(&self) -> Theme { Theme::default() }
}
fn context(root: &Path) -> ExtensionContext {
    ExtensionContext { ui: Arc::new(FixtureUi), mode: ExtensionMode::Print, has_ui: false, cwd: root.into(), agent_dir: root.into(),
        session_manager: Arc::new(FixtureSession), model_registry: Arc::new(FixtureRegistry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: vec![], goal_store_file: None, loaded_extension_paths: vec![], signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| false), is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(String::new), get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default), registered_mcp_servers: vec![], update_tool_hook_status: None }
}
#[tokio::test]
async fn print_session_emits_disabled_readiness_and_shutdown_joins() {
    let root = tempfile::tempdir().unwrap();
    let events = EventBus::default();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = events.on(maho_ext_config_reload::protocol::CONFIG_WATCH_READY, Arc::new(move |value| { sender.send(value.clone()).unwrap(); }));
    let mut api = ExtensionApi::new(LoadedExtension::new("config-reload", root.path().into(), SourceInfo::default()), ExtensionSessionProfile::default(), events, ExtensionRuntime::default());
    maho_ext_config_reload::ConfigReload.register(&mut api);
    let ctx = context(root.path());
    let mut start = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None });
    (api.registered.handlers[&EventKind::SessionStart][0])(&mut start, &ctx).await.unwrap();
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.unwrap().unwrap(), serde_json::json!({"enabled":false}));
    let mut shutdown = ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None });
    (api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown, &ctx).await.unwrap();
}
#[tokio::test]
async fn active_session_delivers_validated_change_and_joins_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let events = EventBus::default();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = events.on(maho_ext_config_reload::protocol::CONFIG_WATCH_CHANGED, Arc::new(move |value| { sender.send(value.clone()).unwrap(); }));
    let mut api = ExtensionApi::new(LoadedExtension::new("config-reload", root.path().into(), SourceInfo::default()), ExtensionSessionProfile::default(), events, ExtensionRuntime::default());
    maho_ext_config_reload::ConfigReload.register(&mut api);
    let mut ctx = context(root.path());
    ctx.mode = ExtensionMode::Tui;
    ctx.is_idle_fn = Arc::new(|| false);
    let mut start = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None });
    (api.registered.handlers[&EventKind::SessionStart][0])(&mut start, &ctx).await.unwrap();
    let staged = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(staged.path(), "{\"fixture\":\"ctrl+x\"}").unwrap();
    let path = root.path().join("keybindings.json");
    std::fs::rename(staged.path(), &path).unwrap();
    let change = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.unwrap().unwrap();
    assert_eq!(change, serde_json::json!({"registrationId":"builtin","paths":[path],"deferred":true}));
    let mut shutdown = ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None });
    (api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown, &ctx).await.unwrap();
}
#[test]
fn registers_native_start_idle_and_shutdown_hooks() {
    let mut api = ExtensionApi::new(LoadedExtension::new("config-reload", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_ext_config_reload::ConfigReload.register(&mut api);
    for kind in [EventKind::SessionStart, EventKind::AgentEnd, EventKind::AgentSettled, EventKind::SessionShutdown] { assert_eq!(api.registered.handlers[&kind].len(), 1); }
}
