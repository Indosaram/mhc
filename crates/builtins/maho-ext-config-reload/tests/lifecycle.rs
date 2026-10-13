use maho_ext_api::{EventBus, EventKind, Extension, ExtensionApi, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension, SourceInfo};
use maho_ext_api::*;
use std::{path::Path, sync::Arc};
use futures::future::BoxFuture;
use maho_ext_config_reload::watch_engine::WatchClock;

struct RetryClock {
    now: Arc<std::sync::Mutex<tokio::time::Instant>>,
    armed: tokio::sync::mpsc::UnboundedSender<tokio::time::Instant>,
    advanced: Arc<tokio::sync::Notify>,
}
impl RetryClock {
    fn advance_to(&self, deadline: tokio::time::Instant) {
        let mut now = self.now.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if deadline > *now { *now = deadline; }
        self.advanced.notify_waiters();
    }
}
impl WatchClock for RetryClock {
    fn now(&self) -> tokio::time::Instant { *self.now.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }
    fn sleep_until(&self, deadline: tokio::time::Instant) -> BoxFuture<'static, ()> {
        let now = Arc::clone(&self.now);
        let armed = self.armed.clone();
        let advanced = Arc::clone(&self.advanced);
        Box::pin(async move {
            armed.send(deadline).expect("clock deadline receiver remains active");
            loop {
                let notified = advanced.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if *now.lock().unwrap_or_else(std::sync::PoisonError::into_inner) >= deadline { return; }
                notified.await;
            }
        })
    }
}
struct FixtureSession;
impl ToolSessionManager for FixtureSession { fn session_id(&self) -> &str { "fixture" } fn session_file(&self) -> Option<&Path> { None } }
impl SessionManager for FixtureSession {
    fn get_entries(&self) -> Vec<SessionEntry> { vec![] } fn get_branch(&self) -> Vec<SessionEntry> { vec![] }
    fn get_leaf_id(&self) -> Option<String> { None } fn get_session_name(&self) -> Option<String> { None }
}
struct FixtureRegistry;
struct BoundSession(Arc<dyn ExtensionContextActions>);
/// A bound session manager that reports the real session's id, so a reload can be checked for identity.
struct IdentifiedSession(Arc<dyn ExtensionContextActions>, String);
impl ToolSessionManager for IdentifiedSession { fn session_id(&self) -> &str { &self.1 } fn session_file(&self) -> Option<&Path> { None } }
impl SessionManager for IdentifiedSession {
    fn get_entries(&self) -> Vec<SessionEntry> { vec![] }
    fn get_branch(&self) -> Vec<SessionEntry> { vec![] }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
    fn extension_context_actions(&self) -> Option<&dyn ExtensionContextActions> { Some(self.0.as_ref()) }
}
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
        get_system_prompt_fn: Arc::new(String::new), get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default), registered_mcp_servers: vec![], update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default() }
}
#[test]
fn native_registration_replay_and_live_publication_use_consumer_admission() {
    use maho_ext_config_reload::protocol::CONFIG_WATCH_REJECTED;
    let root = tempfile::tempdir().unwrap();
    let agent_dir = std::path::PathBuf::from(maho_core::config::get_agent_dir());
    let events = EventBus::default();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = events.on(CONFIG_WATCH_REJECTED, Arc::new(move |payload| sender.send(payload.clone()).unwrap()));
    let registration = RegisteredConfigWatch { id: "replay".into(), display_name: "replay".into(), targets: vec![ConfigWatchTargetSpec { path: agent_dir.join("auth.json"), kind: ConfigWatchTargetKind::File, filter_globs: vec![] }], validate: Arc::new(|_| ConfigWatchValidation::Ok) };
    events.publish_config_watch("owner", registration.clone());
    let mut api = ExtensionApi::new(LoadedExtension::new("config-reload", root.path().into(), SourceInfo::default()), ExtensionSessionProfile::default(), events.clone(), ExtensionRuntime::default());
    maho_ext_config_reload::ConfigReload.register(&mut api);
    assert_eq!(receiver.try_recv().unwrap()["registrationId"], "replay");
    events.publish_config_watch("owner", RegisteredConfigWatch { id: "live".into(), ..registration });
    assert_eq!(receiver.try_recv().unwrap()["registrationId"], "live");
    assert!(receiver.try_recv().is_err());
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

#[tokio::test]
async fn compacting_change_retries_and_completes_real_session_reload() {
    real_reload_with_options(false, true).await;
}

#[tokio::test]
async fn external_handoff_change_is_rejected_under_its_registration_id() {
    use maho_core::{sdk::{create_agent_session, CreateAgentSessionOptions}, model_runtime::{ModelRuntime, CreateModelRuntimeOptions}};
    use maho_ext_config_reload::protocol::*;
    use maho_ext_host::{ExtensionRunner, loader::{load_extensions, NativeExtensionFactory}};
    let root = tempfile::tempdir().expect("reload fixture operation must succeed");
    let external = tempfile::tempdir().expect("reload fixture operation must succeed");
    let cwd = root.path().to_string_lossy().into_owned();
    let external_path = external.path().join("external.json");
    std::fs::write(&external_path, "{}").expect("external baseline created");
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
    let (event_sender, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let subscriptions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let retained = Arc::clone(&subscriptions);
    let watched_path = external_path.clone();
    let factory: maho_ext_host::runner::RuntimeFactory = Arc::new(move |ctx| {
        let event_sender = event_sender.clone();
        let retained = Arc::clone(&retained);
        let watched = watched_path.clone();
        Box::pin(async move {
            let before_reload_path = watched.clone();
            let registration = RegisteredConfigWatch {
                id: "external-config".into(),
                display_name: "external config".into(),
                targets: vec![ConfigWatchTargetSpec { path: watched.clone(), kind: maho_ext_api::ConfigWatchTargetKind::File, filter_globs: vec![] }],
                validate: Arc::new(move |paths| {
                    if paths.iter().any(|path| path == &before_reload_path) {
                        ConfigWatchValidation::Rejected { errors: vec!["external config rejected".into()] }
                    } else { ConfigWatchValidation::Ok }
                }),
            };
            struct ConfigWatchOwner(RegisteredConfigWatch);
            impl Extension for ConfigWatchOwner {
                fn register(&self, api: &mut ExtensionApi) { api.register_config_watch(self.0.clone()); }
            }
            struct ChangeDuringReload { path: std::path::PathBuf, armed: Arc<std::sync::atomic::AtomicBool> }
            impl Extension for ChangeDuringReload {
                fn register(&self, api: &mut ExtensionApi) {
                    let path = self.path.clone();
                    let armed = Arc::clone(&self.armed);
                    api.on(EventKind::SessionBeforeReload, Arc::new(move |_, _| {
                        let path = path.clone();
                        let armed = Arc::clone(&armed);
                        Box::pin(async move {
                            if armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                                std::fs::write(path, "{\"changed\":true}").expect("handoff fixture changed");
                            }
                            Ok(EventResult::SessionBefore(SessionBeforeEventResult::default()))
                        })
                    }));
                }
            }
            let loaded = load_extensions(vec![
                NativeExtensionFactory { path: "config-reload".into(), source_info: Default::default(), extension: Box::new(maho_ext_config_reload::ConfigReload) },
                NativeExtensionFactory { path: "config-watch-owner".into(), source_info: Default::default(), extension: Box::new(ConfigWatchOwner(registration)) },
                NativeExtensionFactory { path: "change-during-reload".into(), source_info: Default::default(), extension: Box::new(ChangeDuringReload { path: watched, armed: Arc::new(std::sync::atomic::AtomicBool::new(true)) }) },
            ], &ctx.cwd, Default::default());
            assert!(loaded.errors.is_empty());
            let reloaded_sender = event_sender.clone();
            let subscription = loaded.events.on(CONFIG_WATCH_RELOADED, Arc::new(move |value| { let _ = reloaded_sender.send(("reloaded", value.clone())); }));
            retained.lock().expect("reload fixture operation must succeed").push(subscription);
            let rejected_sender = event_sender.clone();
            let subscription = loaded.events.on(CONFIG_WATCH_REJECTED, Arc::new(move |value| { rejected_sender.send(("rejected", value.clone())).expect("event receiver active"); }));
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
    std::fs::rename(staged.path(), root.path().join("keybindings.json")).expect("reload fixture operation must succeed");

    let (first_kind, first) = tokio::time::timeout(std::time::Duration::from_secs(5), observed.recv()).await.expect("reload completion delivered").expect("event channel open");
    assert_eq!(first_kind, "reloaded", "ordinary watcher validation must not satisfy the handoff regression");
    assert_eq!(first["registrationId"], "builtin");
    let (second_kind, rejection) = tokio::time::timeout(std::time::Duration::from_secs(5), observed.recv()).await.expect("handoff rejection delivered").expect("event channel open");
    assert_eq!(second_kind, "rejected");
    assert_eq!(rejection["registrationId"], "external-config");
    assert_eq!(rejection["paths"], serde_json::json!([external_path.clone()]));
    assert_eq!(rejection["errors"], serde_json::json!(["external config rejected"]));
    session.dispose().await;
}

async fn real_reload(veto_once: bool) {
    real_reload_with_options(veto_once, false).await;
}

async fn real_reload_with_options(veto_once: bool, compact_once: bool) {
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
    let (clock_sender, mut clock_armed) = tokio::sync::mpsc::unbounded_channel::<tokio::time::Instant>();
    let clock_now = tokio::time::Instant::now();
    let retry_clock = Arc::new(RetryClock { now: Arc::new(std::sync::Mutex::new(clock_now)), armed: clock_sender, advanced: Arc::new(tokio::sync::Notify::new()) });
    // The injected clock also drives the watcher debounce (default 200ms). Release each debounce
    // deadline as soon as it is armed and hand only the reload rechecks to the test, together with
    // the clock time at which they were armed.
    let (recheck_sender, mut rechecks) = tokio::sync::mpsc::unbounded_channel();
    let driver_clock = Arc::clone(&retry_clock);
    let driver = tokio::spawn(async move {
        while let Some(deadline) = clock_armed.recv().await {
            let armed_at = driver_clock.now();
            if deadline.saturating_duration_since(armed_at) == std::time::Duration::from_millis(200) { driver_clock.advance_to(deadline); }
            else if recheck_sender.send((armed_at, deadline)).is_err() { break; }
        }
    });
    let compacting = Arc::new(std::sync::atomic::AtomicBool::new(compact_once));
    ctx.is_compacting_fn = { let compacting = Arc::clone(&compacting); Arc::new(move || compacting.load(std::sync::atomic::Ordering::SeqCst)) };
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
    let factory_clock = Arc::clone(&retry_clock);
    let factory: maho_ext_host::runner::RuntimeFactory = Arc::new(move |ctx| {
        let sender = sender.clone();
        let retained = Arc::clone(&retained);
        let reject = Arc::clone(&reject);
        let veto_sender = veto_sender.clone();
        let retry_clock = Arc::clone(&factory_clock);
        Box::pin(async move {
            let loaded = load_extensions(vec![
                NativeExtensionFactory { path: "config-reload".into(), source_info: Default::default(), extension: Box::new(maho_ext_config_reload::ConfigReload::with_clock(retry_clock)) },
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
    if compact_once || veto_once {
        let (armed_at, deadline) = tokio::time::timeout(std::time::Duration::from_secs(5), rechecks.recv()).await.expect("scheduled recheck clock armed").expect("clock waiter remains active");
        assert_eq!(deadline - armed_at, if compact_once { std::time::Duration::from_millis(250) } else { std::time::Duration::from_millis(1000) });
        compacting.store(false, std::sync::atomic::Ordering::SeqCst);
        retry_clock.advance_to(deadline);
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), reloaded.recv()).await.expect("reload fixture operation must succeed").expect("reload fixture operation must succeed");
    assert_eq!(result, serde_json::json!({"registrationId":"builtin","paths":[path]}));
    driver.abort();
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

#[tokio::test]
async fn scoped_real_reload_preserves_session_identity_and_provider_scope() {
    use maho_core::{sdk::{create_agent_session, CreateAgentSessionOptions}, model_runtime::{ModelRuntime, CreateModelRuntimeOptions}};
    use maho_ext_host::{ExtensionRunner, loader::{load_extensions, NativeExtensionFactory}};
    use maho_ext_config_reload::protocol::CONFIG_WATCH_RELOADED;
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let model = provider.get_model(Some("faux-1")).unwrap();
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions { models_path: Some(root.path().join("models.json")), auth_path: Some(root.path().join("auth.json")), providers: Some(vec![provider.provider.clone()]), ..Default::default() });
    let session = create_agent_session(CreateAgentSessionOptions { cwd: Some(cwd.clone()), agent_dir: Some(cwd.clone()), model: Some(model), model_runtime: Some(runtime), session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)), settings_manager: Some(maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), false)), auto_title_sessions: Some(false), ..Default::default() }).await.unwrap().session;
    let expected_id = session.session_id();
    let scope = maho_ai::node::provider_scope::ProviderScope::new();
    let expected_scope = scope.clone();
    let (sender, mut reloaded) = tokio::sync::mpsc::unbounded_channel();
    let retained = Arc::new(std::sync::Mutex::new(Vec::new()));
    let keep = Arc::clone(&retained);
    let factory: maho_ext_host::runner::RuntimeFactory = Arc::new(move |ctx| {
        let keep = Arc::clone(&keep);
        let scope = expected_scope.clone();
        let sender = sender.clone();
        let identity = expected_id.clone();
        Box::pin(async move {
            assert_eq!(ctx.session_manager.session_id(), identity);
            assert!(maho_ai::node::provider_scope::active_provider_scope().is_some_and(|active| active.ptr_eq(&scope)));
            let loaded = load_extensions(vec![NativeExtensionFactory { path: "config-reload".into(), source_info: Default::default(), extension: Box::new(maho_ext_config_reload::ConfigReload) }], &ctx.cwd, Default::default());
            let subscription = loaded.events.on(CONFIG_WATCH_RELOADED, Arc::new(move |value| { sender.send(value.clone()).expect("receiver active"); }));
            keep.lock().unwrap().push(subscription);
            Ok(ExtensionRunner::new(loaded.extensions, loaded.runtime, loaded.events, ctx))
        })
    });
    let mut ctx = context(root.path());
    ctx.mode = ExtensionMode::Tui;
    ctx.session_manager = Arc::new(IdentifiedSession(session.extension_context_actions(), session.session_id()));
    let mut runner = maho_ai::node::provider_scope::run_with_provider_scope_async(&scope, factory(ctx)).await.unwrap().unwrap();
    runner.set_runtime_factory(factory);
    session.set_extension_runner(runner).await;
    session.bind_extensions(maho_core::agent_session::ExtensionBindings { mode: Some(ExtensionMode::Tui), ..Default::default() }).await;
    let staged = tempfile::NamedTempFile::new_in(root.path()).unwrap();
    std::fs::write(staged.path(), "{\"fixture\":\"ctrl+x\"}").unwrap();
    let path = root.path().join("keybindings.json");
    std::fs::rename(staged.path(), &path).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), reloaded.recv()).await.unwrap().unwrap();
    assert_eq!(result, serde_json::json!({"registrationId":"builtin","paths":[path]}));
    session.dispose().await;
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
