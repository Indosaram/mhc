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
        session_manager: SessionManager::create("/workspace", Some(directory.to_str().unwrap()), None),
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
    .unwrap()
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
