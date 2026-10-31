//! senpi `test/suite/recommended-models-extension.test.ts`: session_start / model_select drive the
//! session action binding (`setModel` / `setSessionModel`, thinking level, notices).
use maho_ai::types::ThinkingLevel;
use maho_ext_api::*;
use maho_ext_recommended_models::RecommendedModels;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

fn model(id: &str, provider: &str) -> Model {
    serde_json::from_value(serde_json::json!({"id":id,"name":id,"provider":provider,"api":"faux","baseUrl":"","reasoning":true,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096,"maxTokens":1024})).expect("model")
}

#[derive(Default)]
struct Session {
    /// (model id, provider, persisted as default)
    switched: Mutex<Vec<(String, String, bool)>>,
    /// (level, persisted as default)
    levels: Mutex<Vec<(ThinkingLevel, bool)>>,
}
impl Session {
    fn record(&self, model: Model, persist: bool) -> ExtensionFuture<'_, bool> {
        self.switched
            .lock()
            .expect("switched")
            .push((model.id, model.provider, persist));
        Box::pin(async { Ok(true) })
    }
}
impl ExtensionSessionActions for Session {
    fn set_session_name(&self, _: &str) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> {
        Err("unused".into())
    }
    fn set_label(&self, _: &str, _: Option<&str>) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn execute_tool<'a>(
        &'a self,
        _: &'a str,
        _: JsonValue,
        _: ExecuteToolOptions,
    ) -> ExecuteToolFuture<'a> {
        panic!("unused")
    }
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        Ok(Vec::new())
    }
    fn set_active_tools(&self, _: Vec<String>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn refresh_tools(&self) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_removed_tool_hint(&self, _: &str, _: &str) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_lazy_tool_activator(&self, _: LazyToolActivator) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure> {
        Ok(Vec::new())
    }
    fn set_model(&self, model: Model) -> ExtensionFuture<'_, bool> {
        self.record(model, true)
    }
    fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure> {
        Err("unused".into())
    }
    fn set_thinking_level(&self, level: ThinkingLevel) -> Result<(), ExtensionFailure> {
        self.levels.lock().expect("levels").push((level, true));
        Ok(())
    }
    fn set_session_model(&self, model: Model) -> ExtensionFuture<'_, bool> {
        self.record(model, false)
    }
    fn set_session_thinking_level(&self, level: ThinkingLevel) -> Result<(), ExtensionFailure> {
        self.levels.lock().expect("levels").push((level, false));
        Ok(())
    }
    fn set_session_fast_mode(&self, _: bool) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn exec<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: &'a Path,
        _: ExecOptions,
    ) -> ExtensionFuture<'a, ExecResult> {
        panic!("unused")
    }
}

