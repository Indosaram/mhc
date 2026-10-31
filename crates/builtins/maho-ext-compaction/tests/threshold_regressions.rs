//! senpi test/compaction.test.ts "builtin compaction extension threshold regressions": the registered
//! handlers driven with a scripted summary stream (upstream mocks `complete`/`stream`; here the
//! model registry's `stream` serves the summary request).
use maho_ai::types::{AssistantMessageEvent, ContentBlock, DoneReason, StopReason};
use maho_ai::utils::event_stream::{
    AssistantMessageEventStream, create_assistant_message_event_stream,
};
use maho_ext_api::*;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn anthropic_model(id: &str, context_window: u64) -> Model {
    serde_json::from_value(serde_json::json!({"id":id,"name":id,"api":"anthropic-messages","provider":"anthropic","baseUrl":"https://api.anthropic.com","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":context_window,"maxTokens":8192})).expect("anthropic model")
}

/// senpi `DEFAULT_COMPACTION_SETTINGS`.
fn default_settings() -> ResolvedCompactionSettings {
    ResolvedCompactionSettings {
        enabled: true,
        reserve_tokens: 16384,
        keep_recent_tokens: 20000,
        speculative_enabled: true,
        speculative_fraction: 0.75,
        speculative_cooldown_ms: 30000.,
        restoration_enabled: true,
        restoration_max_items: 10.,
        restoration_max_tokens_per_item: 5000.,
        restoration_max_total_tokens: 50_000.,
        restoration_context_ratio: 0.15,
        idle_compaction_enabled: true,
        grace_band_enabled: true,
        tool_admission_enabled: true,
        reminder_enabled: true,
        reserve_scaling_enabled: true,
        speculative_lead_tokens: None,
        summarization_max_duration_ms: None,
    }
}

/// The four-entry branch upstream builds with createMessageEntry: two user requests, two answers.
fn branch() -> Vec<SessionEntry> {
    let assistant = |text: &str, input: u64| serde_json::json!({"role":"assistant","content":[{"type":"text","text":text}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":input,"output":500,"cacheRead":0,"cacheWrite":0,"totalTokens":input+500,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":0});
    let user = |text: &str| serde_json::json!({"role":"user","content":text,"timestamp":0});
    [
        user("first request"),
        assistant("first answer", 4000),
        user("second request"),
        assistant("second answer", 5000),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, message)| SessionEntry {
        id: format!("e{index}"),
        parent_id: index.checked_sub(1).map(|parent| format!("e{parent}")),
        timestamp: "2026-01-01T00:00:00.000Z".into(),
        kind: "message".into(),
        data: serde_json::json!({ "message": message }),
    })
    .collect()
}

enum Reply {
    Summary(&'static str),
    /// Holds the stream open until the test releases it or the request is cancelled.
    Hold(tokio::sync::oneshot::Sender<()>),
}

/// Records what the summary requests carried and answers them from a script (senpi `completeMock`).
struct Registry {
    order: Arc<Mutex<Vec<String>>>,
    replies: Mutex<VecDeque<Reply>>,
    held: Mutex<Option<(AssistantMessageEventStream, Model)>>,
    tool_names: Mutex<Vec<Vec<String>>>,
}

impl Registry {
    fn new(order: Arc<Mutex<Vec<String>>>, replies: Vec<Reply>) -> Self {
        Self {
            order,
            replies: Mutex::new(replies.into()),
            held: Mutex::new(None),
            tool_names: Mutex::new(Vec::new()),
        }
    }

    /// senpi `releaseStale`: the held summary resolves unless its request was already cancelled.
    fn release_held(&self, text: &str) {
        if let Some((stream, model)) = lock(&self.held).take() {
            stream.push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: summary_message(&model, text),
            });
        }
    }
}

