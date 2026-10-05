use maho_core::{
    agent_session::{AgentSession, AgentSessionConfig},
    model_runtime::{CreateModelRuntimeOptions, ModelRuntime},
    session_manager::SessionManager,
    settings_manager::{InMemorySettingsStorage, SettingsManager},
};
use maho_ext_api::{
    ComponentFactory, CustomUiOptions, ExtensionFuture, ExtensionUi, ExtensionUiDialogOptions, JsonValue, NotificationType,
    Theme, UiFuture, WidgetContent, ExtensionWidgetOptions,
};
use maho_ext_goal::{GoalRuntime, GoalStatus, GoalStoreChangedEvent, GoalStoreRef, GoalUpdate, GoalUpdateSource, create_goal, update_goal};
use std::sync::Arc;

struct HeadlessUi;
impl ExtensionUi for HeadlessUi {
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
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("UI unavailable".into()) }) }
    fn theme(&self) -> Theme { Theme::default() }
}

fn session(directory: &std::path::Path) -> AgentSession {
    let stream: maho_agent::types::StreamFn = Arc::new(|_, _, _| maho_ai::types::AssistantMessageEventStream::assistant());
    AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions { stream_fn: Some(stream), ..Default::default() }),
        session_manager: SessionManager::create("/workspace", Some(directory.to_str().expect("temp dir path is valid UTF-8")), None),
        settings_manager: SettingsManager::from_storage(Box::<InMemorySettingsStorage>::default(), false),
        cwd: "/workspace".into(),
        agent_dir: Some(directory.display().to_string()),
        fallback_now: None,
        retry_random: None,
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: Default::default(),
        custom_tools: Vec::new(),
        model_runtime: Some(ModelRuntime::create_sync(CreateModelRuntimeOptions { providers: Some(Vec::new()), ..Default::default() })),
        model_registry: None,
        uses_default_stream_function: Some(false),
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: Some(false),
    })
    .expect("AgentSession::new succeeds")
}

#[tokio::test]
async fn typed_goal_event_observes_owning_thread_context_and_gates_on_active_status() {
    let directory = tempfile::tempdir().unwrap();
    let session = session(directory.path());
    let thread_id = session.session_id();
    let reference = GoalStoreRef { base_dir: directory.path().join("goal"), thread_id: thread_id.clone() };
    let stored = reference.clone();
    let runtime = GoalRuntime::new(Arc::new(move |_| stored.clone()), Arc::new(|| 0.0));
    let context = session.extension_context(Arc::new(HeadlessUi));
    let event = GoalStoreChangedEvent { thread_id: thread_id.clone(), ctx: Some(context.clone()) };

    create_goal(&reference, "work", None, 0).await.unwrap();
    assert!(runtime.store_changed_with_context(&event).await.unwrap().is_some());

    update_goal(&reference, &GoalUpdate { status: Some(GoalStatus::Paused), ..Default::default() }, GoalUpdateSource::User, 1).await.unwrap();
    assert!(runtime.store_changed_with_context(&event).await.unwrap().is_none());

    update_goal(&reference, &GoalUpdate { status: Some(GoalStatus::Active), ..Default::default() }, GoalUpdateSource::User, 2).await.unwrap();
    assert!(runtime.store_changed_with_context(&event).await.unwrap().is_some());

    update_goal(&reference, &GoalUpdate { status: Some(GoalStatus::Complete), ..Default::default() }, GoalUpdateSource::Model, 3).await.unwrap();
    assert!(runtime.store_changed_with_context(&event).await.unwrap().is_none());

    let foreign = GoalStoreChangedEvent { thread_id: "other-thread".into(), ctx: Some(context) };
    assert!(runtime.store_changed_with_context(&foreign).await.unwrap().is_none());

    session.dispose().await;
}

/// UB2 causal regression: the pinned `GoalDelivery::subscribe` handler is wired to the session bus,
/// so a `goal_store_changed` publication on the bound extension runner's bus reaches the real
/// native `GoalExtension` consumer, which re-reads the store and queues a continuation through the
/// host (observed by the exact queued custom-message event, not by polling).
#[tokio::test]
async fn session_bus_store_changed_event_reaches_the_bound_goal_delivery_and_queues_a_continuation() {
    use maho_agent::types::CustomAgentMessage;
    use maho_core::agent_session::ExtensionBindings;
    use maho_ext_api::{AgentEvent, AgentMessage, AgentSessionEvent};
    use maho_ext_goal::{GoalExtension, GOAL_STORE_CHANGED_EVENT};

    let directory = tempfile::tempdir().unwrap();
    let session = session(directory.path());
    let thread_id = session.session_id();
    let base_dir = directory.path().join("goal");
    let store_ref = GoalStoreRef { base_dir: base_dir.clone(), thread_id: thread_id.clone() };
    let extension_ref = store_ref.clone();
    let reference = Arc::new(move |_ctx: &maho_ext_api::ExtensionContext| extension_ref.clone());

    // Subscribe to the session event stream BEFORE triggering, so the queued continuation is
    // observed by exact event with a bounded timeout instead of a sleep/poll loop.
    let queued = Arc::new(tokio::sync::Notify::new());
    let queued_signal = queued.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::Agent(AgentEvent::MessageStart { message }) = event
            && let AgentMessage::Custom(CustomAgentMessage::Custom(custom)) = message
            && custom.custom_type == "goal-continuation"
        {
            queued_signal.notify_one();
        }
    }));

    let context = session.extension_context(Arc::new(HeadlessUi));
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::from_static(
            vec![Box::new(GoalExtension::new(reference))],
            context,
        ))
        .await;
    // SessionStart binds the goal delivery subscription on the runner bus.
    session.bind_extensions(ExtensionBindings::default()).await;

    // Create the active goal AFTER SessionStart so the start path itself does not queue.
    create_goal(&store_ref, "work", None, 0).await.unwrap();

    // Emit exactly as the app-server goal handler does: the JSON bus event (which the delivery
    // consumer subscribes to) plus the typed event.
    session.emit_extension_event(GOAL_STORE_CHANGED_EVENT, &serde_json::json!({"threadId": thread_id}));
    session.emit_extension_event_typed(GOAL_STORE_CHANGED_EVENT, &GoalStoreChangedEvent { thread_id: thread_id.clone(), ctx: None });

    tokio::time::timeout(std::time::Duration::from_secs(5), queued.notified())
        .await
        .expect("the bound goal delivery queues a continuation when the session publishes goal_store_changed");

    session.dispose().await;
}
