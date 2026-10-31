use maho_ext_api::*;
use maho_ext_pi_websearch::index::*;
use maho_ext_pi_websearch::websearch::types::ConfigLoadResult;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
struct Session;
impl ToolSessionManager for Session {
    fn session_id(&self) -> &str {
        "fixture"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for Session {
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
struct Registry;
impl ModelRegistry for Registry {
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
struct Ui {
    state: Arc<Mutex<ConfigLoadResult>>,
    observed: Mutex<Vec<bool>>,
    notified: Mutex<Vec<(String, NotificationType)>>,
}
impl ExtensionUi for Ui {
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
        self.notified
            .lock()
            .expect("notifications")
            .push((message.into(), kind));
    }
    fn set_status(&self, _: &str, _: Option<&str>) {
        self.observed.lock().expect("observations").push(matches!(
            *self.state.lock().expect("state"),
            ConfigLoadResult::Success { .. }
        ));
    }
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
        Box::pin(async { Err("headless fixture".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}
fn context(ui: Arc<Ui>, cwd: &Path, provider: &str, api: &str) -> ExtensionContext {
    ExtensionContext {
        ui,
        mode: ExtensionMode::Print,
        has_ui: true,
        cwd: cwd.into(),
        agent_dir: cwd.join("agent"),
        session_manager: Arc::new(Session),
        model_registry: Arc::new(Registry),
        model: Some(serde_json::from_value(serde_json::json!({
            "id":"test", "name":"Test", "api":api, "provider":provider, "baseUrl":"https://api.openai.com/v1",
            "reasoning":false, "input":["text"], "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}, "contextWindow":1000, "maxTokens":100
        })).expect("model")),
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
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}
#[tokio::test]
async fn session_publishes_loaded_config_before_ui_callbacks() {
    let temp = tempfile::tempdir().expect("temp");
    std::fs::create_dir(temp.path().join(".pi")).expect("config directory");
    std::fs::write(
        temp.path().join(".pi/websearch.json"),
        r#"{"providers":[{"provider":"duckduckgo-html"}],"auto":false}"#,
    )
    .expect("config");
    let mut api = ExtensionApi::new(
        LoadedExtension::new("pi-websearch", temp.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    let state = register_search_lifecycle(&mut api);
    let ui = Arc::new(Ui {
        state: Arc::clone(&state),
        observed: Mutex::new(Vec::new()),
        notified: Mutex::new(Vec::new()),
    });
    let ctx = context(ui.clone(), temp.path(), "openai", "openai-responses");
    let mut event = ExtensionEvent::SessionStart(SessionStartEvent {
        sizes_only: false,
        reason: SessionReason::New,
        initial_model_provenance: None,
        previous_session_file: None,
        attended: None,
    });
    api.registered.handlers[&EventKind::SessionStart][0](&mut event, &ctx)
        .await
        .expect("start");
    assert_eq!(*ui.observed.lock().expect("observations"), vec![true]);
}
#[test]
fn bypass_matches_only_native_provider_extensions() {
    assert!(is_provider_native_bypass(Some("openai")));
    assert!(is_provider_native_bypass(Some("anthropic")));
    for provider in [None, Some("openrouter"), Some("custom"), Some("OpenAI")] {
        assert!(!is_provider_native_bypass(provider));
    }
}
#[test]
fn registers_lifecycle_and_status_command() {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("pi-websearch", "/fixture".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    register_search_lifecycle(&mut api);
    assert_eq!(api.registered.handlers[&EventKind::SessionStart].len(), 1);
    assert_eq!(
        api.registered.handlers[&EventKind::SessionShutdown].len(),
        1
    );
    assert!(
        api.registered
            .commands
            .iter()
            .any(|command| command.name == "websearch")
    );
}
fn session_api(cwd: &Path) -> ExtensionApi {
    ExtensionApi::new(
        LoadedExtension::new("pi-websearch", cwd.into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    )
}
async fn start_session(api: &ExtensionApi, ctx: &ExtensionContext) {
    let mut event = ExtensionEvent::SessionStart(SessionStartEvent {
        sizes_only: false,
        reason: SessionReason::New,
        initial_model_provenance: None,
        previous_session_file: None,
        attended: None,
    });
    api.registered.handlers[&EventKind::SessionStart][0](&mut event, ctx)
        .await
        .expect("start");
}
#[tokio::test]
async fn custom_openai_compatible_provider_without_config_reports_missing_config() {
    let temp = tempfile::tempdir().expect("temp");
    let mut api = session_api(temp.path());
    let state = register_search_lifecycle(&mut api);
    let ui = Arc::new(Ui {
        state: Arc::clone(&state),
        observed: Mutex::new(Vec::new()),
        notified: Mutex::new(Vec::new()),
    });
    let ctx = context(ui.clone(), temp.path(), "apitopia", "openai-responses");
    start_session(&api, &ctx).await;
    let ConfigLoadResult::Failure { message, .. } = &*state.lock().expect("state") else {
        panic!("expected missing config")
    };
    let notified = ui.notified.lock().expect("notifications");
    assert_eq!(notified.len(), 1);
    assert_eq!(&notified[0].0, message);
    assert!(matches!(notified[0].1, NotificationType::Error));
}
#[tokio::test]
async fn status_command_renders_provider_id_labels_and_collapses_native_ids() {
    let temp = tempfile::tempdir().expect("temp");
    std::fs::create_dir(temp.path().join(".pi")).expect("config directory");
    std::fs::write(
        temp.path().join(".pi/websearch.json"),
        r#"{"strategy":"priority","fallback":true,"auto":true,"providers":[
            {"id":"primary","provider":"exa","apiKey":"test-key"},
            {"id":"backup","provider":"duckduckgo-html"},
            {"id":"native-openai-abc123","provider":"openai","apiKey":"test-key"}]}"#,
    )
    .expect("config");
    let mut api = session_api(temp.path());
    let state = register_search_lifecycle(&mut api);
    let ui = Arc::new(Ui {
        state: Arc::clone(&state),
        observed: Mutex::new(Vec::new()),
        notified: Mutex::new(Vec::new()),
    });
    let ctx = context(ui.clone(), temp.path(), "local", "openai-completions");
    start_session(&api, &ctx).await;
    assert!(matches!(
        &*state.lock().expect("state"),
        ConfigLoadResult::Success { .. }
    ));
    let command = api
        .registered
        .commands
        .iter()
        .find(|command| command.name == "websearch")
        .expect("status command");
    (command.handler)("status", &ctx).await.expect("status");
    let notified = ui.notified.lock().expect("notifications");
    assert_eq!(notified.len(), 1);
    assert!(matches!(notified[0].1, NotificationType::Info));
    assert!(
        notified[0]
            .0
            .contains("providers=exa/primary, duckduckgo-html/backup, openai/native"),
        "{}",
        notified[0].0
    );
    assert!(!notified[0].0.contains("primary/exa"));
}