fn summary_message(model: &Model, text: &str) -> AssistantMessage {
    let mut message = maho_ai::utils::lazy::setup_error_message(model, "");
    message.stop_reason = StopReason::Stop;
    message.error_message = None;
    message.content = vec![ContentBlock::text(text)];
    message
}

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
        true
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(Some("test-key".into())) })
    }
    fn get_api_key_and_headers<'a>(
        &'a self,
        _: &'a Model,
    ) -> ExtensionFuture<'a, ResolvedRequestAuth> {
        lock(&self.order).push("auth-start".into());
        Box::pin(async {
            Ok(ResolvedRequestAuth {
                ambient: false,
                auth: maho_ai::models::ProviderAuthResult {
                    api_key: Some("test-key".into()),
                    headers: None,
                    base_url: None,
                },
                extra_body: None,
                upstream_model_id: None,
                service_tier: None,
                env: None,
            })
        })
    }
    fn stream(
        &self,
        model: &Model,
        context: &maho_ai::types::Context,
        _: Option<maho_ai::types::StreamOptions>,
    ) -> Result<AssistantMessageEventStream, ExtensionFailure> {
        lock(&self.tool_names).push(
            context
                .tools
                .iter()
                .flatten()
                .map(|tool| tool.name.clone())
                .collect(),
        );
        let stream = create_assistant_message_event_stream();
        match lock(&self.replies).pop_front() {
            Some(Reply::Hold(started)) => {
                *lock(&self.held) = Some((stream.clone(), model.clone()));
                let _ = started.send(());
            }
            reply => {
                let text = match reply {
                    Some(Reply::Summary(text)) => text,
                    _ => "<summary>locked summary</summary>",
                };
                stream.push(AssistantMessageEvent::Done {
                    reason: DoneReason::Stop,
                    message: summary_message(model, text),
                });
            }
        }
        Ok(stream)
    }
}

struct Actions {
    settings: ResolvedCompactionSettings,
    usage: Mutex<(u64, u64)>,
    begin_signal: bool,
    order: Arc<Mutex<Vec<String>>>,
    applied: Mutex<Vec<String>>,
    compacts: AtomicUsize,
}

impl Actions {
    fn new(
        settings: ResolvedCompactionSettings,
        tokens: u64,
        context_window: u64,
        order: Arc<Mutex<Vec<String>>>,
    ) -> Self {
        Self {
            settings,
            usage: Mutex::new((tokens, context_window)),
            begin_signal: false,
            order,
            applied: Mutex::new(Vec::new()),
            compacts: AtomicUsize::new(0),
        }
    }
}

