use maho_ext_api::*;
use std::{path::Path, sync::{Arc}};

struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str { "session" }
    fn session_file(&self) -> Option<&Path> { None }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}
struct TestRegistry;
impl ModelRegistry for TestRegistry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(Some("faux".into())) }) }
}
struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("UI not available".into()) }) }
    fn theme(&self) -> Theme { Theme::default() }
}
fn context() -> ExtensionContext {
    ExtensionContext { ui: Arc::new(TestUi), mode: ExtensionMode::Print, has_ui: false, cwd: "/tmp".into(), agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(TestSession), model_registry: Arc::new(TestRegistry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions { cwd: "/tmp".into(), ..Default::default() }),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None }
}

#[tokio::test]
async fn provider_owned_model_selection_stands_down_before_unbound_actions() {
    let mut api = ExtensionApi::new(LoadedExtension::new("compaction","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    maho_ext_compaction::CompactionExtension.register(&mut api);
    let mut ctx = context();
    let model: Model = serde_json::from_value(serde_json::json!({"id":"m","name":"m","api":"anthropic-messages","provider":"anthropic-subscription","baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":100000,"maxTokens":1000})).unwrap();
    ctx.model=Some(model.clone());
    let mut event=ExtensionEvent::ModelSelect(ModelSelectEvent {model,previous_model:None,source:ModelSelectSource::Set,system_prompt:String::new(),system_prompt_options:Default::default()});
    for handler in &api.registered.handlers[&EventKind::ModelSelect] {assert!(matches!(handler(&mut event,&ctx).await.unwrap(),EventResult::None));}
}

#[tokio::test]
async fn native_session_compaction_uses_registered_generator_and_persists_metadata() {
    use maho_core::agent_session::{AgentSession, AgentSessionConfig};
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, FauxAssistantMessageOptions, faux_assistant_message, register_faux_provider};
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().to_string_lossy().into_owned();
    let provider = register_faux_provider(RegisterFauxProviderOptions { api: Some("compaction-registration-faux".into()), tokens_per_second: Some(0.), ..Default::default() });
    let model = provider.get_model(None).unwrap();
    provider.set_responses(vec![faux_assistant_message(vec![ContentBlock::text("<summary>native checkpoint</summary>")], FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() }).into()]);
    let mut credentials = maho_core::auth_storage::AuthStorage::in_memory(Default::default());
    credentials.set_runtime_api_key(&model.provider,"faux");
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(temp.path().join("models.json")), credentials: Some(Arc::new(credentials)), ..Default::default()
    });
    let mut manager = maho_core::session_manager::SessionManager::in_memory(&cwd,None,None);
    manager.append_message(serde_json::json!({"role":"user","content":"old ".repeat(30000),"timestamp":0}));
    manager.append_message(serde_json::json!({"role":"assistant","content":[{"type":"text","text":"reply"}],"api":model.api,"provider":model.provider,"model":model.id,"usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":0}));
    manager.append_message(serde_json::json!({"role":"user","content":"continue","timestamp":0}));
    let storage = maho_core::settings_manager::InMemorySettingsStorage::default();
    maho_core::settings_manager::SettingsStorage::with_lock(&storage, maho_core::settings_manager::SettingsScope::Global,
        &mut |_|Some(serde_json::json!({"compaction":{"keepRecentTokens":1}}).to_string())).unwrap();
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent_for_test(model), session_manager: manager,
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(Box::new(storage),false),
        cwd: cwd.clone(), agent_dir: Some(cwd), fallback_now: Some(Arc::new(||0.)), retry_random: Some(Arc::new(||0.5)),
        scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(), model_runtime: Some(runtime), model_registry: None,
        uses_default_stream_function: Some(false), initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None, allowed_tool_names: None, excluded_tool_names: None, base_tools_override: None, session_start_event: None, auto_title_sessions: Some(false),
    }).unwrap();
    session.set_extension_runner(maho_ext_host::ExtensionRunner::from_static(vec![Box::new(maho_ext_compaction::CompactionExtension)],context())).await;
    let result = session.compact(None).await.unwrap();
    assert_eq!(result.details.as_ref().unwrap()["origin"],"core-route");
    let entries = session.with_session_manager(|manager|manager.entries());
    assert!(entries.iter().any(|entry|entry["customType"] == "compaction.agent-checkpoint"));
    assert!(entries.iter().any(|entry|entry["customType"] == "compaction.todo-snapshot"));
    assert_eq!(provider.get_call_log().len(),1);
    session.dispose().await;
    provider.unregister();
}