struct Registry(Vec<Model>);
impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> {
        self.0.clone()
    }
    fn get_available(&self) -> Vec<Model> {
        self.0.clone()
    }
    fn find(&self, provider: &str, id: &str) -> Option<Model> {
        self.0
            .iter()
            .find(|model| model.provider == provider && model.id == id)
            .cloned()
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        true
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

struct Ledger;
impl ToolSessionManager for Ledger {
    fn session_id(&self) -> &str {
        "s"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for Ledger {
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

#[derive(Default)]
struct Ui(Mutex<Vec<(String, NotificationType)>>);
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
        self.0.lock().expect("notices").push((message.into(), kind));
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
        Box::pin(async { Err("UI unavailable".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

struct Harness {
    api: ExtensionApi,
    ctx: ExtensionContext,
    session: Arc<Session>,
    ui: Arc<Ui>,
    _dir: tempfile::TempDir,
}
impl Harness {
    fn new(active: Model, available: Vec<Model>, settings: JsonValue, mode: ExtensionMode) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        std::fs::write(agent_dir.join("settings.json"), settings.to_string()).expect("settings");
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let runtime = ExtensionRuntime::default();
        let session = Arc::new(Session::default());
        runtime.bind_session_actions(session.clone());
        let mut api = ExtensionApi::new(
            LoadedExtension::new("recommended-models", cwd.clone(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            runtime,
        );
        RecommendedModels.register(&mut api);
        let ui = Arc::new(Ui::default());
        let ctx = ExtensionContext {
            ui: ui.clone(),
            mode,
            has_ui: true,
            cwd,
            agent_dir,
            session_manager: Arc::new(Ledger),
            model_registry: Arc::new(Registry(available)),
            model: Some(active),
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
            is_project_trusted_fn: Arc::new(|| false),
            is_compacting_fn: Arc::new(|| false),
            get_system_prompt_fn: Arc::new(String::new),
            get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
            registered_mcp_servers: Vec::new(),
            update_tool_hook_status: None,
            idle_coordinator: None,
            logger: None,
            defer_macrotask: None,
            compaction_signal: Default::default(),
        };
        Self {
            api,
            ctx,
            session,
            ui,
            _dir: dir,
        }
    }
    async fn emit(&self, mut event: ExtensionEvent) {
        let handler = self.api.registered.handlers[&event.kind()][0].clone();
        handler(&mut event, &self.ctx).await.expect("handler");
    }
    async fn start(&self, provenance: &str) {
        self.emit(ExtensionEvent::SessionStart(SessionStartEvent {
            reason: SessionReason::Startup,
            initial_model_provenance: Some(provenance.into()),
            previous_session_file: None,
            attended: None,
        }))
        .await;
    }
    async fn select(&self, model: Model, source: ModelSelectSource) {
        self.emit(ExtensionEvent::ModelSelect(ModelSelectEvent {
            model,
            previous_model: None,
            source,
            system_prompt: String::new(),
            system_prompt_options: BuildSystemPromptOptions::default(),
        }))
        .await;
    }
    fn switched(&self) -> Vec<(String, String, bool)> {
        self.session.switched.lock().expect("switched").clone()
    }
    fn levels(&self) -> Vec<(ThinkingLevel, bool)> {
        self.session.levels.lock().expect("levels").clone()
    }
    fn notices(&self) -> Vec<(String, NotificationType)> {
        self.ui.0.lock().expect("notices").clone()
    }
}

fn harness(active: Model, available: Vec<Model>) -> Harness {
    Harness::new(active, available, serde_json::json!({}), ExtensionMode::Tui)
}

#[tokio::test]
async fn tui_switch_persists_the_subscription_lane_at_its_level() {
    let h = harness(
        model("off-list", "faux"),
        vec![
            model("claude-opus-5-5", "anthropic"),
            model("claude-opus-5-5", "anthropic-subscription"),
        ],
    );
    h.start("provider-default").await;
    assert_eq!(
        h.switched(),
        [(
            "claude-opus-5-5".into(),
            "anthropic-subscription".into(),
            true
        )]
    );
    assert_eq!(h.levels(), [(ThinkingLevel::Medium, true)]);
    assert_eq!(
        h.notices(),
        [(
            "Switched to recommended model 'claude-opus-5-5'.".into(),
            NotificationType::Info
        )]
    );
}

#[tokio::test]
async fn non_tui_switch_stays_in_the_session() {
    let h = Harness::new(
        model("off-list", "faux"),
        vec![model("gpt-6-astra", "chatgpt-subscription")],
        serde_json::json!({}),
        ExtensionMode::Rpc,
    );
    h.start("first-available").await;
    assert_eq!(
        h.switched(),
        [("gpt-6-astra".into(), "chatgpt-subscription".into(), false)]
    );
    assert_eq!(h.levels(), [(ThinkingLevel::Xhigh, false)]);
}

#[tokio::test]
async fn gateway_claude_is_skipped_for_kimi_k3_1_pro_at_max() {
    let h = harness(
        model("off-list", "faux"),
        vec![
            model("anthropic/claude-opus-5-5", "opengateway"),
            model("anthropic/claude-fable-5-1", "openrouter"),
            model("k3", "kimi-coding"),
            model("k3.1-pro", "kimi-coding"),
        ],
    );
    h.start("first-available").await;
    assert_eq!(
        h.switched(),
        [("k3.1-pro".into(), "kimi-coding".into(), true)]
    );
    assert_eq!(h.levels(), [(ThinkingLevel::Max, true)]);
}

#[tokio::test]
async fn explicit_provenance_already_recommended_and_disabled_runs_never_switch() {
    for provenance in ["settings", "cli", "scoped"] {
        let h = harness(
            model("off-list", "faux"),
            vec![model("k3.1-pro", "kimi-coding")],
        );
        h.start(provenance).await;
        assert!(h.switched().is_empty() && h.notices().is_empty());
    }
    for active in ["gpt-6.1-sol-fast", "kimi-k3.1-pro", "k3.1-pro"] {
        let h = harness(
            model(active, "faux"),
            vec![model("k3.1-pro", "kimi-coding")],
        );
        h.start("first-available").await;
        assert!(h.switched().is_empty() && h.notices().is_empty());
    }
    let h = Harness::new(
        model("off-list", "faux"),
        vec![model("k3.1-pro", "kimi-coding")],
        serde_json::json!({"warnings":{"offRecommendedModel":true}}),
        ExtensionMode::Tui,
    );
    h.start("provider-default").await;
    assert!(h.switched().is_empty() && h.notices().is_empty());
    let h = harness(
        model("off-list", "faux"),
        vec![model("k3.1-pro", "kimi-coding")],
    );
    h.api
        .runtime
        .set_flag("no-recommended-models", FlagValue::Boolean(true));
    h.start("provider-default").await;
    assert!(h.switched().is_empty() && h.notices().is_empty());
}

#[tokio::test]
async fn override_priority_and_unranked_ids_switch_on_any_provider() {
    let h = Harness::new(
        model("off-list", "faux"),
        vec![
            model("k3.1-pro", "kimi-coding"),
            model("glm-5.3", "zai-coding-plan"),
        ],
        serde_json::json!({"recommendedModels":["glm-5.3"]}),
        ExtensionMode::Tui,
    );
    h.start("provider-default").await;
    assert_eq!(
        h.switched(),
        [("glm-5.3".into(), "zai-coding-plan".into(), true)]
    );
    assert_eq!(h.levels(), [(ThinkingLevel::Max, true)]);
    let h = Harness::new(
        model("off-list", "faux"),
        vec![model("custom-model", "venice")],
        serde_json::json!({"recommendedModels":["custom-model"]}),
        ExtensionMode::Tui,
    );
    h.start("provider-default").await;
    assert_eq!(
        h.switched(),
        [("custom-model".into(), "venice".into(), true)]
    );
    assert_eq!(h.levels(), [(ThinkingLevel::Medium, true)]);
}

#[tokio::test]
async fn no_available_recommendation_warns_once() {
    let h = harness(model("off-list", "faux"), Vec::new());
    h.start("first-available").await;
    h.select(
        model("another-off-list", "faux"),
        ModelSelectSource::Fallback,
    )
    .await;
    assert!(h.switched().is_empty());
    assert_eq!(
        h.notices(),
        [(
            "Non-recommended model 'off-list': odd behavior is the default state; a working session is the anomaly.".into(),
            NotificationType::Warning
        )]
    );
}