impl ExtensionSessionSettings for Actions {
    fn get_retry_fallback_settings(&self) -> RetryFallbackSettings {
        RetryFallbackSettings {
            model_fallback: false,
            chains: Default::default(),
            revert_policy: FallbackRevertPolicy::Never,
        }
    }
    fn set_fallback_chain<'a>(&'a self, _: &'a str, _: &'a [String]) -> ExtensionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn remove_fallback_chain<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn set_model_fallback_enabled(&self, _: bool) -> ExtensionFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn set_fallback_revert_policy(&self, _: FallbackRevertPolicy) -> ExtensionFuture<'_, ()> {
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
        "/tmp/agent".into()
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
        let (tokens, context_window) = *lock(&self.usage);
        Some(ContextUsage {
            tokens: Some(tokens),
            context_window,
            percent: Some(tokens as f64 / context_window as f64 * 100.),
        })
    }
    fn get_compaction_settings(&self) -> CompactionSettings {
        CompactionSettings {
            enabled: self.settings.enabled,
            reserve_tokens: self.settings.reserve_tokens,
            keep_recent_tokens: self.settings.keep_recent_tokens,
        }
    }
    fn get_resolved_compaction_settings(&self) -> Option<ResolvedCompactionSettings> {
        Some(self.settings.clone())
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
    fn compact(&self, _: CompactOptions) {
        self.compacts.fetch_add(1, Ordering::SeqCst);
    }
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
    fn begin_compaction(&self, options: BeginCompactionOptions) -> Option<AbortSignal> {
        if !self.begin_signal {
            return None;
        }
        let reason = match options.reason {
            CompactionReason::Extension => "extension",
            _ => "other",
        };
        lock(&self.order).push(format!("begin-{reason}"));
        Some(AbortSignal::default())
    }
    fn update_compaction(&self, _: UpdateCompactionOptions) {}
    fn end_compaction(&self, _: EndCompactionOptions) {
        if self.begin_signal {
            lock(&self.order).push("end-called".into());
        }
    }
    fn get_message_revision(&self) -> u64 {
        0
    }
    fn apply_compaction(
        &self,
        compaction: CompactionResult,
        _: ApplyCompactionOptions,
    ) -> ExtensionFuture<'_, ApplyCompactionResult> {
        lock(&self.order).push("apply-called".into());
        lock(&self.applied).push(compaction.summary);
        Box::pin(async { Ok(ApplyCompactionResult::Applied) })
    }
    fn get_system_prompt(&self) -> String {
        "system".into()
    }
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        Default::default()
    }
    fn get_loaded_hook_sources(&self) -> LoadedHookSources {
        LoadedHookSources {
            cwd: "/tmp".into(),
            agent_dir: "/tmp/agent".into(),
            global_hooks_path: "/tmp/global".into(),
            project_hooks_path: "/tmp/project".into(),
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

struct Session {
    actions: Arc<Actions>,
    branch: Vec<SessionEntry>,
}
impl ToolSessionManager for Session {
    fn session_id(&self) -> &str {
        "threshold-regressions"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for Session {
    fn get_entries(&self) -> Vec<SessionEntry> {
        self.branch.clone()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        self.branch.clone()
    }
    fn get_leaf_id(&self) -> Option<String> {
        self.branch.last().map(|entry| entry.id.clone())
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
    fn extension_context_actions(&self) -> Option<&dyn ExtensionContextActions> {
        Some(self.actions.as_ref())
    }
}

/// `pi.getActiveTools()` / `pi.getAllTools()` of upstream's captured ExtensionAPI.
struct Tools {
    active: Vec<String>,
    all: Vec<ToolInfo>,
}
impl ExtensionActions for Tools {
    fn send_message(
        &self,
        _: CustomMessage,
        _: SendMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn send_user_message(
        &self,
        _: UserMessageContent,
        _: SendUserMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(self.all.clone())
    }
}
impl ExtensionSessionActions for Tools {
    fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure> {
        Ok(ThinkingLevel::Minimal)
    }
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        Ok(self.active.clone())
    }
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
        Box::pin(async { panic!("unused") })
    }
    fn set_active_tools(&self, _: Vec<String>) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn refresh_tools(&self) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn register_removed_tool_hint(&self, _: &str, _: &str) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn register_lazy_tool_activator(&self, _: LazyToolActivator) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure> {
        Err("unused".into())
    }
    fn set_model(&self, _: Model) -> ExtensionFuture<'_, bool> {
        Box::pin(async { Err("unused".into()) })
    }
    fn set_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn set_session_model(&self, _: Model) -> ExtensionFuture<'_, bool> {
        Box::pin(async { Err("unused".into()) })
    }
    fn set_session_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn set_session_fast_mode(&self, _: bool) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn exec<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: &'a Path,
        _: ExecOptions,
    ) -> ExtensionFuture<'a, ExecResult> {
        Box::pin(async { Err("unused".into()) })
    }
}

fn tool(name: &str, description: &str) -> ToolInfo {
    ToolInfo {
        name: name.into(),
        label: name.into(),
        description: description.into(),
        parameters: serde_json::json!({"type":"object","properties":{}}),
        prompt_guidelines: None,
        source_info: SourceInfo::default(),
        exposure: ToolExposure::Direct,
        search_text: None,
        search_keywords: Vec::new(),
        search_group: None,
        allow_lazy_activation: false,
        kernel_prelude: None,
    }
}

struct TestUi;
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

