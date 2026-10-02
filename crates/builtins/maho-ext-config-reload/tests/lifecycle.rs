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
struct BoundSession(Arc<dyn ExtensionContextActions>);
impl ToolSessionManager for BoundSession { fn session_id(&self) -> &str { "reload-fixture" } fn session_file(&self) -> Option<&Path> { None } }
impl SessionManager for BoundSession {
    fn get_entries(&self) -> Vec<SessionEntry> { vec![] }
    fn get_branch(&self) -> Vec<SessionEntry> { vec![] }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
    fn extension_context_actions(&self) -> Option<&dyn ExtensionContextActions> { Some(self.0.as_ref()) }
}
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
    let mut trust = ExtensionEvent::ProjectTrust { cwd: root.path().into() };
    let result = (api.registered.handlers[&EventKind::ProjectTrust][0])(&mut trust, &ctx).await.unwrap();
    assert!(matches!(result, EventResult::ProjectTrust(ProjectTrustEventResult { trusted: TrustDecision::Undecided, .. })));
    assert_eq!(receiver.try_recv().unwrap(), serde_json::json!({"enabled":false}));
    let mut shutdown = ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None });
    (api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown, &ctx).await.unwrap();
}
#[tokio::test]
async fn idle_change_reloads_real_session_and_consumes_handoff() {
    real_reload(false).await;
}

#[tokio::test]
async fn vetoed_change_retries_and_completes_real_session_reload() {
    real_reload(true).await;
}

async fn real_reload(veto_once: bool) {
    use maho_core::{sdk::{create_agent_session, CreateAgentSessionOptions}, model_runtime::{ModelRuntime, CreateModelRuntimeOptions}};
    use maho_ext_host::{ExtensionRunner, loader::{load_extensions, NativeExtensionFactory}};
    use maho_ext_config_reload::protocol::*;
    let root = tempfile::tempdir().expect("reload fixture operation must succeed");
    let cwd = root.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let model = provider.get_model(Some("faux-1")).expect("reload fixture operation must succeed");
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        models_path: Some(root.path().join("models.json")), auth_path: Some(root.path().join("auth.json")), providers: Some(vec![provider.provider.clone()]), ..Default::default()
    });
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(cwd.clone()), model: Some(model), model_runtime: Some(runtime),
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        settings_manager: Some(maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), false)),
        auto_title_sessions: Some(false), ..Default::default()
    }).await.expect("reload fixture operation must succeed").session;
    let mut ctx = context(root.path());
    ctx.mode = ExtensionMode::Tui;
    ctx.session_manager = Arc::new(BoundSession(session.extension_context_actions()));
    let (sender, mut reloaded) = tokio::sync::mpsc::unbounded_channel();
    let (veto_sender, mut vetoed) = tokio::sync::mpsc::unbounded_channel();
    struct Veto { reject: Arc<std::sync::atomic::AtomicBool>, sender: tokio::sync::mpsc::UnboundedSender<()> }
    impl Extension for Veto {
        fn register(&self, api: &mut ExtensionApi) {
            let reject = Arc::clone(&self.reject);
            let sender = self.sender.clone();
            api.on(EventKind::SessionBeforeReload, Arc::new(move |_, _| {
                let reject = Arc::clone(&reject);
                let sender = sender.clone();
                Box::pin(async move {
                    let cancel = reject.swap(false, std::sync::atomic::Ordering::SeqCst);
                    if cancel { assert!(sender.send(()).is_ok()); }
                    Ok(EventResult::SessionBefore(SessionBeforeEventResult { cancel: Some(cancel), ..Default::default() }))
                })
            }));
        }
    }
    let reject = Arc::new(std::sync::atomic::AtomicBool::new(veto_once));
    let subscriptions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let retained = Arc::clone(&subscriptions);
    let factory: maho_ext_host::runner::RuntimeFactory = Arc::new(move |ctx| {
        let sender = sender.clone();
        let retained = Arc::clone(&retained);
        let reject = Arc::clone(&reject);
        let veto_sender = veto_sender.clone();
        Box::pin(async move {
            let loaded = load_extensions(vec![
                NativeExtensionFactory { path: "config-reload".into(), source_info: Default::default(), extension: Box::new(maho_ext_config_reload::ConfigReload) },
                NativeExtensionFactory { path: "fixture-veto".into(), source_info: Default::default(), extension: Box::new(Veto { reject, sender: veto_sender }) },
            ], &ctx.cwd, Default::default());
            assert!(loaded.errors.is_empty());
            let subscription = loaded.events.on(CONFIG_WATCH_RELOADED, Arc::new(move |value| { sender.send(value.clone()).expect("reload fixture operation must succeed"); }));
            retained.lock().expect("reload fixture operation must succeed").push(subscription);
            Ok(ExtensionRunner::new(loaded.extensions, loaded.runtime, loaded.events, ctx))
        })
    });
    let mut runner = factory(ctx).await.expect("reload fixture operation must succeed");
    runner.set_runtime_factory(factory);
    session.set_extension_runner(runner).await;
    session.bind_extensions(maho_core::agent_session::ExtensionBindings { mode: Some(ExtensionMode::Tui), ..Default::default() }).await;
    let staged = tempfile::NamedTempFile::new_in(root.path()).expect("reload fixture operation must succeed");
    std::fs::write(staged.path(), "{\"fixture\":\"ctrl+x\"}").expect("reload fixture operation must succeed");
    let path = root.path().join("keybindings.json");
    std::fs::rename(staged.path(), &path).expect("reload fixture operation must succeed");
    if veto_once {
        tokio::time::timeout(std::time::Duration::from_secs(5), vetoed.recv()).await.expect("reload fixture operation must succeed").expect("reload fixture operation must succeed");
        assert!(reloaded.try_recv().is_err());
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), reloaded.recv()).await.expect("reload fixture operation must succeed").expect("reload fixture operation must succeed");
    assert_eq!(result, serde_json::json!({"registrationId":"builtin","paths":[path]}));
    session.dispose().await;
}

