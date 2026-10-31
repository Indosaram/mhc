//! Port of `model-fallback-command.test.ts` (senpi 903b8336). Settings persistence is an in-memory
//! `retry` object resolved through core `resolve_retry_fallback_settings`, the same resolution the
//! production `SessionExtensionActions` performs over `settings.json`.
use crate::{ModelFallback, settings::is_model_fallback_disabled};
use maho_ext_api::*;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
};

fn model(provider: &str, id: &str) -> Model {
    serde_json::from_value(json!({
        "provider": provider, "id": id, "name": id, "api": "faux",
        "baseUrl": "https://models.example.test/v1", "reasoning": true,
        "thinkingLevelMap": {"xhigh": "xhigh", "max": "max"},
        "input": ["text"], "contextWindow": 1, "maxTokens": 1,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}
    }))
    .unwrap()
}
fn primary() -> Model {
    model("anthropic", "claude-fable-5")
}
fn fallback() -> Model {
    model("ccapi", "kimi-k3")
}
fn kimi_k3() -> Model {
    model("kimi-coding", "k3")
}
fn sdk_fable() -> Model {
    model("anthropic-subscription", "claude-fable-5")
}
fn sdk_opus5() -> Model {
    model("anthropic-subscription", "claude-opus-5")
}
fn opus5() -> Model {
    model("anthropic", "claude-opus-5")
}
fn opus48() -> Model {
    model("anthropic", "claude-opus-4-8")
}