/// senpi `createExtensionContext`: print mode, no UI, the given session, registry and model.
fn context(
    session: Arc<Session>,
    registry: Arc<Registry>,
    model: Option<Model>,
) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: "/tmp".into(),
        agent_dir: "/tmp/agent".into(),
        session_manager: session,
        model_registry: registry,
        model,
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
        get_system_prompt_fn: Arc::new(|| "system".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions {
            cwd: "/tmp".into(),
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

/// senpi `captureCompactionHandlers`: the builtin registered against an API whose tool accessors are bound.
fn register(tools: Tools) -> ExtensionApi {
    let runtime = ExtensionRuntime::default();
    let tools = Arc::new(tools);
    runtime.bind(Arc::clone(&tools) as Arc<dyn ExtensionActions>);
    runtime.bind_session_actions(tools);
    let mut api = ExtensionApi::new(
        LoadedExtension::new("compaction", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        runtime,
    );
    maho_ext_compaction::CompactionExtension.register(&mut api);
    api
}

fn no_tools() -> Tools {
    Tools {
        active: Vec::new(),
        all: Vec::new(),
    }
}

async fn dispatch(api: &ExtensionApi, mut event: ExtensionEvent, context: &ExtensionContext) {
    let kind = event.kind();
    for handler in &api.registered.handlers[&kind] {
        tokio::time::timeout(WAIT, handler(&mut event, context))
            .await
            .expect("bounded handler")
            .expect("handler succeeds");
    }
}

fn before_agent_start() -> ExtensionEvent {
    ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {
        prompt: String::new(),
        trigger: "prompt".into(),
        preview: false,
        images: None,
        system_prompt: "system".into(),
        system_prompt_options: Default::default(),
    })
}

#[tokio::test]
async fn does_not_trigger_proactive_compaction_when_resolved_user_settings_disable_compaction() {
    // given
    let order = Arc::new(Mutex::new(Vec::new()));
    let settings = ResolvedCompactionSettings {
        enabled: false,
        ..default_settings()
    };
    let actions = Arc::new(Actions::new(settings, 190_000, 200_000, Arc::clone(&order)));
    let session = Arc::new(Session {
        actions: Arc::clone(&actions),
        branch: Vec::new(),
    });
    let registry = Arc::new(Registry::new(Arc::clone(&order), Vec::new()));
    let ctx = context(session, registry, None);
    let api = register(no_tools());

    // when
    dispatch(&api, before_agent_start(), &ctx).await;

    // then
    assert_eq!(actions.compacts.load(Ordering::SeqCst), 0);
    assert!(lock(&actions.applied).is_empty());
}

#[tokio::test]
async fn synchronously_applies_compaction_via_apply_compaction_in_before_agent_start_when_above_threshold()
 {
    // given
    let order = Arc::new(Mutex::new(Vec::new()));
    let settings = ResolvedCompactionSettings {
        keep_recent_tokens: 1,
        reserve_tokens: 100,
        ..default_settings()
    };
    let actions = Arc::new(Actions::new(settings, 190_000, 200_000, Arc::clone(&order)));
    let session = Arc::new(Session {
        actions,
        branch: branch(),
    });
    let registry = Arc::new(Registry::new(Arc::clone(&order), Vec::new()));
    let ctx = context(
        session,
        registry,
        Some(anthropic_model("claude-sonnet-4-5", 200_000)),
    );
    let api = register(no_tools());

    // when
    dispatch(&api, before_agent_start(), &ctx).await;
    lock(&order).push("hook-returned".into());

    // then
    assert_eq!(
        *lock(&order),
        ["auth-start", "apply-called", "hook-returned"]
    );
}

#[tokio::test]
async fn sends_only_active_registered_and_mcp_tools_to_blocking_compaction_summarization() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let settings = ResolvedCompactionSettings {
        keep_recent_tokens: 1,
        ..default_settings()
    };
    let mut actions = Actions::new(settings, 190_000, 200_000, Arc::clone(&order));
    actions.begin_signal = true;
    let session = Arc::new(Session {
        actions: Arc::new(actions),
        branch: branch(),
    });
    let registry = Arc::new(Registry::new(
        order,
        vec![Reply::Summary("summary constrained to active tools")],
    ));
    let ctx = context(
        session,
        Arc::clone(&registry),
        Some(anthropic_model("claude-tools", 200_000)),
    );
    let api = register(Tools {
        active: vec!["registered_active".into()],
        all: vec![
            tool("registered_active", "Active registered tool"),
            tool("mcp__inactive_server__query", "Inactive MCP tool"),
        ],
    });

    dispatch(&api, before_agent_start(), &ctx).await;

    assert_eq!(
        *lock(&registry.tool_names),
        [vec!["registered_active".to_owned()]]
    );
}

#[tokio::test]
async fn starts_compaction_feedback_before_blocking_extension_summary_generation() {
    // given
    let order = Arc::new(Mutex::new(Vec::new()));
    let settings = ResolvedCompactionSettings {
        keep_recent_tokens: 1,
        reserve_tokens: 100,
        ..default_settings()
    };
    let mut actions = Actions::new(settings, 190_000, 200_000, Arc::clone(&order));
    actions.begin_signal = true;
    let session = Arc::new(Session {
        actions: Arc::new(actions),
        branch: branch(),
    });
    let registry = Arc::new(Registry::new(Arc::clone(&order), Vec::new()));
    let ctx = context(
        session,
        registry,
        Some(anthropic_model("claude-sonnet-4-5", 200_000)),
    );
    let api = register(no_tools());

    // when
    dispatch(&api, before_agent_start(), &ctx).await;
    lock(&order).push("hook-returned".into());

    // then
    assert_eq!(
        *lock(&order),
        [
            "begin-extension",
            "auth-start",
            "apply-called",
            "hook-returned"
        ]
    );
}

/// What invalidates the speculative summary in `expect_speculative_compaction_invalidated_by`.
enum Invalidation {
    ModelSwitch,
    AbortedAssistantMessage,
}

/// senpi `expectSpeculativeCompactionInvalidatedBy`: a speculative summary started for the small
/// model must not be applied by the blocking route once `trigger` has invalidated it.
async fn expect_speculative_compaction_invalidated_by(trigger: Invalidation) {
    let previous_model = anthropic_model("claude-small", 200_000);
    let next_model = anthropic_model("claude-large", 800_000);
    let order = Arc::new(Mutex::new(Vec::new()));
    let settings = ResolvedCompactionSettings {
        keep_recent_tokens: 1,
        ..default_settings()
    };
    let actions = Arc::new(Actions::new(
        settings,
        130_000,
        previous_model.context_window,
        Arc::clone(&order),
    ));
    let session = Arc::new(Session {
        actions: Arc::clone(&actions),
        branch: branch(),
    });
    let (started, speculative_started) = tokio::sync::oneshot::channel();
    let registry = Arc::new(Registry::new(
        order,
        vec![Reply::Hold(started), Reply::Summary("fresh summary")],
    ));
    let mut ctx = context(session, Arc::clone(&registry), Some(previous_model.clone()));
    let api = register(no_tools());

    // given
    dispatch(&api, before_agent_start(), &ctx).await;
    tokio::time::timeout(WAIT, speculative_started)
        .await
        .expect("speculative summary request")
        .expect("speculative summary started");

    // when
    ctx.model = Some(next_model.clone());
    *lock(&actions.usage) = (130_000, next_model.context_window);
    let event = match trigger {
        Invalidation::ModelSwitch => ExtensionEvent::ModelSelect(ModelSelectEvent {
            model: next_model.clone(),
            previous_model: Some(previous_model.clone()),
            source: ModelSelectSource::Set,
            system_prompt: "system".into(),
            system_prompt_options: Default::default(),
        }),
        Invalidation::AbortedAssistantMessage => {
            let mut aborted = maho_ai::utils::lazy::setup_error_message(&next_model, "");
            aborted.stop_reason = StopReason::Aborted;
            aborted.error_message = None;
            aborted.content = Vec::new();
            ExtensionEvent::MessageEnd {
                message: AgentMessage::Llm(Message::Assistant(Box::new(aborted))),
            }
        }
    };
    dispatch(&api, event, &ctx).await;
    registry.release_held("stale summary");
    *lock(&actions.usage) = (790_000, next_model.context_window);
    dispatch(&api, before_agent_start(), &ctx).await;

    // then
    assert_eq!(*lock(&actions.applied), ["fresh summary"]);
}

#[tokio::test]
async fn drops_speculative_compaction_when_a_model_switch_happens_before_a_blocking_compaction_route()
 {
    expect_speculative_compaction_invalidated_by(Invalidation::ModelSwitch).await;
}

#[tokio::test]
async fn drops_speculative_compaction_when_an_assistant_message_is_aborted_before_a_blocking_compaction_route()
 {
    expect_speculative_compaction_invalidated_by(Invalidation::AbortedAssistantMessage).await;
}