#[tokio::test]
async fn invalid_settings_event_is_rejected_and_logged_without_change_delivery() {
    use maho_ext_config_reload::protocol::*;
    let root = tempfile::tempdir().unwrap();
    let events = EventBus::default();
    let (rejected_sender, mut rejected) = tokio::sync::mpsc::unbounded_channel();
    let (changed_sender, mut changed) = tokio::sync::mpsc::unbounded_channel();
    let _rejected = events.on(CONFIG_WATCH_REJECTED, Arc::new(move |value| { rejected_sender.send(value.clone()).unwrap(); }));
    let _changed = events.on(CONFIG_WATCH_CHANGED, Arc::new(move |value| { changed_sender.send(value.clone()).unwrap(); }));
    let mut api = ExtensionApi::new(LoadedExtension::new("config-reload", root.path().into(), SourceInfo::default()), ExtensionSessionProfile::default(), events, ExtensionRuntime::default());
    maho_ext_config_reload::ConfigReload.register(&mut api);
    let mut ctx = context(root.path());
    ctx.mode = ExtensionMode::Tui;
    let mut start = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None });
    (api.registered.handlers[&EventKind::SessionStart][0])(&mut start, &ctx).await.unwrap();
    let staged = tempfile::NamedTempFile::new_in(root.path()).unwrap();
    std::fs::write(staged.path(), "{ invalid").unwrap();
    let path = root.path().join("settings.json");
    std::fs::rename(staged.path(), &path).unwrap();
    let rejection = tokio::time::timeout(std::time::Duration::from_secs(5), rejected.recv()).await.unwrap().unwrap();
    assert_eq!(rejection["registrationId"], "builtin");
    assert_eq!(rejection["paths"], serde_json::json!([path]));
    assert!(!rejection["errors"].as_array().unwrap().is_empty());
    let mut shutdown = ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None });
    (api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown, &ctx).await.unwrap();
    assert!(changed.try_recv().is_err());
    let entries: Vec<serde_json::Value> = std::fs::read_to_string(root.path().join("logs/config-reload.log")).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert!(entries.iter().any(|entry| entry["event"] == "validation_rejected"));
    assert!(!entries.iter().any(|entry| entry["event"] == "reload_requested"));
}

#[tokio::test]
async fn external_bus_registration_rebuilds_watches_and_delivers_its_group() {
    // Given
    use maho_ext_config_reload::protocol::*;
    let root = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let events = EventBus::default();
    let (ready_sender, mut ready) = tokio::sync::mpsc::unbounded_channel();
    let (change_sender, mut changes) = tokio::sync::mpsc::unbounded_channel();
    let _ready = events.on(CONFIG_WATCH_READY, Arc::new(move |value| { ready_sender.send(value.clone()).unwrap(); }));
    let _changes = events.on(CONFIG_WATCH_CHANGED, Arc::new(move |value| { change_sender.send(value.clone()).unwrap(); }));
    let mut api = ExtensionApi::new(LoadedExtension::new("config-reload", root.path().into(), SourceInfo::default()), ExtensionSessionProfile::default(), events.clone(), ExtensionRuntime::default());
    maho_ext_config_reload::ConfigReload.register(&mut api);
    let mut ctx = context(root.path());
    ctx.mode = ExtensionMode::Tui;
    ctx.is_idle_fn = Arc::new(|| false);
    let mut start = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None });
    (api.registered.handlers[&EventKind::SessionStart][0])(&mut start, &ctx).await.unwrap();
    ready.try_recv().unwrap();
    let path = external.path().join("fixture.json");
    // When
    events.emit(CONFIG_WATCH_REGISTER, &serde_json::json!({"id":"external","displayName":"fixture","targets":[{"path":path,"kind":"file"}]}));
    tokio::time::timeout(std::time::Duration::from_secs(5), ready.recv()).await.unwrap().unwrap();
    let staged = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(staged.path(), "{}").unwrap();
    std::fs::rename(staged.path(), &path).unwrap();
    let change = tokio::time::timeout(std::time::Duration::from_secs(5), changes.recv()).await.unwrap().unwrap();
    // Then
    assert_eq!(change, serde_json::json!({"registrationId":"external","paths":[path],"deferred":true}));
    events.emit(CONFIG_WATCH_UNREGISTER, &serde_json::json!({"id":"external"}));
    tokio::time::timeout(std::time::Duration::from_secs(5), ready.recv()).await.unwrap().unwrap();
    let entries: Vec<serde_json::Value> = std::fs::read_to_string(root.path().join("logs/config-reload.log")).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    for event in ["registration_added", "registration_removed"] {
        assert!(entries.iter().any(|entry| entry["event"] == event && entry["id"] == "external"));
    }
    let mut shutdown = ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None });
    (api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown, &ctx).await.unwrap();
}