fn maho_agent_for_test(model: Model) -> maho_agent::Agent {
    maho_agent::Agent::new(maho_agent::AgentOptions { initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
        stream_fn: Some(Arc::new(|model, context, options|maho_ai::stream::stream_simple(model,context,options.map(|options|options.simple)))), ..Default::default() })
}
#[tokio::test]
async fn registered_lifecycle_handlers_accept_real_api_events() {
    let registered = LoadedExtension::new("compaction", "/tmp".into(), SourceInfo::default());
    let mut api = ExtensionApi::new(registered, ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_ext_compaction::CompactionExtension.register(&mut api);
    let ctx = context();
    let mut event = ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted {
        reason: CompactionReason::Extension, request_id: "request".into(),
        compaction_entry: SessionEntry { id: "compact".into(), parent_id: None, timestamp: "date".into(), kind: "compaction".into(), data: serde_json::json!({"tokensBefore":10000,"details":{"structuralYield":{"savedTokens":4000,"savingsRatio":0.4}}}) },
        from_extension: true, will_retry: false,
    });
    for handler in &api.registered.handlers[&EventKind::SessionCompact] {
        assert!(matches!(handler(&mut event, &ctx).await.unwrap(), EventResult::None));
    }
    let mut event = ExtensionEvent::AgentEnd { messages: Vec::new(), aborted: None, will_retry: None, abort_source: None };
    for handler in &api.registered.handlers[&EventKind::AgentEnd] {
        assert!(matches!(handler(&mut event, &ctx).await.unwrap(), EventResult::None));
    }
}

#[tokio::test]
async fn preaborted_compaction_does_not_read_unbound_checkpoint_actions() {
    let registered = LoadedExtension::new("compaction", "/tmp".into(), SourceInfo::default());
    let mut api = ExtensionApi::new(registered, ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_ext_compaction::CompactionExtension.register(&mut api);
    let signal = AbortSignal::default();
    signal.abort();
    let mut event = ExtensionEvent::SessionBeforeCompact(SessionBeforeCompactEvent {
        reason: CompactionReason::Manual, will_retry: false, request_id: "cancelled".into(),
        preparation: CompactionPreparation { settings: CompactionSettings { enabled: true, reserve_tokens: 100, keep_recent_tokens: 100 },
            messages_to_summarize: Vec::new(), turn_prefix_messages: Vec::new(), tokens_before: 1000, first_kept_entry_id: "keep".into(), previous_summary: None },
        branch_entries: Vec::new(), custom_instructions: None, signal,
    });
    for handler in &api.registered.handlers[&EventKind::SessionBeforeCompact] {
        assert!(matches!(handler(&mut event, &context()).await.unwrap(), EventResult::None));
    }
}

#[tokio::test]
async fn rejected_compactions_trip_registered_breaker_but_external_owner_does_not() {
    for cause in [CompactionRejectionCause::CancelledByExtension, CompactionRejectionCause::ExternalOwner] {
        let mut api = ExtensionApi::new(LoadedExtension::new("compaction", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
        maho_ext_compaction::CompactionExtension.register(&mut api);
        let ctx = context();
        for _ in 0..3 {
            let mut event = ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected { reason: CompactionReason::Threshold, request_id: "request".into(), rejection_cause: cause });
            for handler in &api.registered.handlers[&EventKind::SessionCompact] { handler(&mut event,&ctx).await.unwrap(); }
        }
        let mut event = ExtensionEvent::SessionBeforeCompact(SessionBeforeCompactEvent {
            reason: CompactionReason::Threshold, will_retry: false, request_id: "request".into(),
            preparation: CompactionPreparation { settings: CompactionSettings { enabled: true, reserve_tokens: 100, keep_recent_tokens: 100 }, messages_to_summarize: Vec::new(), turn_prefix_messages: Vec::new(), tokens_before: 1000, first_kept_entry_id: "keep".into(), previous_summary: None },
            branch_entries: Vec::new(), custom_instructions: None, signal: AbortSignal::default(),
        });
        let result = api.registered.handlers[&EventKind::SessionBeforeCompact][0](&mut event,&ctx).await;
        if cause == CompactionRejectionCause::ExternalOwner { assert!(result.is_err()); }
        else { assert!(matches!(result.unwrap(), EventResult::SessionBefore(SessionBeforeEventResult { rejection_cause: Some(CompactionRejectionCause::CircuitBreaker), .. }))); }
    }
}

#[test]
fn disabled_restoration_and_rejected_compaction_never_read_unbound_context() {
    let mut settings = maho_core::compaction::settings::default_compaction_settings();
    settings.restoration_enabled = Some(false);
    let mut state = maho_ext_compaction::restoration_tracker::RestorationTrackerState::default();
    let accepted = SessionCompactEvent::Accepted { reason: CompactionReason::Manual, request_id: "r".into(), compaction_entry: SessionEntry { id:"c".into(),parent_id:None,timestamp:String::new(),kind:"compaction".into(),data:serde_json::json!({}) },from_extension:true,will_retry:false };
    maho_ext_compaction::extension_wiring::prepare_accepted_restoration(&mut state,&context(),&accepted,&settings).unwrap();
    settings.restoration_enabled = Some(true);
    let rejected = SessionCompactEvent::Rejected { reason:CompactionReason::Manual,request_id:"r".into(),rejection_cause:CompactionRejectionCause::ExternalOwner };
    maho_ext_compaction::extension_wiring::prepare_accepted_restoration(&mut state,&context(),&rejected,&settings).unwrap();
    assert!(state.pending_payload.is_none());
}

