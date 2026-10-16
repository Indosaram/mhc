use maho_ext_goal::{Goal, GoalStatus};
use maho_server::app_server::goal_wire::to_thread_goal;
use serde_json::{Value, json};

fn stored_goal() -> Goal {
    Goal {
        id: "g1".into(),
        thread_id: "t1".into(),
        objective: "obj".into(),
        status: GoalStatus::Active,
        token_budget: Some(100),
        tokens_used: 5,
        time_used_seconds: 1.5,
        consecutive_continuations: None,
        unattended_continuations: None,
        last_continuation_signature: None,
        continuation_stopped_at: None,
        created_at: 10,
        updated_at: 20,
        last_started_at: None,
        blocked_reason: None,
        blocked_at: None,
        completed_at: None,
    }
}

#[test]
fn thread_goal_wire_projects_lowercase_status_and_nullable_budget() {
    assert_eq!(
        serde_json::to_value(to_thread_goal(&stored_goal())).unwrap(),
        json!({"threadId":"t1","objective":"obj","status":"active","tokenBudget":100.0,"tokensUsed":5.0,"timeUsedSeconds":1.5,"createdAt":10.0,"updatedAt":20.0})
    );
    let mut absent = stored_goal();
    absent.token_budget = None;
    absent.status = GoalStatus::Blocked;
    let wire = to_thread_goal(&absent);
    assert_eq!(serde_json::to_value(&wire).unwrap()["tokenBudget"], Value::Null);
    assert_eq!(serde_json::to_value(&wire).unwrap()["status"], "blocked");
    let mut complete = stored_goal();
    complete.status = GoalStatus::Complete;
    assert_eq!(serde_json::to_value(to_thread_goal(&complete)).unwrap()["status"], "complete");
    let mut paused = stored_goal();
    paused.status = GoalStatus::Paused;
    assert_eq!(serde_json::to_value(to_thread_goal(&paused)).unwrap()["status"], "paused");
}

#[tokio::test]
async fn token_budget_and_status_params_match_pinned_validation() {
    use maho_server::app_server::{registry::RegistryConnection, runtime::AppServerRuntime};

        let directory = tempfile::tempdir().unwrap();
        let dir = directory.path().display().to_string();
        let runtime = AppServerRuntime::new(dir.clone(), dir.clone(), "1".into(), Some(dir), None).await;
        let dispatch = |params: Value| {
            let core = runtime.core.clone();
            async move {
                core.read()
                    .await
                    .registry
                    .dispatch(
                        RegistryConnection { initialized: true, ..Default::default() },
                        json!({"id":1,"method":"thread/goal/set","params":params}),
                    )
                    .await
            }
        };
        let error = |response: &Value| response["error"]["code"].as_i64().unwrap();
        let missing_thread = |params| json!({"threadId":"missing","tokenBudget":params});

        for params in [json!({}), json!({"tokenBudget":null}), json!({"tokenBudget":7}), json!({"tokenBudget":0})] {
            let mut request = json!({"threadId":"missing"});
            if let Some(value) = params.get("tokenBudget") { request["tokenBudget"] = value.clone(); }
            assert_eq!(error(&dispatch(request).await), -32600, "valid budget reaches thread lookup");
        }
        for budget in [json!(-1), json!(1.5), json!("7"), json!(9007199254740992.0)] {
            assert_eq!(error(&dispatch(missing_thread(budget)).await), -32602);
        }
        for status in ["active", "paused", "complete"] {
            assert_eq!(error(&dispatch(json!({"threadId":"missing","status":status})).await), -32600);
        }
        for status in ["blocked", "usageLimited", "budgetLimited"] {
            let response = dispatch(json!({"threadId":"missing","status":status})).await;
            assert_eq!(error(&response), -32600);
            assert!(response["error"]["message"].as_str().unwrap().contains("unsupported goal status"));
        }
        assert_eq!(error(&dispatch(json!({"threadId":"missing","status":"bogus"})).await), -32602);
        assert_eq!(error(&dispatch(json!({"threadId":"missing","objective":3})).await), -32602);
        assert_eq!(error(&dispatch(json!({"threadId":"missing","objective":"x"})).await), -32600);
        assert_eq!(error(&dispatch(json!({"threadId":"missing","objective":null})).await), -32600);
        runtime.dispose().await;
}

#[tokio::test]
async fn only_active_goals_publish_the_store_changed_event() {
    use maho_core::{
        agent_session::{AgentSession, AgentSessionConfig},
        model_runtime::{CreateModelRuntimeOptions, ModelRuntime},
        session_manager::SessionManager,
        settings_manager::{InMemorySettingsStorage, SettingsManager},
    };
    use maho_ext_api::{
        ComponentFactory, CustomUiOptions, ExtensionFuture, ExtensionUi, ExtensionUiDialogOptions,
        JsonValue, NotificationType, Theme, UiFuture, WidgetContent, ExtensionWidgetOptions,
    };
    use maho_ext_goal::{GoalRuntime, GoalStoreChangedEvent, GoalStoreRef, GoalUpdate, GoalUpdateSource, create_goal, update_goal};
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

    let directory = tempfile::tempdir().unwrap();
    let stream: maho_agent::types::StreamFn = Arc::new(|_, _, _| maho_ai::types::AssistantMessageEventStream::assistant());
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions { stream_fn: Some(stream), ..Default::default() }),
        session_manager: SessionManager::create("/workspace", Some(directory.path().to_str().unwrap()), None),
        settings_manager: SettingsManager::from_storage(Box::<InMemorySettingsStorage>::default(), false),
        cwd: "/workspace".into(),
        agent_dir: Some(directory.path().display().to_string()),
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
    }).unwrap();
    let thread_id = session.session_id();
    let reference = GoalStoreRef { base_dir: directory.path().join("goal"), thread_id: thread_id.clone() };
    let stored = reference.clone();
    let runtime = GoalRuntime::new(Arc::new(move |_| stored.clone()), Arc::new(|| 0.0));
    let context = session.extension_context(Arc::new(HeadlessUi));
    let event = GoalStoreChangedEvent { thread_id, ctx: Some(context) };

    create_goal(&reference, "work", None, 0).await.unwrap();
    assert!(runtime.store_changed_with_context(&event).await.unwrap().is_some());
    update_goal(&reference, &GoalUpdate { status: Some(GoalStatus::Blocked), reason: Some("waiting on review".into()), ..Default::default() }, GoalUpdateSource::Model, 1).await.unwrap();
    assert!(runtime.store_changed_with_context(&event).await.unwrap().is_none(), "blocked goals must not deliver");
    session.dispose().await;
}