#[tokio::test]
async fn active_session_delivers_validated_change_and_joins_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let events = EventBus::default();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let scope = maho_ai::node::provider_scope::ProviderScope::new();
    let expected_scope = scope.clone();
    let _subscription = events.on(maho_ext_config_reload::protocol::CONFIG_WATCH_CHANGED, Arc::new(move |value| {
        assert!(maho_ai::node::provider_scope::active_provider_scope().is_some_and(|scope| scope.ptr_eq(&expected_scope)));
        sender.send(value.clone()).unwrap();
    }));
    let mut api = ExtensionApi::new(LoadedExtension::new("config-reload", root.path().into(), SourceInfo::default()), ExtensionSessionProfile::default(), events, ExtensionRuntime::default());
    maho_ext_config_reload::ConfigReload.register(&mut api);
    let mut ctx = context(root.path());
    ctx.mode = ExtensionMode::Tui;
    ctx.is_idle_fn = Arc::new(|| false);
    let mut start = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None });
    maho_ai::node::provider_scope::run_with_provider_scope_async(&scope, (api.registered.handlers[&EventKind::SessionStart][0])(&mut start, &ctx)).await.unwrap().unwrap();
    let staged = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(staged.path(), "{\"fixture\":\"ctrl+x\"}").unwrap();
    let path = root.path().join("keybindings.json");
    std::fs::rename(staged.path(), &path).unwrap();
    let change = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.unwrap().unwrap();
    assert_eq!(change, serde_json::json!({"registrationId":"builtin","paths":[path],"deferred":true}));
    let mut shutdown = ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None });
    (api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown, &ctx).await.unwrap();
    let entries: Vec<serde_json::Value> = std::fs::read_to_string(root.path().join("logs/config-reload.log")).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert!(entries.iter().any(|entry| entry["event"] == "watcher_started"));
    assert!(entries.iter().any(|entry| entry["event"] == "change_detected" && entry["deferred"] == true));
}
#[test]
fn registers_native_start_idle_and_shutdown_hooks() {
    let mut api = ExtensionApi::new(LoadedExtension::new("config-reload", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_ext_config_reload::ConfigReload.register(&mut api);
    for kind in [EventKind::SessionStart, EventKind::AgentEnd, EventKind::AgentSettled, EventKind::ProjectTrust, EventKind::SessionShutdown] { assert_eq!(api.registered.handlers[&kind].len(), 1); }
}
#[tokio::test]
async fn missing_prompt_directory_rearms_before_observing_its_files() {
    use maho_ext_config_reload::protocol::*;
    let root = tempfile::tempdir().unwrap();
    let events = EventBus::default();
    let (ready_sender, mut ready) = tokio::sync::mpsc::unbounded_channel();
    let (change_sender, mut changes) = tokio::sync::mpsc::unbounded_channel();
    let _ready = events.on(CONFIG_WATCH_READY, Arc::new(move |value| { ready_sender.send(value.clone()).unwrap(); }));
    let _changes = events.on(CONFIG_WATCH_CHANGED, Arc::new(move |value| { change_sender.send(value.clone()).unwrap(); }));
    let mut api = ExtensionApi::new(LoadedExtension::new("config-reload", root.path().into(), SourceInfo::default()), ExtensionSessionProfile::default(), events, ExtensionRuntime::default());
    maho_ext_config_reload::ConfigReload.register(&mut api);
    let mut ctx = context(root.path());
    ctx.mode = ExtensionMode::Tui;
    ctx.is_idle_fn = Arc::new(|| false);
    let mut start = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None });
    (api.registered.handlers[&EventKind::SessionStart][0])(&mut start, &ctx).await.unwrap();
    ready.try_recv().unwrap();
    let prompts = root.path().join("prompts");
    std::fs::create_dir(&prompts).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), ready.recv()).await.unwrap().unwrap();
    let directory_change = changes.try_recv().unwrap();
    assert_eq!(directory_change["paths"], serde_json::json!([prompts]));
    let staged = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(staged.path(), "fixture").unwrap();
    let path = prompts.join("fixture.md");
    std::fs::rename(staged.path(), &path).unwrap();
    let change = tokio::time::timeout(std::time::Duration::from_secs(5), changes.recv()).await.unwrap().unwrap();
    assert_eq!(change["paths"], serde_json::json!([path]));
    let mut shutdown = ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None });
    (api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown, &ctx).await.unwrap();
}