struct Registry {
    all: Vec<Model>,
    available: Vec<Model>,
}
impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> {
        self.all.clone()
    }
    fn get_available(&self) -> Vec<Model> {
        self.available.clone()
    }
    fn find(&self, provider: &str, id: &str) -> Option<Model> {
        self.all
            .iter()
            .find(|model| model.provider == provider && model.id == id)
            .cloned()
    }
    fn has_configured_auth(&self, model: &Model) -> bool {
        self.available.contains(model)
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Default)]
struct Ui {
    notices: Mutex<Vec<String>>,
    choices: Mutex<VecDeque<String>>,
}
impl ExtensionUi for Ui {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { self.choices.lock().unwrap().pop_front() })
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
    fn notify(&self, message: &str, _: NotificationType) {
        self.notices.lock().unwrap().push(message.into());
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
        Box::pin(async { Err("Fallback command tests do not render custom UI".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

/// The persisted `retry` settings object (`settings.json` `retry`).
#[derive(Default)]
struct Actions {
    retry: Mutex<Option<Value>>,
}
impl Actions {
    fn update(&self, change: impl FnOnce(&mut serde_json::Map<String, Value>)) {
        let mut retry = self.retry.lock().unwrap();
        let object = retry
            .get_or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap();
        change(object);
    }
}
impl ExtensionSessionSettings for Actions {
    fn get_retry_fallback_settings(&self) -> RetryFallbackSettings {
        let raw = self.retry.lock().unwrap().clone();
        let resolved =
            maho_core::retry_fallback::settings::resolve_retry_fallback_settings(raw.as_ref());
        RetryFallbackSettings {
            model_fallback: resolved.model_fallback,
            chains: resolved.chains.into_iter().collect(),
            revert_policy: match resolved.revert_policy {
                maho_core::retry_fallback::settings::FallbackRevertPolicy::Never => {
                    FallbackRevertPolicy::Never
                }
                maho_core::retry_fallback::settings::FallbackRevertPolicy::CooldownExpiry => {
                    FallbackRevertPolicy::CooldownExpiry
                }
            },
        }
    }
    fn set_fallback_chain<'a>(
        &'a self,
        key: &'a str,
        entries: &'a [String],
    ) -> ExtensionFuture<'a, ()> {
        self.update(|retry| {
            let chains = retry.entry("fallbackChains").or_insert_with(|| json!({}));
            chains[key] = json!(entries);
        });
        Box::pin(async { Ok(()) })
    }
    fn remove_fallback_chain<'a>(&'a self, key: &'a str) -> ExtensionFuture<'a, ()> {
        self.update(|retry| {
            if let Some(chains) = retry
                .get_mut("fallbackChains")
                .and_then(Value::as_object_mut)
            {
                chains.remove(key);
            }
        });
        Box::pin(async { Ok(()) })
    }
    fn set_model_fallback_enabled(&self, enabled: bool) -> ExtensionFuture<'_, ()> {
        self.update(|retry| {
            retry.insert("modelFallback".into(), json!(enabled));
        });
        Box::pin(async { Ok(()) })
    }
    fn set_fallback_revert_policy(&self, policy: FallbackRevertPolicy) -> ExtensionFuture<'_, ()> {
        self.update(|retry| {
            retry.insert(
                "fallbackRevertPolicy".into(),
                json!(if policy == FallbackRevertPolicy::Never {
                    "never"
                } else {
                    "cooldown-expiry"
                }),
            );
        });
        Box::pin(async { Ok(()) })
    }
    fn reload(&self) -> ExtensionFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn get_fallback_status(&self) -> Option<RetryFallbackStatus> {
        None
    }
}
impl ExtensionContextActions for Actions {
    fn get_model(&self) -> Option<Model> {
        None
    }
    fn get_service_tier(&self) -> Option<ServiceTier> {
        None
    }
    fn get_scoped_models(&self) -> Vec<ScopedModel> {
        Vec::new()
    }
    fn get_agent_dir(&self) -> std::path::PathBuf {
        "/agent".into()
    }
    fn is_idle(&self) -> bool {
        true
    }
    fn is_project_trusted(&self) -> bool {
        true
    }
    fn get_signal(&self) -> Option<AbortSignal> {
        None
    }
    fn abort(&self, _: Option<AbortSource>) {}
    fn has_pending_messages(&self) -> bool {
        false
    }
    fn request_reload(&self) -> ExtensionFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn is_compacting(&self) -> bool {
        false
    }
    fn check_reload_veto(&self) -> ExtensionFuture<'_, ReloadVetoDecision> {
        Box::pin(async {
            Ok(ReloadVetoDecision {
                cancelled: false,
                reason: None,
            })
        })
    }
    fn shutdown(&self) {}
    fn get_context_usage(&self) -> Option<ContextUsage> {
        None
    }
    fn get_compaction_settings(&self) -> CompactionSettings {
        CompactionSettings {
            enabled: true,
            reserve_tokens: 100,
            keep_recent_tokens: 200,
        }
    }
    fn get_prompt_cache_safe_wait_seconds(&self) -> Option<f64> {
        None
    }
    fn get_prompt_cache_goal_backstop_max_seconds(&self) -> f64 {
        0.
    }
    fn get_prompt_cache_keep_alive_settings(&self) -> PromptCacheKeepAliveSettings {
        PromptCacheKeepAliveSettings {
            enabled: false,
            max_requests_per_session: 0,
            max_cost_usd_per_session: 0.,
            margin_seconds: 0.,
        }
    }
    fn get_look_at_settings(&self) -> LookAtSettings {
        LookAtSettings {
            enabled: true,
            models: None,
        }
    }
    fn get_ask_user_settings(&self) -> AskUserSettings {
        AskUserSettings {
            enabled: false,
            timeout_minutes: 0.,
        }
    }
    fn get_image_settings(&self) -> ImageSettings {
        ImageSettings {
            auto_resize: true,
            block_images: false,
        }
    }
    fn session_settings(&self) -> &dyn ExtensionSessionSettings {
        self
    }
    fn compact(&self, _: CompactOptions) {}
    fn prepare_provider_request(
        &self,
        messages: Vec<AgentMessage>,
    ) -> ExtensionFuture<'_, ProviderRequestPreparation> {
        Box::pin(async move {
            Ok(ProviderRequestPreparation {
                messages,
                transform_payload: Arc::new(|payload| Box::pin(async move { Ok(payload) })),
                transform_headers: Arc::new(|headers| Box::pin(async move { Ok(headers) })),
            })
        })
    }
    fn begin_compaction(&self, _: BeginCompactionOptions) -> Option<AbortSignal> {
        None
    }
    fn update_compaction(&self, _: UpdateCompactionOptions) {}
    fn end_compaction(&self, _: EndCompactionOptions) {}
    fn get_message_revision(&self) -> u64 {
        0
    }
    fn apply_compaction(
        &self,
        _: CompactionResult,
        _: ApplyCompactionOptions,
    ) -> ExtensionFuture<'_, ApplyCompactionResult> {
        Box::pin(async { Ok(ApplyCompactionResult::Rejected) })
    }
    fn get_system_prompt(&self) -> String {
        String::new()
    }
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        Default::default()
    }
    fn get_loaded_hook_sources(&self) -> LoadedHookSources {
        LoadedHookSources {
            cwd: "/repo".into(),
            agent_dir: "/agent".into(),
            global_hooks_path: "/g".into(),
            project_hooks_path: "/p".into(),
            global_settings_hooks: None,
            project_settings_hooks: None,
            global_hook_source_paths: Vec::new(),
            project_hook_source_paths: Vec::new(),
            pre_session_hook_source_paths: Vec::new(),
            runtime_hook_source_paths: Vec::new(),
        }
    }
    fn kernel_tools(&self) -> Option<&dyn ExtensionKernelTools> {
        None
    }
}
struct Session(Arc<Actions>);
impl ToolSessionManager for Session {
    fn session_id(&self) -> &str {
        "s"
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
    fn extension_context_actions(&self) -> Option<&dyn ExtensionContextActions> {
        Some(self.0.as_ref())
    }
}

struct Fixture {
    ui: Arc<Ui>,
    actions: Arc<Actions>,
    ctx: ExtensionContext,
}
fn fixture(choices: &[&str], all: Vec<Model>, available: Vec<Model>) -> Fixture {
    let ui = Arc::new(Ui {
        notices: Mutex::default(),
        choices: Mutex::new(choices.iter().map(|choice| (*choice).to_owned()).collect()),
    });
    let actions = Arc::new(Actions::default());
    let has_ui = !choices.is_empty();
    let ctx = ExtensionContext {
        ui: ui.clone(),
        mode: if has_ui {
            ExtensionMode::Tui
        } else {
            ExtensionMode::Print
        },
        has_ui,
        cwd: "/repo".into(),
        agent_dir: "/agent".into(),
        session_manager: Arc::new(Session(actions.clone())),
        model_registry: Arc::new(Registry { all, available }),
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
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    };
    Fixture { ui, actions, ctx }
}
fn command() -> RegisteredCommand {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("model-fallback", "/repo".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    ModelFallback {
        is_using_oauth: Arc::new(|_| false),
    }
    .register(&mut api);
    api.registered
        .commands
        .iter()
        .find(|command| command.name == "fallback")
        .cloned()
        .expect("fallback command")
}
fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    // The handlers under test never wait on I/O; a pending poll means a handler started waiting,
    // which must fail the test instead of spinning forever.
    const MAX_POLLS: usize = 1_000;
    for _ in 0..MAX_POLLS {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
    }
    panic!("future still pending after {MAX_POLLS} polls");
}
fn run(fixture: &Fixture, args: &str) -> String {
    block_on((command().handler)(args, &fixture.ctx)).unwrap();
    fixture.ui.notices.lock().unwrap().join("\n")
}

#[test]
fn registers_fallback_with_its_quick_set_hint() {
    let command = command();
    assert_eq!(
        command.argument_hint.as_deref(),
        Some("[target [fallback1 fallback2 ...]]")
    );
    assert!(command.description.unwrap_or_default().contains("fallback"));
}

#[test]
fn reports_empty_state_when_nothing_is_configured_and_no_shipped_family_is_served() {
    let fixture = fixture(
        &["Show chains & live state"],
        vec![kimi_k3()],
        vec![kimi_k3()],
    );
    assert!(run(&fixture, "").contains("No fallback chains configured"));
}

#[test]
fn renders_the_shipped_fable_chain_when_the_user_configured_nothing() {
    let catalog_only = model("github-copilot", "claude-fable-5");
    let fixture = fixture(
        &["Show chains & live state"],
        vec![sdk_fable(), sdk_opus5(), kimi_k3(), catalog_only],
        vec![sdk_fable(), sdk_opus5(), kimi_k3()],
    );
    let rendered = run(&fixture, "");
    assert!(
        rendered.contains("anthropic-subscription/claude-fable-5 ->"),
        "{rendered}"
    );
    assert!(rendered.contains("claude-opus-5"), "{rendered}");
}

#[test]
fn lists_a_configured_chain_for_the_provider_the_user_actually_pinned() {
    let models = vec![sdk_fable(), sdk_opus5(), kimi_k3()];
    let fixture = fixture(&["Show chains & live state"], models.clone(), models);
    *fixture.actions.retry.lock().unwrap() = Some(
        json!({"fallbackChains": {"anthropic-subscription/claude-fable-5": ["kimi-coding/k3:max"]}}),
    );
    let rendered = run(&fixture, "");
    assert!(
        rendered.contains("anthropic-subscription/claude-fable-5 ->"),
        "{rendered}"
    );
    assert!(rendered.contains("kimi-coding/k3:max"), "{rendered}");
}

#[test]
fn quick_set_validates_and_persists_a_chain_visible_after_a_session_side_reload() {
    let models = vec![primary(), fallback()];
    let fixture = fixture(&[], models.clone(), models);
    run(&fixture, "anthropic/claude-fable-5 ccapi/kimi-k3:max");
    let chains = fixture.actions.get_retry_fallback_settings().chains;
    assert_eq!(
        chains.get("anthropic/claude-fable-5"),
        Some(&vec!["ccapi/kimi-k3:max".to_owned()])
    );
    assert!(
        fixture
            .ui
            .notices
            .lock()
            .unwrap()
            .contains(&"Fallback chain saved for anthropic/claude-fable-5.".to_owned())
    );
}

#[test]
fn rejects_an_invalid_quick_set_without_writing_settings() {
    let models = vec![primary(), fallback()];
    let fixture = fixture(&[], models.clone(), models);
    let notices = run(&fixture, "bogus/model nope");
    assert!(fixture.actions.retry.lock().unwrap().is_none());
    assert!(
        notices.contains("not a valid or known model selector"),
        "{notices}"
    );
}

#[test]
fn maps_the_cli_flag_and_environment_escape_hatch_to_a_disabled_run_override() {
    assert!(is_model_fallback_disabled(
        Some(&FlagValue::Boolean(true)),
        None
    ));
    assert!(is_model_fallback_disabled(
        Some(&FlagValue::Boolean(false)),
        Some("1")
    ));
    assert!(!is_model_fallback_disabled(
        Some(&FlagValue::Boolean(false)),
        None
    ));
}

#[test]
fn filters_a_configured_chain_by_live_availability() {
    let cases = [
        (
            "all fallback models are available",
            vec![primary(), kimi_k3(), opus5(), opus48()],
            vec!["kimi-coding/k3:max", "anthropic/claude-opus-5:xhigh"],
            vec![],
        ),
        (
            "Kimi K3 is unavailable",
            vec![primary(), opus5(), opus48()],
            vec![
                "anthropic/claude-opus-5:xhigh",
                "anthropic/claude-opus-4-8:xhigh",
            ],
            vec!["kimi-coding/k3:max"],
        ),
    ];
    for (name, models, must_list, must_omit) in cases {
        let fixture = fixture(&["Show chains & live state"], models.clone(), models);
        *fixture.actions.retry.lock().unwrap() = Some(
            json!({"fallbackChains": {"anthropic/claude-fable-5": ["kimi-coding/k3:max", "anthropic/claude-opus-5:xhigh", "anthropic/claude-opus-4-8:xhigh"]}}),
        );
        let rendered = run(&fixture, "");
        for entry in must_list {
            assert!(
                rendered.contains(entry),
                "{name}: missing {entry} in {rendered}"
            );
        }
        for entry in must_omit {
            assert!(
                !rendered.contains(entry),
                "{name}: unexpected {entry} in {rendered}"
            );
        }
    }
}

#[test]
fn handles_a_headless_menu_invocation_cleanly() {
    let models = vec![primary(), fallback()];
    let fixture = fixture(&[], models.clone(), models);
    run(&fixture, "");
    assert!(fixture.ui.notices.lock().unwrap().contains(
        &"Fallback menu requires interactive UI. Use /fallback <target> <fallback...>.".to_owned()
    ));
}
