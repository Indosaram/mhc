//! Port of senpi `issue-3140-restored-keepalive.test.ts`: a restored session with recent cached
//! history does not arm the idle keep-alive until the person shows activity, for every
//! `session_start` reason. The extension runs behind a real `ExtensionRunner`, so activity
//! reaches it through the host's `on_user_activity` wiring.
use maho_ext_api::*;
use maho_ext_cache_keepalive::CacheKeepalive;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const SAFE_WAIT_SECONDS: f64 = 300.0;

struct Host {
    model: Model,
    entries: Vec<SessionEntry>,
    activity: Arc<Mutex<Vec<(u64, UserActivityListener)>>>,
    next_listener: std::sync::atomic::AtomicU64,
}
impl Host {
    fn record_user_activity(&self, kind: UserActivityKind) {
        let listeners: Vec<_> = self
            .activity
            .lock()
            .expect("activity listeners")
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect();
        for listener in listeners {
            listener(kind);
        }
    }
}
impl ToolSessionManager for Host {
    fn session_id(&self) -> &str {
        "restored"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for Host {
    fn get_entries(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }
    fn get_leaf_id(&self) -> Option<String> {
        self.entries.last().map(|entry| entry.id.clone())
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}
impl ExtensionActions for Host {
    fn send_message(
        &self,
        _: CustomMessage,
        _: SendMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        Err("unexpected message".into())
    }
    fn send_user_message(
        &self,
        _: UserMessageContent,
        _: SendUserMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        Err("unexpected user message".into())
    }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(Vec::new())
    }
}
impl ExtensionContextActions for Host {
    fn get_model(&self) -> Option<Model> {
        Some(self.model.clone())
    }
    fn get_service_tier(&self) -> Option<ServiceTier> {
        None
    }
    fn get_scoped_models(&self) -> Vec<ScopedModel> {
        Vec::new()
    }
    fn get_agent_dir(&self) -> PathBuf {
        "/fixture/agent".into()
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
        Box::pin(async { Err("unexpected reload".into()) })
    }
    fn is_compacting(&self) -> bool {
        false
    }
    fn check_reload_veto(&self) -> ExtensionFuture<'_, ReloadVetoDecision> {
        Box::pin(async { Ok(Default::default()) })
    }
    fn shutdown(&self) {}
    fn get_context_usage(&self) -> Option<ContextUsage> {
        None
    }
    fn get_compaction_settings(&self) -> CompactionSettings {
        panic!("unexpected compaction settings")
    }
    fn get_prompt_cache_safe_wait_seconds(&self) -> Option<f64> {
        Some(SAFE_WAIT_SECONDS)
    }
    fn get_prompt_cache_goal_backstop_max_seconds(&self) -> f64 {
        0.0
    }
    fn get_prompt_cache_keep_alive_settings(&self) -> PromptCacheKeepAliveSettings {
        PromptCacheKeepAliveSettings {
            enabled: true,
            max_requests_per_session: 1,
            max_cost_usd_per_session: 0.05,
            margin_seconds: 0.0,
        }
    }
    fn on_user_activity(&self, listener: UserActivityListener) -> Option<UserActivityUnsubscribe> {
        let id = self
            .next_listener
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.activity
            .lock()
            .expect("activity listeners")
            .push((id, listener));
        let activity = Arc::clone(&self.activity);
        Some(Box::new(move || {
            activity
                .lock()
                .expect("activity listeners")
                .retain(|(existing, _)| *existing != id)
        }))
    }
    fn get_look_at_settings(&self) -> LookAtSettings {
        panic!("unexpected settings")
    }
    fn get_ask_user_settings(&self) -> AskUserSettings {
        panic!("unexpected settings")
    }
    fn get_image_settings(&self) -> ImageSettings {
        panic!("unexpected settings")
    }
    fn session_settings(&self) -> &dyn ExtensionSessionSettings {
        panic!("unexpected settings")
    }
    fn compact(&self, _: CompactOptions) {
        panic!("unexpected compaction")
    }
    fn prepare_provider_request(
        &self,
        _: Vec<AgentMessage>,
    ) -> ExtensionFuture<'_, ProviderRequestPreparation> {
        Box::pin(async { Err("unexpected provider request".into()) })
    }
    fn begin_compaction(&self, _: BeginCompactionOptions) -> Option<AbortSignal> {
        panic!("unexpected compaction")
    }
    fn update_compaction(&self, _: UpdateCompactionOptions) {
        panic!("unexpected compaction")
    }
    fn end_compaction(&self, _: EndCompactionOptions) {
        panic!("unexpected compaction")
    }
    fn get_message_revision(&self) -> u64 {
        0
    }
    fn apply_compaction(
        &self,
        _: CompactionResult,
        _: ApplyCompactionOptions,
    ) -> ExtensionFuture<'_, ApplyCompactionResult> {
        Box::pin(async { Err("unexpected compaction".into()) })
    }
    fn get_system_prompt(&self) -> String {
        String::new()
    }
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        Default::default()
    }
    fn get_loaded_hook_sources(&self) -> LoadedHookSources {
        panic!("unexpected hooks")
    }
    fn kernel_tools(&self) -> Option<&dyn ExtensionKernelTools> {
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
        true
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}
struct Ui;
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
    fn notify(&self, message: &str, _: NotificationType) {
        panic!("unexpected notification: {message}")
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
        Box::pin(async { Err("headless fixture".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

fn model() -> Model {
    serde_json::from_value(serde_json::json!({"id":"restored","name":"restored","api":"anthropic-messages","provider":"anthropic","baseUrl":"https://api.anthropic.com","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":200000,"maxTokens":8192}))
        .expect("anthropic model fixture")
}

/// One cached assistant turn whose safe wait has already elapsed, so an armed keep-alive would
/// fire without delay (upstream advances fake timers past the wait).
fn restored_entries(model: &Model) -> Vec<SessionEntry> {
    let completed_at = maho_ai::utils::diagnostics::now_ms() - (SAFE_WAIT_SECONDS as i64) * 1000;
    let mut assistant = maho_ai::providers::faux::faux_assistant_message(
        "cached response",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            timestamp: Some(completed_at),
            ..Default::default()
        },
    );
    assistant.api = model.api.clone();
    assistant.provider = model.provider.clone();
    assistant.model = model.id.clone();
    assistant.usage.cache_read = 45_000;
    let message = serde_json::to_value(&assistant).expect("assistant message");
    let data = serde_json::json!({"type":"message","id":"cached","parentId":null,"timestamp":"2026-10-31T00:00:00.000Z","message":message});
    vec![SessionEntry {
        id: "cached".into(),
        parent_id: None,
        timestamp: "2026-10-31T00:00:00.000Z".into(),
        kind: "message".into(),
        data,
    }]
}

fn context(host: &Arc<Host>) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(Ui),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: "/fixture".into(),
        agent_dir: "/fixture/agent".into(),
        session_manager: host.clone(),
        model_registry: Arc::new(Registry),
        model: Some(host.model.clone()),
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

#[tokio::test(start_paused = true)]
async fn restored_keepalive_waits_for_first_activity_after_every_session_start_reason() {
    for reason in [
        SessionReason::Startup,
        SessionReason::Resume,
        SessionReason::Fork,
        SessionReason::Reload,
    ] {
        let model = model();
        let host = Arc::new(Host {
            entries: restored_entries(&model),
            model,
            activity: Arc::default(),
            next_listener: Default::default(),
        });
        let (warmed, mut warms) = tokio::sync::mpsc::unbounded_channel();
        let keepalive = CacheKeepalive {
            warm: Arc::new(move |_, messages, _, _| {
                let warmed = warmed.clone();
                Box::pin(async move {
                    let _receiver_alive = warmed.send(messages.len());
                    Ok::<_, ExtensionFailure>(
                        maho_ai::api::warm_prompt_cache::WarmPromptCacheResult::Unsupported,
                    )
                })
            }),
            prewarm: Arc::new(|_, _, _| {
                Box::pin(async { panic!("anthropic sessions are not prewarmed") })
            }),
        };
        let mut runner =
            maho_ext_host::ExtensionRunner::from_static(vec![Box::new(keepalive)], context(&host));
        runner.bind_core(host.clone(), context(&host));
        runner
            .bind_context_actions(host.clone())
            .expect("bind context actions");
        let start = |reason| {
            ExtensionEvent::SessionStart(SessionStartEvent {
                sizes_only: false,
                reason,
                initial_model_provenance: None,
                previous_session_file: None,
                attended: None,
            })
        };
        runner
            .emit(start(SessionReason::Startup))
            .await
            .expect("startup");
        if reason != SessionReason::Startup {
            runner.emit(start(reason)).await.expect("restart");
        }
        assert!(runner.errors.is_empty(), "{:?}", runner.errors);

        // Paused time advances only once every spawned task is idle, so an armed ping would run.
        tokio::time::sleep(std::time::Duration::from_secs(SAFE_WAIT_SECONDS as u64)).await;
        assert!(
            warms.try_recv().is_err(),
            "{reason:?}: restored history armed the keep-alive"
        );

        host.record_user_activity(UserActivityKind::Draft);
        let restored = tokio::time::timeout(std::time::Duration::from_secs(5), warms.recv())
            .await
            .expect("activity arms the keep-alive")
            .expect("warm request");
        assert_eq!(restored, 1, "{reason:?}");
        tokio::time::sleep(std::time::Duration::from_secs(SAFE_WAIT_SECONDS as u64)).await;
        // The fake provider reports the model unsupported, which stops the keep-alive.
        assert!(
            warms.try_recv().is_err(),
            "{reason:?}: one warm per activity"
        );
    }
}

/// index.ts `if (event.sizesOnly) return;`: a sizes-only `session_start` neither starts the
/// session prewarm nor arms the keep-alive, so nothing subscribes to user activity and activity
/// cannot trigger a warm. A normal start on the same runner then subscribes, proving the probe.
#[tokio::test]
async fn sizes_only_session_start_neither_prewarms_nor_arms() {
    let model = model();
    let host = Arc::new(Host {
        entries: restored_entries(&model),
        model,
        activity: Arc::default(),
        next_listener: Default::default(),
    });
    let (warmed, mut warms) = tokio::sync::mpsc::unbounded_channel();
    let keepalive = CacheKeepalive {
        warm: Arc::new(move |_, messages, _, _| {
            let warmed = warmed.clone();
            Box::pin(async move {
                let _receiver_alive = warmed.send(messages.len());
                Ok::<_, ExtensionFailure>(
                    maho_ai::api::warm_prompt_cache::WarmPromptCacheResult::Unsupported,
                )
            })
        }),
        prewarm: Arc::new(|_, _, _| Box::pin(async { panic!("sizes-only startup prewarmed") })),
    };
    let mut runner =
        maho_ext_host::ExtensionRunner::from_static(vec![Box::new(keepalive)], context(&host));
    runner.bind_core(host.clone(), context(&host));
    runner
        .bind_context_actions(host.clone())
        .expect("bind context actions");
    let start = |sizes_only| {
        ExtensionEvent::SessionStart(SessionStartEvent {
            sizes_only,
            reason: SessionReason::Startup,
            initial_model_provenance: None,
            previous_session_file: None,
            attended: None,
        })
    };
    runner.emit(start(true)).await.expect("sizes-only startup");
    assert!(runner.errors.is_empty(), "{:?}", runner.errors);
    assert!(
        host.activity.lock().expect("activity listeners").is_empty(),
        "sizes-only startup subscribed the prewarm or the startup gate"
    );
    host.record_user_activity(UserActivityKind::Draft);
    assert!(
        warms.try_recv().is_err(),
        "sizes-only startup armed the keep-alive"
    );

    runner.emit(start(false)).await.expect("startup");
    assert!(runner.errors.is_empty(), "{:?}", runner.errors);
    assert!(
        !host.activity.lock().expect("activity listeners").is_empty(),
        "a normal startup subscribes to user activity"
    );
}
