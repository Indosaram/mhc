use super::*;
use serde_json::json;

fn conversation_entries(entries: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    entries
        .into_iter()
        .filter(|e| {
            e.get("type").and_then(serde_json::Value::as_str) != Some("custom")
                || !matches!(
                    e.get("customType").and_then(serde_json::Value::as_str),
                    Some("session-lifecycle" | "work-custody" | "child-lifecycle")
                )
        })
        .collect()
}

#[test]
fn extension_message_update_event_omits_partial_but_keeps_tool_block_name() {
    use maho_ai::types::AssistantMessageEvent as Event;
    let partial: maho_ai::types::AssistantMessage = serde_json::from_value(serde_json::json!({
        "content": [{"type": "text", "text": "long text"}, {"type": "toolCall", "id": "call", "name": "edit", "arguments": {"path": "a"}}],
        "api": "faux", "provider": "faux", "model": "faux",
        "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
        "stopReason": "stop", "timestamp": 0,
    })).expect("partial");
    let text = extension_assistant_message_event(&Event::TextDelta {
        content_index: 0,
        delta: "d".into(),
        partial: partial.clone(),
    });
    assert_eq!(
        text,
        serde_json::json!({"type": "text_delta", "contentIndex": 0, "delta": "d"})
    );
    let tool = extension_assistant_message_event(&Event::ToolcallDelta {
        content_index: 1,
        delta: "{}".into(),
        partial: partial.clone(),
    });
    assert_eq!(tool["partial"]["content"][1]["name"], "edit");
    assert_eq!(tool["partial"]["content"][1]["type"], "toolCall");
    assert!(tool["partial"]["content"][0].is_null());
    let missing = extension_assistant_message_event(&Event::ToolcallDelta {
        content_index: 0,
        delta: "{}".into(),
        partial: partial.clone(),
    });
    assert_eq!(missing["partial"]["content"], serde_json::json!([]));
    let start = extension_assistant_message_event(&Event::Start {
        partial: partial.clone(),
    });
    assert_eq!(
        start,
        serde_json::to_value(Event::Start { partial }).expect("start")
    );
}

struct TestExtensionUi;
impl maho_ext_api::ExtensionUi for TestExtensionUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(
        &self,
        _: &str,
        _: Option<maho_ext_api::WidgetContent>,
        _: maho_ext_api::ExtensionWidgetOptions,
    ) {
    }
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        _: maho_ext_api::ComponentFactory,
        _: maho_ext_api::CustomUiOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, Value> {
        Box::pin(async { Err("UI unavailable".into()) })
    }
    fn theme(&self) -> maho_ext_api::Theme {
        Default::default()
    }
}
struct TestExtensionRegistry;
impl maho_ext_api::ModelRegistry for TestExtensionRegistry {
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
    fn get_api_key_for_provider<'a>(
        &'a self,
        _: &'a str,
    ) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}
fn test_extension_context(session: &AgentSession) -> maho_ext_api::ExtensionContext {
    maho_ext_api::ExtensionContext {
        ui: Arc::new(TestExtensionUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: session.cwd().into(),
        agent_dir: session.agent_dir().into(),
        session_manager: Arc::new(SessionContextManager::new(session)),
        model_registry: Arc::new(TestExtensionRegistry),
        model: Some(session.model()),
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
        get_system_prompt_options_fn: Arc::new(Default::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

#[tokio::test]
async fn extension_shortcuts_skip_reserved_builtins_and_keep_free_keys() {
    let session = test_session();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:shortcuts>",
        session.cwd().into(),
        Default::default(),
    );
    for key in ["Escape", "alt+k"] {
        extension.shortcuts.insert(
            key.into(),
            maho_ext_api::ExtensionShortcut {
                shortcut: key.into(),
                description: Some(key.into()),
                handler: Arc::new(|_| Box::pin(async { Ok(()) })),
                extension_path: "<inline:shortcuts>".into(),
            },
        );
    }
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let resolved = crate::keybindings::KeybindingsManager::new(Default::default(), None)
        .get_effective_config();
    let (shortcuts, diagnostics) = session.extension_shortcuts(&resolved).await;
    assert_eq!(
        shortcuts.keys().map(String::as_str).collect::<Vec<_>>(),
        ["alt+k"]
    );
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(session.extension_shortcut_context().await.is_some());
}

#[tokio::test]
async fn extension_session_views_project_cached_usage_totals() {
    // Given
    use maho_ext_api::SessionManager as _;
    let session = test_session();
    let mut assistant = maho_ai::providers::faux::faux_assistant_message("", Default::default());
    assistant.usage.input = 25;
    assistant.usage.cache_read = 75;
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::to_value(assistant).expect("assistant JSON"))
    });
    let expected = session.with_session_manager(|manager| manager.extension_usage_totals());
    let bound = SessionContextManager::new(&session);
    let weak = ExtensionSessionManagerView {
        session: Arc::downgrade(&session.inner),
        id: session.session_id(),
        file: None,
    };
    let sdk = crate::sdk::extension_context::create(&session);
    // When
    let bound_totals = bound.get_usage_totals();
    let sdk_totals = sdk.session_manager.get_usage_totals();
    // Then
    assert_eq!(bound_totals, Some(expected.clone()));
    assert_eq!(weak.get_usage_totals(), Some(expected.clone()));
    assert_eq!(sdk_totals, Some(expected));
    assert_eq!(
        sdk_totals.expect("cached totals")["latestCacheHitRate"],
        serde_json::json!(75.0)
    );
    session.dispose().await;
}

#[tokio::test]
async fn cancelling_session_stream_awaits_pending_header_setup_destruction() {
    // Given
    struct Capture(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Capture {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let session = retry_session(Vec::new(), 0);
    let mut runtime = session.model_runtime().clone();
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("configured faux auth");
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let capture = dropped.clone();
    let (entered, observed) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:pending-headers>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::BeforeProviderHeaders,
        vec![Arc::new(move |_, _| {
            let capture = Capture(capture.clone());
            let entered = lock(&entered).take();
            Box::pin(async move {
                let _capture = capture;
                if let Some(entered) = entered {
                    entered.send(()).expect("setup observer");
                }
                std::future::pending().await
            })
        })],
    );
    session
        .set_extension_runner(ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    session.install_default_stream_function();
    let stream = session.agent.stream_function()(
        &session.model(),
        &maho_ai::types::Context::default(),
        None,
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), observed)
        .await
        .expect("bounded setup entry")
        .expect("setup entered");
    // When
    tokio::time::timeout(std::time::Duration::from_secs(5), stream.cancel())
        .await
        .expect("bounded awaited cancellation")
        .expect("cancel");
    // Then
    assert!(dropped.load(Ordering::SeqCst));
    assert!(session.with_session_manager(|manager| manager.entries().is_empty()));
    session.dispose().await;
}

#[tokio::test]
async fn message_end_replacement_retains_typed_error_cause_through_persistence() {
    // Given
    let session = test_session();
    let cause = maho_ai::types::AssistantErrorCause::UsageLimit {
        scope: maho_ai::types::UsageLimitScope::Account,
    };
    let mut assistant = maho_ai::providers::faux::faux_assistant_message("", Default::default());
    assistant.stop_reason = StopReason::Error;
    assistant.error_cause = Some(cause.clone());
    let original = AgentMessage::from(maho_ai::types::Message::Assistant(std::sync::Arc::new(
        assistant,
    )));
    session.agent.set_messages(vec![original.clone()]);
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:advice>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::MessageEnd,
        vec![Arc::new(|event, _| {
            let mut message = match event {
                maho_ext_api::ExtensionEvent::MessageEnd { message } => message.clone(),
                _ => unreachable!(),
            };
            if let AgentMessage::Llm(maho_ai::types::Message::Assistant(assistant)) = &mut message {
                let assistant = std::sync::Arc::make_mut(assistant);
                assistant.error_message = Some("replacement advice".into());
                assistant.error_cause = None;
            }
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::MessageEnd {
                    message: Some(message),
                })
            })
        })],
    );
    session
        .set_extension_runner(ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    // When
    session
        .process_agent_event(
            maho_agent::types::AgentEvent::MessageEnd { message: original },
            maho_ai::utils::abort::AbortController::new().signal(),
        )
        .await;
    // Then
    assert_eq!(
        session.messages()[0]
            .as_assistant()
            .expect("assistant")
            .error_cause,
        Some(cause.clone())
    );
    session
        .rebuild_session_context()
        .expect("restore projected assistant");
    assert_eq!(
        session.messages()[0]
            .as_assistant()
            .expect("restored assistant")
            .error_cause,
        Some(cause)
    );
    session.dispose().await;
}

#[tokio::test]
async fn memory_reporters_follow_factory_load_order_and_live_registration() {
    // Given
    struct Reporter(usize);
    impl maho_ext_api::Extension for Reporter {
        fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
            let bytes = self.0;
            api.register_memory_reporter(
                "cache",
                Arc::new(move || [("bytes".into(), bytes)].into()),
            )
            .expect("register reporter");
        }
    }
    let session = test_session();
    assert!(session.extension_memory_reporters().is_empty());
    let runner = ExtensionRunner::from_static(
        vec![Box::new(Reporter(17)), Box::new(Reporter(29))],
        test_extension_context(&session),
    );
    let runtime = runner.runtime.clone();
    let first_path = runner.extensions[0].identity.path.clone();
    session.set_extension_runner(runner).await;
    runtime
        .register_memory_reporter(
            &first_path,
            "cache",
            Arc::new(|| [("bytes".into(), 31)].into()),
        )
        .expect("replace reporter");
    // When
    let figures: Vec<_> = session
        .extension_memory_reporters()
        .into_iter()
        .map(|named| (named.name, (named.reporter)()["bytes"]))
        .collect();
    // Then
    assert_eq!(figures, vec![("cache".into(), 31), ("cache".into(), 29)]);
    session.dispose().await;
}

#[tokio::test]
async fn sequential_nested_call_can_reenter_without_waiting_on_its_own_queue() {
    // Given
    let session = test_session();
    session
        .agent
        .set_tool_execution(maho_agent::types::ToolExecutionMode::Sequential);
    let mut leaf = test_tool("leaf");
    leaf.execute = Arc::new(|id, _, _, _| {
        Box::pin(async move {
            assert!(id.ends_with("/1/1"));
            AgentToolResult::text("leaf")
        })
    });
    session.register_tool_definition(test_definition("leaf"), empty_source_info(), leaf);
    for (name, child) in [("middle", "leaf"), ("root", "middle")] {
        let weak = session.weak_accessor();
        let mut tool = test_tool(name);
        tool.execution_mode = Some(maho_agent::types::ToolExecutionMode::Sequential);
        tool.execute = Arc::new(move |_, _, _, _| {
            let session = weak().expect("live session");
            Box::pin(async move {
                session
                    .execute_tool(child, serde_json::json!({}), Default::default())
                    .await
                    .expect("child");
                AgentToolResult::text("parent")
            })
        });
        session.register_tool_definition(test_definition(name), empty_source_info(), tool);
    }
    session.set_active_tools_by_name(vec!["root".into(), "middle".into(), "leaf".into()]);
    // When
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.execute_tool("root", serde_json::json!({}), Default::default()),
    )
    .await
    .expect("reentrant queue")
    .expect("root");
    // Then
    let nested = result.nested_calls.expect("actual children");
    assert!(nested.complete);
    assert_eq!(nested.calls.len(), 2);
    assert_eq!(nested.calls[1].id, format!("{}/1", nested.calls[0].id));
    session.dispose().await;
}

#[tokio::test]
async fn nested_execute_tool_records_real_calls_and_persists_parent_result() {
    // Given
    let mut assistant = maho_ai::providers::faux::faux_assistant_message("", Default::default());
    assistant.stop_reason = StopReason::ToolUse;
    assistant.content = vec![maho_ai::types::ContentBlock::ToolCall(
        maho_ai::types::ToolCall {
            id: "parent".into(),
            name: "outer".into(),
            arguments: Map::new(),
            ..Default::default()
        },
    )];
    let session = retry_session(
        vec![
            assistant,
            maho_ai::providers::faux::faux_assistant_message("done", Default::default()),
        ],
        0,
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::Agent(event) = event {
            lock(&captured).push(event.clone());
        }
    }));
    let mut inner = test_tool("inner");
    inner.execution_mode = Some(maho_agent::types::ToolExecutionMode::Sequential);
    inner.execute = Arc::new(|id, _, _, update| {
        Box::pin(async move {
            assert_eq!(id, "parent/1");
            if let Some(update) = update {
                update(AgentToolResult::text("progress"));
            }
            let mut result = AgentToolResult::text("inner");
            result.usage = Some(maho_ai::types::Usage {
                context_usage: None,
                input: 7,
                total_tokens: 7,
                ..Default::default()
            });
            result
        })
    });
    session.register_tool_definition(test_definition("inner"), empty_source_info(), inner);
    let weak = session.weak_accessor();
    let mut outer = test_tool("outer");
    outer.execute = Arc::new(move |_, _, _, _| {
        let session = weak().expect("live parent session");
        Box::pin(async move {
            session
                .execute_tool("inner", serde_json::json!({}), Default::default())
                .await
                .expect("nested execution");
            AgentToolResult::text("outer")
        })
    });
    session.register_tool_definition(test_definition("outer"), empty_source_info(), outer);
    session.set_active_tools_by_name(vec!["outer".into(), "inner".into()]);
    // When
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("input", Default::default()),
    )
    .await
    .expect("bounded nested turn")
    .expect("prompt");
    // Then
    let results: Vec<_> = session
        .messages()
        .into_iter()
        .filter_map(|message| match message {
            AgentMessage::Llm(maho_ai::types::Message::ToolResult(result)) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1);
    let nested = results[0]
        .nested_calls
        .as_ref()
        .expect("real nested record");
    assert!(nested.complete);
    assert_eq!(nested.calls.len(), 1);
    assert_eq!(nested.calls[0].id, "parent/1");
    assert_eq!(nested.calls[0].name, "inner");
    assert_eq!(
        nested.calls[0].status,
        maho_ai::types::NestedToolCallStatus::Ok
    );
    assert_eq!(results[0].usage.expect("nested usage").input, 7);
    let nested_events: Vec<_> = lock(&events)
        .iter()
        .filter_map(|event| match event {
            maho_agent::types::AgentEvent::ToolExecutionStart {
                tool_call_id,
                parent_tool_call_id: Some(parent),
                ..
            } => Some(("start", tool_call_id.clone(), parent.clone())),
            maho_agent::types::AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                parent_tool_call_id: Some(parent),
                ..
            } => Some(("update", tool_call_id.clone(), parent.clone())),
            maho_agent::types::AgentEvent::ToolExecutionEnd {
                tool_call_id,
                parent_tool_call_id: Some(parent),
                ..
            } => Some(("end", tool_call_id.clone(), parent.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        nested_events,
        vec![
            ("start", "parent/1".into(), "parent".into()),
            ("update", "parent/1".into(), "parent".into()),
            ("end", "parent/1".into(), "parent".into())
        ]
    );
    let persisted = session
        .with_session_manager(|manager| manager.entries())
        .into_iter()
        .find(|entry| entry["message"]["toolCallId"] == "parent")
        .expect("persisted parent");
    assert_eq!(
        persisted["message"]["nestedCalls"],
        serde_json::to_value(nested).expect("nested JSON")
    );
    session
        .rebuild_session_context()
        .expect("restore transcript");
    assert!(session.messages().iter().any(|message| matches!(message,
        AgentMessage::Llm(maho_ai::types::Message::ToolResult(result)) if result.nested_calls.as_ref() == Some(nested))));
    session.dispose().await;
}

#[tokio::test]
async fn native_provider_tool_calls_dispatch_call_and_result_hooks() {
    let mut assistant = maho_ai::providers::faux::faux_assistant_message("", Default::default());
    assistant.stop_reason = StopReason::ToolUse;
    assistant.content = vec![maho_ai::types::ContentBlock::ToolCall(
        maho_ai::types::ToolCall {
            id: "call".to_owned(),
            name: "echo".to_owned(),
            arguments: Map::new(),
            ..Default::default()
        },
    )];
    let session = retry_session(
        vec![
            assistant,
            maho_ai::providers::faux::faux_assistant_message("done", Default::default()),
        ],
        0,
    );
    session.register_tool_definition(
        test_definition("echo"),
        empty_source_info(),
        test_tool("echo"),
    );
    session.set_active_tools_by_name(vec!["echo".to_owned()]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:hooks>",
        session.cwd().into(),
        Default::default(),
    );
    for kind in [
        maho_ext_api::EventKind::Context,
        maho_ext_api::EventKind::ToolCall,
        maho_ext_api::EventKind::ToolResult,
    ] {
        let seen = seen.clone();
        let session = Arc::downgrade(&session.inner);
        extension.handlers.insert(
            kind,
            vec![Arc::new(move |event, _| {
                let seen = seen.clone();
                let session = AgentSession {
                    inner: session.upgrade().expect("live callback session"),
                };
                Box::pin(async move {
                    assert!(session.has_extension_handlers(kind).await);
                    lock(&seen).push(kind);
                    if let maho_ext_api::ExtensionEvent::ToolResult(_) = event {
                        Ok(maho_ext_api::EventResult::ToolResult(
                            maho_ext_api::ToolResultEventResult {
                                content: Some(vec![maho_tools::definition::ToolContent::text(
                                    "rewritten",
                                )]),
                                ..Default::default()
                            },
                        ))
                    } else {
                        Ok(maho_ext_api::EventResult::None)
                    }
                })
            })],
        );
    }
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let prompt = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("input", Default::default()),
    )
    .await;
    let messages = session.messages();
    session.dispose().await;
    prompt
        .expect("bounded reentrant provider callbacks")
        .expect("prompt");
    assert_eq!(
        *lock(&seen),
        [
            maho_ext_api::EventKind::Context,
            maho_ext_api::EventKind::ToolCall,
            maho_ext_api::EventKind::ToolResult,
            maho_ext_api::EventKind::Context
        ]
    );
    let result = messages
        .iter()
        .find_map(|message| match message {
            AgentMessage::Llm(maho_ai::types::Message::ToolResult(result)) => Some(result),
            _ => None,
        })
        .expect("tool result");
    assert_eq!(
        maho_ai::utils::text::content_text(&result.content, ""),
        "rewritten"
    );
}

#[tokio::test]
async fn agent_loop_resolves_registered_search_tool_on_first_call() {
    let mut assistant = maho_ai::providers::faux::faux_assistant_message("", Default::default());
    assistant.stop_reason = StopReason::ToolUse;
    assistant.content = vec![maho_ai::types::ContentBlock::ToolCall(
        maho_ai::types::ToolCall {
            id: "lazy-call".to_owned(),
            name: "mcp_fake_echo".to_owned(),
            arguments: Map::new(),
            ..Default::default()
        },
    )];
    let session = retry_session(
        vec![
            assistant,
            maho_ai::providers::faux::faux_assistant_message("done", Default::default()),
        ],
        0,
    );
    let mut definition = test_definition("mcp_fake_echo");
    definition.exposure = Some(ToolExposure::Search);
    definition.allow_lazy_activation = Some(true);
    let mut tool = test_tool("mcp_fake_echo");
    tool.execute =
        Arc::new(|_, _, _, _| Box::pin(async { AgentToolResult::text("mcp_fake_echo") }));
    session.register_tool_definition(definition, empty_source_info(), tool);
    assert!(
        !session
            .get_active_tool_names()
            .iter()
            .any(|name| name == "mcp_fake_echo")
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("input", Default::default()),
    )
    .await
    .expect("bounded lazy tool turn")
    .expect("prompt");
    let messages = session.messages();
    let result = messages
        .iter()
        .find_map(|message| match message {
            AgentMessage::Llm(maho_ai::types::Message::ToolResult(result)) => Some(result),
            _ => None,
        })
        .expect("first-call tool result");
    assert_eq!(
        maho_ai::utils::text::content_text(&result.content, ""),
        "mcp_fake_echo"
    );
    assert!(
        session
            .get_active_tool_names()
            .iter()
            .any(|name| name == "mcp_fake_echo")
    );
    session.dispose().await;
}

#[tokio::test]
async fn after_tool_call_without_runner_converts_bmp_and_preserves_result_metadata() {
    use base64::Engine;

    let bmp = {
        let mut bytes = vec![0; 58];
        bytes[0..2].copy_from_slice(b"BM");
        bytes[2..6].copy_from_slice(&58_u32.to_le_bytes());
        bytes[10..14].copy_from_slice(&54_u32.to_le_bytes());
        bytes[14..18].copy_from_slice(&40_u32.to_le_bytes());
        bytes[18..22].copy_from_slice(&1_i32.to_le_bytes());
        bytes[22..26].copy_from_slice(&1_i32.to_le_bytes());
        bytes[26..28].copy_from_slice(&1_u16.to_le_bytes());
        bytes[28..30].copy_from_slice(&24_u16.to_le_bytes());
        bytes[34..38].copy_from_slice(&4_u32.to_le_bytes());
        bytes[56] = 0xff;
        bytes
    };
    let mut assistant = maho_ai::providers::faux::faux_assistant_message("", Default::default());
    assistant.stop_reason = StopReason::ToolUse;
    assistant.content = vec![maho_ai::types::ContentBlock::ToolCall(
        maho_ai::types::ToolCall {
            id: "image-call".to_owned(),
            name: "image_result".to_owned(),
            arguments: Map::new(),
            ..Default::default()
        },
    )];
    let session = retry_session(
        vec![
            assistant,
            maho_ai::providers::faux::faux_assistant_message("done", Default::default()),
        ],
        0,
    );
    session.register_tool_definition(
        test_definition("image_result"),
        empty_source_info(),
        test_tool("image_result"),
    );
    session.set_active_tools_by_name(vec!["image_result".to_owned()]);
    let mut tool = test_tool("image_result");
    tool.execute = Arc::new(move |_, _, _, _| {
        let data = base64::engine::general_purpose::STANDARD.encode(&bmp);
        Box::pin(async move {
            let mut result = AgentToolResult::text("tool output");
            result.content = vec![maho_ai::types::ContentBlock::Image(
                maho_ai::types::ImageContent {
                    data,
                    mime_type: "image/bmp".to_owned(),
                },
            )];
            result.details = serde_json::json!({"source":"tool"});
            result.is_error = Some(true);
            result.usage = Some(maho_ai::types::Usage {
                input: 7,
                output: 3,
                ..Default::default()
            });
            result
        })
    });
    session.register_tool_definition(test_definition("image_result"), empty_source_info(), tool);
    session.set_active_tools_by_name(vec!["image_result".to_owned()]);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("input", Default::default()),
    )
    .await
    .expect("bounded tool turn")
    .expect("prompt");
    let result = session
        .messages()
        .iter()
        .find_map(|message| match message {
            AgentMessage::Llm(maho_ai::types::Message::ToolResult(result)) => Some(result.clone()),
            _ => None,
        })
        .expect("tool result");
    session.dispose().await;
    assert_eq!(result.details, Some(serde_json::json!({"source":"tool"})));
    assert!(result.is_error);
    assert_eq!(
        result.usage,
        Some(maho_ai::types::Usage {
            input: 7,
            output: 3,
            ..Default::default()
        })
    );
    assert!(
        matches!(&result.content[0], maho_ai::types::ContentBlock::Image(image) if image.mime_type == "image/png")
    );
}

#[tokio::test]
async fn after_tool_call_without_runner_resizes_oversized_png_only_when_enabled() {
    use base64::Engine;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&2001_u32.to_be_bytes());
    ihdr.extend_from_slice(&1_u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    let raw = std::iter::once(0)
        .chain(std::iter::repeat_n(0_u8, 2001))
        .collect::<Vec<_>>();
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw).expect("write image fixture");
    let compressed = encoder.finish().expect("finish image fixture");
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let png_chunk = |kind: &[u8; 4], body: &[u8]| {
        let mut chunk = Vec::with_capacity(body.len() + 12);
        chunk.extend_from_slice(&(body.len() as u32).to_be_bytes());
        chunk.extend_from_slice(kind);
        chunk.extend_from_slice(body);
        let mut crc = !0_u32;
        for byte in kind.iter().chain(body) {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
            }
        }
        chunk.extend_from_slice(&(!crc).to_be_bytes());
        chunk
    };
    png.extend(png_chunk(b"IHDR", &ihdr));
    png.extend(png_chunk(b"IDAT", &compressed));
    png.extend(png_chunk(b"IEND", &[]));
    let image_data = base64::engine::general_purpose::STANDARD.encode(png);
    for auto_resize in [false, true] {
        let image_data = image_data.clone();
        let mut assistant =
            maho_ai::providers::faux::faux_assistant_message("", Default::default());
        assistant.stop_reason = StopReason::ToolUse;
        assistant.content = vec![maho_ai::types::ContentBlock::ToolCall(
            maho_ai::types::ToolCall {
                id: "text-image-call".to_owned(),
                name: "text_image_result".to_owned(),
                arguments: Map::new(),
                ..Default::default()
            },
        )];
        let session = retry_session(
            vec![
                assistant,
                maho_ai::providers::faux::faux_assistant_message("done", Default::default()),
            ],
            0,
        );
        session
            .with_settings_manager_mut(|manager| {
                manager.set(
                    crate::settings_manager::SettingsScope::Global,
                    &serde_json::from_value(
                        serde_json::json!({"images":{"autoResize":auto_resize}}),
                    )
                    .expect("settings"),
                )
            })
            .expect("save settings");
        let mut tool = test_tool("text_image_result");
        tool.execute = Arc::new(move |_, _, _, _| {
            let image_data = image_data.clone();
            Box::pin(async move {
                let mut result = AgentToolResult::text("unchanged text");
                result.content.push(maho_ai::types::ContentBlock::Image(
                    maho_ai::types::ImageContent {
                        data: image_data,
                        mime_type: "image/png".to_owned(),
                    },
                ));
                result
            })
        });
        session.register_tool_definition(
            test_definition("text_image_result"),
            empty_source_info(),
            tool,
        );
        session.set_active_tools_by_name(vec!["text_image_result".to_owned()]);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session.prompt("input", Default::default()),
        )
        .await
        .expect("bounded tool turn")
        .expect("prompt");
        let result = session
            .messages()
            .iter()
            .find_map(|message| match message {
                AgentMessage::Llm(maho_ai::types::Message::ToolResult(result)) => {
                    Some(result.clone())
                }
                _ => None,
            })
            .expect("tool result");
        session.dispose().await;
        assert!(
            matches!(&result.content[0], maho_ai::types::ContentBlock::Text(text) if text.text == "unchanged text")
        );
        assert!(
            matches!(&result.content[1], maho_ai::types::ContentBlock::Image(image) if image.mime_type == "image/png")
        );
        let maho_ai::types::ContentBlock::Image(image) = &result.content[1] else {
            panic!("image block");
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&image.data)
            .expect("PNG base64");
        let width = u32::from_be_bytes(bytes[16..20].try_into().expect("width"));
        let height = u32::from_be_bytes(bytes[20..24].try_into().expect("height"));
        if auto_resize {
            assert!(
                width < 2001 && height <= 1,
                "oversized image must be resized"
            );
        } else {
            assert_eq!((width, height), (2001, 1));
        }
    }
}

#[tokio::test]
async fn prompt_admission_receipt_precedes_provider_completion() {
    use std::{future::Future, task::Poll};
    let session = retry_session(Vec::new(), 0);
    session.agent.set_stream_function(Arc::new(|_, _, _| {
        maho_ai::utils::event_stream::create_assistant_message_event_stream()
    }));
    let receipts = Arc::new(Mutex::new(Vec::new()));
    let observed = receipts.clone();
    let mut prompt = Box::pin(session.prompt(
        "input",
        PromptOptions {
            prompt_admitted: Some(Arc::new(move |disposition| {
                lock(&observed).push(disposition)
            })),
            session_title_prompt: Some(SessionTitlePrompt::Disabled),
            ..Default::default()
        },
    ));
    let pending =
        std::future::poll_fn(|cx| Poll::Ready(prompt.as_mut().poll(cx).is_pending())).await;
    let admitted = lock(&receipts).clone();
    drop(prompt);
    session.dispose().await;
    assert!(pending, "provider response remains pending");
    assert_eq!(admitted, [PromptDisposition::Started]);
}

#[tokio::test]
async fn queued_prompt_admission_receipt_settles_once_without_provider_completion() {
    let session = retry_session(Vec::new(), 0);
    session.state().prompt_start_pending = true;
    let receipts = Arc::new(Mutex::new(Vec::new()));
    let observed = receipts.clone();
    let result = session
        .prompt(
            "queued",
            PromptOptions {
                streaming_behavior: Some(StreamingBehavior::FollowUp),
                prompt_admitted: Some(Arc::new(move |disposition| {
                    lock(&observed).push(disposition)
                })),
                ..Default::default()
            },
        )
        .await;
    session.state().prompt_start_pending = false;
    session.dispose().await;
    assert_eq!(result, Ok(PromptDisposition::Queued));
    assert_eq!(*lock(&receipts), [PromptDisposition::Queued]);
}

#[tokio::test]
async fn handled_prompt_admission_receipt_settles_once_after_input_handler() {
    let session = retry_session(Vec::new(), 0);
    let receipts = Arc::new(Mutex::new(Vec::new()));
    let handler_receipts = receipts.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:handled>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::Input,
        vec![Arc::new(move |_, _| {
            assert!(lock(&handler_receipts).is_empty());
            Box::pin(async {
                Ok(maho_ext_api::EventResult::Input(
                    maho_ext_api::InputEventResult::Handled,
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let observed = receipts.clone();
    let result = session
        .prompt(
            "handled",
            PromptOptions {
                prompt_admitted: Some(Arc::new(move |disposition| {
                    lock(&observed).push(disposition)
                })),
                ..Default::default()
            },
        )
        .await;
    session.dispose().await;
    assert_eq!(result, Ok(PromptDisposition::Handled));
    assert_eq!(*lock(&receipts), [PromptDisposition::Handled]);
}

#[tokio::test]
async fn rejected_and_cancelled_prompt_never_publish_success_admission() {
    let session = retry_session(Vec::new(), 0);
    let receipts = Arc::new(Mutex::new(Vec::new()));
    let mut model = session.model();
    model.context_window = 128;
    session.agent.set_model(model);
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false}),
        )]))
    });
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:reject>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::BeforeAgentStart,
        vec![Arc::new(|_, _| {
            Box::pin(async {
                Ok(maho_ext_api::EventResult::BeforeAgentStart(
                    maho_ext_api::BeforeAgentStartEventResult {
                        system_prompt_parts: None,
                        message: Some(maho_ext_api::CustomMessage {
                            custom_type: "admission-fixture".into(),
                            content: vec![maho_tools::definition::ToolContent::text(
                                "large input ".repeat(200),
                            )],
                            display: true,
                            details: None,
                        }),
                        system_prompt: None,
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let observed = receipts.clone();
    let rejected = session
        .prompt(
            "rejected",
            PromptOptions {
                prompt_admitted: Some(Arc::new(move |disposition| {
                    lock(&observed).push(disposition)
                })),
                ..Default::default()
            },
        )
        .await;
    let controller = maho_ai::utils::abort::AbortController::new();
    controller.abort(None);
    let observed = receipts.clone();
    let cancelled = session
        .prompt(
            "cancelled",
            PromptOptions {
                signal: Some(controller.signal()),
                prompt_admitted: Some(Arc::new(move |disposition| {
                    lock(&observed).push(disposition)
                })),
                ..Default::default()
            },
        )
        .await;
    session.dispose().await;
    assert_eq!(
        rejected,
        Err("Compaction required before provider request".into())
    );
    assert_eq!(cancelled, Err("Prompt cancelled".into()));
    assert!(lock(&receipts).is_empty());
}

#[tokio::test]
async fn failed_provider_tool_preflight_blocks_executor() {
    let mut assistant = maho_ai::providers::faux::faux_assistant_message("", Default::default());
    assistant.stop_reason = StopReason::ToolUse;
    assistant.content = vec![maho_ai::types::ContentBlock::ToolCall(
        maho_ai::types::ToolCall {
            id: "call".to_owned(),
            name: "echo".to_owned(),
            arguments: Map::new(),
            ..Default::default()
        },
    )];
    let session = retry_session(
        vec![
            assistant,
            maho_ai::providers::faux::faux_assistant_message("done", Default::default()),
        ],
        0,
    );
    let calls = Arc::new(AtomicU64::new(0));
    let captured = calls.clone();
    let mut tool = test_tool("echo");
    tool.execute = Arc::new(move |_, _, _, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { AgentToolResult::text("unexpected") })
    });
    session.register_tool_definition(test_definition("echo"), empty_source_info(), tool);
    session.set_active_tools_by_name(vec!["echo".to_owned()]);
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:failure>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::ToolCall,
        vec![Arc::new(|_, _| {
            Box::pin(async { Err("hook refused".into()) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("input", Default::default()),
    )
    .await
    .expect("bounded prompt")
    .expect("prompt");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let messages = session.messages();
    let result = messages
        .iter()
        .find_map(|message| match message {
            AgentMessage::Llm(maho_ai::types::Message::ToolResult(result)) => Some(result),
            _ => None,
        })
        .expect("blocked tool result");
    assert!(result.is_error);
    assert!(maho_ai::utils::text::content_text(&result.content, "").contains("hook refused"));
}

#[tokio::test]
async fn registered_direct_tool_consumes_private_monitor_approval_once() {
    let session = retry_session(Vec::new(), 0);
    let observed = Arc::new(Mutex::new(None));
    let captured = observed.clone();
    let actions = session.extension_context_actions();
    let mut tool = test_tool("monitor_fixture");
    tool.execute = Arc::new(move |id, input, _, _| {
        let actions = actions.clone();
        let captured = captured.clone();
        Box::pin(async move {
            let parent = actions
                .take_approved_monitor_parent(&id, &input)
                .expect("execution take");
            assert_eq!(
                actions
                    .take_approved_monitor_parent(&id, &input)
                    .expect("second take"),
                None
            );
            *lock(&captured) = parent;
            AgentToolResult::text("registered execution")
        })
    });
    session.register_tool_definition(
        test_definition("monitor_fixture"),
        empty_source_info(),
        tool,
    );
    session.set_active_tools_by_name(vec!["monitor_fixture".into()]);
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:monitor-admission>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::ToolCall,
        vec![Arc::new(|event, context| {
            let maho_ext_api::ExtensionEvent::ToolCall(event) = event else {
                panic!("tool call");
            };
            context
                .set_approved_monitor_parent(
                    &event.tool_call_id,
                    &event.input,
                    std::path::Path::new("/approved"),
                )
                .expect("preflight attachment");
            assert!(
                context
                    .take_approved_monitor_parent(&event.tool_call_id, &event.input)
                    .is_err()
            );
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let result = session
        .execute_tool("monitor_fixture", serde_json::json!({}), Default::default())
        .await;
    session.dispose().await;
    assert!(result.is_ok());
    assert_eq!(
        *lock(&observed),
        Some(std::path::PathBuf::from("/approved"))
    );
    assert!(
        session
            .extension_context_actions()
            .take_approved_monitor_parent("any", &serde_json::json!({}))
            .is_err()
    );
}

#[tokio::test]
async fn registered_non_monitor_tool_receives_valid_input_rewritten_by_hook() {
    let session = retry_session(Vec::new(), 0);
    let observed = Arc::new(Mutex::new(None));
    let captured = observed.clone();
    let actions = session.extension_context_actions();
    let mut tool = test_tool("rewrite_fixture");
    tool.tool.parameters = serde_json::json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"]});
    tool.execute = Arc::new(move |id, input, _, _| {
        assert!(actions.take_approved_monitor_parent(&id, &input).is_err());
        *lock(&captured) = Some(input);
        Box::pin(async { AgentToolResult::text("rewritten execution") })
    });
    session.register_tool_definition(
        test_definition("rewrite_fixture"),
        empty_source_info(),
        tool,
    );
    session.set_active_tools_by_name(vec!["rewrite_fixture".into()]);
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:rewrite>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::ToolCall,
        vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::ToolCall(event) = event else {
                panic!("tool call");
            };
            event.input["value"] = "changed".into();
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let result = session
        .execute_tool(
            "rewrite_fixture",
            serde_json::json!({"value":"original"}),
            Default::default(),
        )
        .await;
    assert!(
        session
            .extension_context_actions()
            .take_approved_monitor_parent("no-scope", &serde_json::json!({}))
            .is_err()
    );
    session.dispose().await;
    assert!(
        result.is_ok(),
        "unrelated tool rewriting must not require monitor identity: {result:?}"
    );
    assert_eq!(
        *lock(&observed),
        Some(serde_json::json!({"value":"changed"}))
    );
}

#[tokio::test]
async fn monitor_approval_retires_on_same_cwd_runtime_invalidation() {
    let session = retry_session(Vec::new(), 0);
    let actions = session.extension_context_actions();
    let input = serde_json::json!({});
    let invocation = monitor_invocation::Invocation::new(
        Arc::as_ptr(&session.inner) as usize,
        session.monitor_generation.load(Ordering::SeqCst),
        "id".into(),
        "monitor".into(),
        input.clone(),
        session.cwd().into(),
    );
    let retirement = monitor_invocation::Retirement(invocation.clone());
    monitor_invocation::CURRENT
        .scope(invocation.clone(), async {
            actions
                .set_approved_monitor_parent("id", &input, std::path::Path::new("/approved"))
                .expect("attachment");
            invocation.admit("monitor", &input).expect("admission");
            session.invalidate_extension_runtime().await;
            assert!(actions.take_approved_monitor_parent("id", &input).is_err());
            assert!(
                actions
                    .set_approved_monitor_parent("id", &input, std::path::Path::new("/late"))
                    .is_err()
            );
        })
        .await;
    drop(retirement);
    session.dispose().await;
}

#[tokio::test]
async fn cancellation_during_monitor_preflight_prevents_executor_admission() {
    let session = retry_session(Vec::new(), 0);
    let calls = Arc::new(AtomicU64::new(0));
    let executed = calls.clone();
    let mut tool = test_tool("monitor_fixture");
    tool.execute = Arc::new(move |_, _, _, _| {
        executed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { AgentToolResult::text("unexpected") })
    });
    session.register_tool_definition(
        test_definition("monitor_fixture"),
        empty_source_info(),
        tool,
    );
    session.set_active_tools_by_name(vec!["monitor_fixture".into()]);
    let (entered, entry) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:cancel-preflight>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::ToolCall,
        vec![Arc::new(move |event, context| {
            let maho_ext_api::ExtensionEvent::ToolCall(event) = event else {
                panic!("tool call");
            };
            context
                .set_approved_monitor_parent(
                    &event.tool_call_id,
                    &event.input,
                    std::path::Path::new("/approved"),
                )
                .expect("attachment");
            let entered = entered.clone();
            Box::pin(async move {
                lock(&entered)
                    .take()
                    .expect("one hook")
                    .send(())
                    .expect("entry observer");
                std::future::pending::<
                    Result<maho_ext_api::EventResult, maho_ext_api::ExtensionFailure>,
                >()
                .await
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let controller = maho_ai::utils::abort::AbortController::new();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            session.execute_tool(
                "monitor_fixture",
                serde_json::json!({}),
                ExecuteToolOptions {
                    signal: Some(controller.signal()),
                    ..Default::default()
                }
            ),
            async {
                entry.await.expect("hook entered");
                controller.abort(None);
            }
        )
        .0
    })
    .await;
    session.dispose().await;
    assert!(result.expect("bounded preflight cancellation").is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn direct_monitor_cancellation_drops_pending_execution_and_retires_scope() {
    let session = retry_session(Vec::new(), 0);
    let (entered, entry) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let retained = Arc::new(Mutex::new(None));
    let captured = retained.clone();
    let mut tool = test_tool("monitor_fixture");
    tool.execute = Arc::new(move |id, input, _, _| {
        let entered = entered.clone();
        let captured = captured.clone();
        Box::pin(async move {
            *lock(&captured) = Some((monitor_invocation::CURRENT.with(Arc::clone), id, input));
            lock(&entered)
                .take()
                .expect("one execution")
                .send(())
                .expect("entry observer");
            std::future::pending::<AgentToolResult>().await
        })
    });
    session.register_tool_definition(
        test_definition("monitor_fixture"),
        empty_source_info(),
        tool,
    );
    session.set_active_tools_by_name(vec!["monitor_fixture".into()]);
    let controller = maho_ai::utils::abort::AbortController::new();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            session.execute_tool(
                "monitor_fixture",
                serde_json::json!({}),
                ExecuteToolOptions {
                    signal: Some(controller.signal()),
                    ..Default::default()
                }
            ),
            async {
                entry.await.expect("execution entered");
                controller.abort(None);
            }
        )
        .0
    })
    .await;
    session.dispose().await;
    assert!(result.expect("bounded cancelled execution").is_err());
    let (invocation, id, input) = lock(&retained).take().expect("retained scope");
    assert!(
        invocation
            .take(&id, &input, std::path::Path::new(&session.cwd()))
            .is_err()
    );
}

#[tokio::test]
async fn dropping_direct_monitor_execution_retires_its_approval() {
    let session = retry_session(Vec::new(), 0);
    let (entered, entry) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let retained = Arc::new(Mutex::new(None));
    let captured = retained.clone();
    let mut tool = test_tool("monitor_fixture");
    tool.execute = Arc::new(move |id, input, _, _| {
        let captured = captured.clone();
        let entered = entered.clone();
        Box::pin(async move {
            *lock(&captured) = Some((monitor_invocation::CURRENT.with(Arc::clone), id, input));
            lock(&entered)
                .take()
                .expect("single executor")
                .send(())
                .expect("entry observer");
            std::future::pending::<AgentToolResult>().await
        })
    });
    session.register_tool_definition(
        test_definition("monitor_fixture"),
        empty_source_info(),
        tool,
    );
    session.set_active_tools_by_name(vec!["monitor_fixture".into()]);
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:monitor-drop>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::ToolCall,
        vec![Arc::new(|event, context| {
            let maho_ext_api::ExtensionEvent::ToolCall(event) = event else {
                panic!("tool call");
            };
            context
                .set_approved_monitor_parent(
                    &event.tool_call_id,
                    &event.input,
                    std::path::Path::new("/approved"),
                )
                .expect("preflight approval");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let mut execution = Box::pin(session.execute_tool(
        "monitor_fixture",
        serde_json::json!({}),
        Default::default(),
    ));
    let admission = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            result = entry => result.map_err(|error| error.to_string()),
            result = &mut execution => Err(format!("executor unexpectedly settled: {result:?}")),
        }
    })
    .await;
    let observed = lock(&retained).take();
    drop(execution);
    let retired_on_drop = observed.as_ref().map(|(invocation, id, input)| {
        invocation
            .take(id, input, std::path::Path::new(&session.cwd()))
            .is_err()
    });
    session.dispose().await;
    admission
        .expect("bounded executor entry")
        .expect("executor entry before settlement");
    assert_eq!(
        retired_on_drop,
        Some(true),
        "caller drop must retire before session disposal"
    );
    let (invocation, id, input) = observed.expect("direct executor entered");
    assert!(
        invocation
            .take(&id, &input, std::path::Path::new(&session.cwd()))
            .is_err()
    );
    assert!(monitor_invocation::CURRENT.try_with(|_| ()).is_err());
}

#[tokio::test]
async fn registered_monitor_attachment_does_not_survive_later_block_or_cancel() {
    for cancelled in [false, true] {
        let session = retry_session(Vec::new(), 0);
        let calls = Arc::new(AtomicU64::new(0));
        let executed = calls.clone();
        let mut tool = test_tool("monitor_fixture");
        tool.execute = Arc::new(move |_, _, _, _| {
            executed.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { AgentToolResult::text("unexpected") })
        });
        session.register_tool_definition(
            test_definition("monitor_fixture"),
            empty_source_info(),
            tool,
        );
        session.set_active_tools_by_name(vec!["monitor_fixture".into()]);
        let controller = maho_ai::utils::abort::AbortController::new();
        let abort = controller.clone();
        let retained = Arc::new(Mutex::new(None));
        let captured = retained.clone();
        let mut extension = maho_ext_api::LoadedExtension::new(
            "<inline:monitor-block>",
            session.cwd().into(),
            Default::default(),
        );
        extension.handlers.insert(
            maho_ext_api::EventKind::ToolCall,
            vec![Arc::new(move |event, context| {
                let maho_ext_api::ExtensionEvent::ToolCall(event) = event else {
                    panic!("tool call");
                };
                context
                    .set_approved_monitor_parent(
                        &event.tool_call_id,
                        &event.input,
                        std::path::Path::new("/approved"),
                    )
                    .expect("attachment");
                *lock(&captured) = Some((
                    monitor_invocation::CURRENT.with(Arc::clone),
                    event.tool_call_id.clone(),
                    event.input.clone(),
                ));
                if cancelled {
                    abort.abort(None);
                }
                Box::pin(async move {
                    Ok(maho_ext_api::EventResult::ToolCall(
                        maho_ext_api::ToolCallEventResult {
                            block: Some(!cancelled),
                            reason: Some("later admission refused".into()),
                            terminate: None,
                        },
                    ))
                })
            })],
        );
        session
            .set_extension_runner(maho_ext_host::ExtensionRunner::new(
                vec![extension],
                Default::default(),
                Default::default(),
                test_extension_context(&session),
            ))
            .await;
        let result = session
            .execute_tool(
                "monitor_fixture",
                serde_json::json!({}),
                ExecuteToolOptions {
                    signal: Some(controller.signal()),
                    ..Default::default()
                },
            )
            .await;
        let (invocation, id, input) = lock(&retained).take().expect("captured private scope");
        session.dispose().await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(
            invocation
                .take(&id, &input, std::path::Path::new(&session.cwd()))
                .is_err()
        );
    }
}

#[tokio::test]
async fn session_context_entry_ids_stamp_projected_messages_only() {
    let session = test_session();
    let entry = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"root","timestamp":0}))
    });
    session.rebuild_session_context().expect("context");
    let mut messages = session.messages();
    messages.push(AgentMessage::Llm(maho_ai::types::Message::User(
        maho_ai::types::UserMessage {
            content: maho_ai::types::UserContent::Text("live".into()),
            timestamp: 1,
        },
    )));
    assert_eq!(
        session.session_context_entry_ids(&messages),
        vec![entry["id"].as_str().map(str::to_owned), None]
    );
}

#[tokio::test]
async fn tree_preparation_hook_observes_navigation_abort() {
    let session = test_session();
    let root = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"root","timestamp":0}))
    });
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"branch","timestamp":1}))
    });
    session.rebuild_session_context().expect("context");
    let leaf = session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned));
    let messages = session.messages();
    let (started, entered) = tokio::sync::oneshot::channel();
    let started = Arc::new(Mutex::new(Some(started)));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:tree-abort>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeTree,
        vec![Arc::new(move |event, _| {
            let started = started.clone();
            Box::pin(async move {
                let maho_ext_api::ExtensionEvent::SessionBeforeTree { signal, .. } = event else {
                    panic!("tree event");
                };
                lock(&started)
                    .take()
                    .expect("single hook invocation")
                    .send(())
                    .expect("observer");
                signal.cancelled().await;
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        cancel: Some(true),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            session.navigate_tree(root["id"].as_str().expect("root"), Default::default()),
            async {
                entered.await.expect("hook started");
                session.abort_branch_summary();
            }
        )
    })
    .await
    .expect("bounded navigation cancellation");
    assert!(result.expect("navigation").cancelled);
    assert_eq!(
        session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)),
        leaf
    );
    assert_eq!(session.messages(), messages);
    assert!(!session.is_compacting());
}

#[tokio::test]
async fn resource_discovery_follows_startup_and_reload_with_builtin_scope() {
    let session = test_session();
    let dir = tempfile::tempdir().expect("directory");
    let prompt = dir.path().join("from-extension.md");
    std::fs::write(
        &prompt,
        "---\ndescription: extension prompt\n---\noriginal $1",
    )
    .expect("prompt fixture");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<builtin:resources>",
        session.cwd().into(),
        maho_ext_api::SourceInfo {
            scope: maho_ext_api::SourceScope::System,
            ..Default::default()
        },
    );
    let started = seen.clone();
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |event, _| {
            if let maho_ext_api::ExtensionEvent::SessionStart(event) = event {
                lock(&started).push((maho_ext_api::EventKind::SessionStart, event.reason));
            }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    let discovered = seen.clone();
    let path = prompt.to_string_lossy().into_owned();
    extension.handlers.insert(
        maho_ext_api::EventKind::ResourcesDiscover,
        vec![Arc::new(move |event, _| {
            let path = path.clone();
            if let maho_ext_api::ExtensionEvent::ResourcesDiscover(event) = event {
                lock(&discovered).push((maho_ext_api::EventKind::ResourcesDiscover, event.reason));
            }
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::ResourcesDiscover(
                    maho_ext_api::ResourcesDiscoverResult {
                        prompt_paths: vec![path.into()],
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    let retained_extension = extension.clone();
    let mut runner = maho_ext_host::ExtensionRunner::new(
        vec![extension],
        Default::default(),
        Default::default(),
        test_extension_context(&session),
    );
    runner.set_runtime_factory(Arc::new(move |context| {
        let extension = retained_extension.clone();
        Box::pin(async move {
            Ok(maho_ext_host::ExtensionRunner::new(
                vec![extension],
                Default::default(),
                Default::default(),
                context,
            ))
        })
    }));
    session.set_extension_runner(runner).await;
    session.bind_extensions(Default::default()).await;
    assert_eq!(
        session.prompt_templates()[0].source_info.scope,
        crate::source_info::SourceScope::System
    );
    assert_eq!(
        session
            .expand_input("/from-extension input", true)
            .expect("expansion"),
        "original input"
    );
    std::fs::write(
        &prompt,
        "---\ndescription: extension prompt\n---\nreloaded $1",
    )
    .expect("updated fixture");
    session.reload().await.expect("reload");
    assert_eq!(
        session
            .expand_input("/from-extension input", true)
            .expect("reloaded expansion"),
        "reloaded input"
    );
    assert_eq!(
        *lock(&seen),
        [
            (
                maho_ext_api::EventKind::SessionStart,
                maho_ext_api::SessionReason::Startup
            ),
            (
                maho_ext_api::EventKind::ResourcesDiscover,
                maho_ext_api::SessionReason::Startup
            ),
            (
                maho_ext_api::EventKind::SessionStart,
                maho_ext_api::SessionReason::Reload
            ),
            (
                maho_ext_api::EventKind::ResourcesDiscover,
                maho_ext_api::SessionReason::Reload
            ),
        ]
    );
}

#[tokio::test]
async fn switch_rebinds_cwd_resources_and_refuses_a_missing_cwd() {
    let dir = crate::test_support::isolated_tempdir();
    let mut cwds = Vec::new();
    for name in ["first", "second"] {
        let skill = dir
            .path()
            .join(name)
            .join(crate::config::config_dir_name())
            .join("skills")
            .join(format!("{name}-skill"));
        std::fs::create_dir_all(&skill).expect("skill dir");
        std::fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}-skill\ndescription: {name}\n---\nbody"),
        )
        .expect("skill");
        cwds.push(dir.path().join(name).to_string_lossy().into_owned());
    }
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture","name":"fixture","api":"faux","provider":"faux","baseUrl":"",
        "reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    }))
    .expect("model");
    let session = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
        cwd: Some(cwds[0].clone()),
        agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model),
        session_manager: Some(SessionManager::in_memory(&cwds[0], None, None)),
        tools: Some(Vec::new()),
        ..Default::default()
    })
    .await
    .expect("session")
    .session;
    // Only this test's fixture skills, each with its project scope: host user skills are outside it.
    let skill_names = |session: &AgentSession| {
        session
            .loaded_skills()
            .expect("skills")
            .skills
            .into_iter()
            .filter(|skill| ["first-skill", "second-skill"].contains(&skill.name.as_str()))
            .map(|skill| (skill.name, skill.source_info.scope))
            .collect::<Vec<_>>()
    };
    let project = |name: &str| vec![(name.to_owned(), crate::source_info::SourceScope::Project)];
    assert_eq!(skill_names(&session), project("first-skill"));
    let header = |id: &str, cwd: &str| {
        format!(
            "{}\n",
            serde_json::json!({"type":"session","version":3,"id":id,"timestamp":"2026-01-01T00:00:00Z","cwd":cwd})
        )
    };
    let gone = dir.path().join("gone").to_string_lossy().into_owned();
    let missing = dir.path().join("missing.jsonl");
    std::fs::write(&missing, header("missing-cwd", &gone)).expect("missing header");
    let failure = session
        .switch_session_with_cwd_coded(&missing.to_string_lossy(), None)
        .await
        .expect_err("missing cwd");
    assert_eq!(
        failure.missing_cwd.expect("typed error").issue.session_cwd,
        gone
    );
    assert_eq!(session.cwd(), cwds[0]);
    let target = dir.path().join("second.jsonl");
    std::fs::write(&target, header("second-cwd", &cwds[1])).expect("second header");
    assert!(
        session
            .switch_session(&target.to_string_lossy())
            .await
            .expect("switch")
    );
    assert_eq!(session.cwd(), cwds[1]);
    assert_eq!(skill_names(&session), project("second-skill"));
    session.dispose().await;
}

type AskedPrompts = Arc<Mutex<Vec<(String, Vec<String>)>>>;

/// A UI whose `select` answers with a fixed label and records each prompt it was shown.
struct AnsweringUi {
    answer: Option<&'static str>,
    asked: AskedPrompts,
}
impl maho_ext_api::ExtensionUi for AnsweringUi {
    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        lock(&self.asked).push((title.to_owned(), options.to_vec()));
        let answer = self.answer.map(str::to_owned);
        Box::pin(async move { answer })
    }
    fn confirm<'a>(
        &'a self,
        title: &'a str,
        message: &'a str,
        options: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, bool> {
        TestExtensionUi.confirm(title, message, options)
    }
    fn input<'a>(
        &'a self,
        title: &'a str,
        placeholder: Option<&'a str>,
        options: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        TestExtensionUi.input(title, placeholder, options)
    }
    fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(
        &self,
        _: &str,
        _: Option<maho_ext_api::WidgetContent>,
        _: maho_ext_api::ExtensionWidgetOptions,
    ) {
    }
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        factory: maho_ext_api::ComponentFactory,
        options: maho_ext_api::CustomUiOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, Value> {
        TestExtensionUi.custom(factory, options)
    }
    fn theme(&self) -> maho_ext_api::Theme {
        Default::default()
    }
}

/// Two projects under a fixture-owned temp root: `trusted` (saved "trust") where the session starts, and
/// `other` with a project skill and no decision, plus a stored session of `other` to switch into.
struct SwitchTrustFixture {
    _dir: tempfile::TempDir,
    agent_dir: String,
    other: String,
    other_session: String,
    session: AgentSession,
}

async fn switch_trust_fixture() -> SwitchTrustFixture {
    let dir = crate::test_support::isolated_tempdir();
    let agent_dir = dir.path().join("agent").to_string_lossy().into_owned();
    let start = dir.path().join("start");
    std::fs::create_dir_all(&start).expect("start dir");
    let other = dir.path().join("other");
    let skill = other
        .join(crate::config::config_dir_name())
        .join("skills")
        .join("other-skill");
    std::fs::create_dir_all(&skill).expect("skill dir");
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: other-skill\ndescription: probe\n---\nbody",
    )
    .expect("skill");
    let start = start.to_string_lossy().into_owned();
    let other = other.to_string_lossy().into_owned();
    let other_session = dir.path().join("other.jsonl");
    std::fs::write(
        &other_session,
        format!(
            "{}\n",
            serde_json::json!({"type":"session","version":3,"id":"other-trust","timestamp":"2026-01-01T00:00:00Z","cwd":other})
        ),
    )
    .expect("other session");
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture","name":"fixture","api":"faux","provider":"faux","baseUrl":"",
        "reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    }))
    .expect("model");
    let session = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
        cwd: Some(start.clone()),
        agent_dir: Some(agent_dir.clone()),
        model: Some(model),
        session_manager: Some(SessionManager::in_memory(&start, None, None)),
        tools: Some(Vec::new()),
        ..Default::default()
    })
    .await
    .expect("session")
    .session;
    session.load_resources().await.expect("resources");
    SwitchTrustFixture {
        _dir: dir,
        agent_dir,
        other,
        other_session: other_session.to_string_lossy().into_owned(),
        session,
    }
}

impl SwitchTrustFixture {
    fn bind_ui(&self, answer: Option<&'static str>) -> AskedPrompts {
        let asked = Arc::new(Mutex::new(Vec::new()));
        self.session.state().extension_ui_context = Some(Arc::new(AnsweringUi {
            answer,
            asked: asked.clone(),
        }));
        asked
    }

    async fn switch_into_other(&self) {
        assert!(
            self.session
                .switch_session(&self.other_session)
                .await
                .expect("switch")
        );
        assert_eq!(self.session.cwd(), self.other);
    }

    fn trusted(&self) -> bool {
        self.session
            .with_settings_manager(|settings| settings.is_project_trusted())
    }

    fn has_other_skill(&self) -> bool {
        self.session.loaded_skills().is_some_and(|loaded| {
            loaded
                .skills
                .iter()
                .any(|skill| skill.name == "other-skill")
        })
    }

    fn store(&self) -> crate::trust_manager::ProjectTrustStore {
        crate::trust_manager::ProjectTrustStore::new(&self.agent_dir)
    }
}

// senpi interactive `switchSession({ projectTrustContextFactory })`: a project nothing decided is asked with
// the launch prompt's text and choices; a session-only answer trusts it for this host only (cache), and its
// project code loads.
#[tokio::test]
async fn a_switch_into_an_undecided_project_asks_and_applies_the_answer() {
    let fixture = switch_trust_fixture().await;
    let cache: crate::agent_session_runtime::ProjectTrustCache = Arc::default();
    fixture
        .session
        .set_switch_project_trust(Some(SwitchProjectTrust {
            cache: Some(cache.clone()),
            prompt: true,
            ..Default::default()
        }));
    let asked = fixture.bind_ui(Some("Trust (this session only)"));

    fixture.switch_into_other().await;

    let asked = lock(&asked).clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].0,
        crate::project_trust::format_project_trust_prompt(&fixture.other)
    );
    let expected: Vec<String> =
        crate::trust_manager::get_project_trust_options(&fixture.other, true)
            .into_iter()
            .map(|option| option.label)
            .collect();
    assert_eq!(asked[0].1, expected);
    assert!(fixture.trusted());
    assert!(fixture.has_other_skill());
    assert_eq!(lock(&cache).get(&fixture.other).copied(), Some(true));
    assert_eq!(fixture.store().get(&fixture.other), Ok(None));
    fixture.session.dispose().await;
}

#[tokio::test]
async fn a_saved_answer_to_the_switch_prompt_is_written_to_the_trust_store() {
    let fixture = switch_trust_fixture().await;
    fixture
        .session
        .set_switch_project_trust(Some(SwitchProjectTrust {
            prompt: true,
            ..Default::default()
        }));
    fixture.bind_ui(Some("Do not trust"));

    fixture.switch_into_other().await;

    assert!(!fixture.trusted());
    assert!(!fixture.has_other_skill());
    assert_eq!(fixture.store().get(&fixture.other), Ok(Some(false)));
    fixture.session.dispose().await;
}

#[tokio::test]
async fn a_cached_decision_is_applied_without_asking() {
    let fixture = switch_trust_fixture().await;
    let cache: crate::agent_session_runtime::ProjectTrustCache = Arc::default();
    lock(&cache).insert(fixture.other.clone(), true);
    fixture
        .session
        .set_switch_project_trust(Some(SwitchProjectTrust {
            cache: Some(cache),
            prompt: true,
            ..Default::default()
        }));
    let asked = fixture.bind_ui(Some("Do not trust"));

    fixture.switch_into_other().await;

    assert!(lock(&asked).is_empty());
    assert!(fixture.trusted());
    assert!(fixture.has_other_skill());
    fixture.session.dispose().await;
}

#[tokio::test]
async fn a_headless_switch_into_an_undecided_project_stays_untrusted() {
    let fixture = switch_trust_fixture().await;
    // The host would allow a prompt, but the session has no UI to ask through.
    fixture
        .session
        .set_switch_project_trust(Some(SwitchProjectTrust {
            prompt: true,
            ..Default::default()
        }));
    assert!(fixture.session.state().extension_ui_context.is_none());

    fixture.switch_into_other().await;

    assert!(!fixture.trusted());
    assert!(!fixture.has_other_skill());
    assert_eq!(fixture.store().get(&fixture.other), Ok(None));
    fixture.session.dispose().await;
}

// senpi `shouldResolveProjectTrust` ignores the saved decision: the extension `project_trust` event runs first,
// so an extension that answers "no" beats a saved "trust". In the interactive mode the handlers get the
// session's UI (senpi `projectTrustContextFactory`, `hasUI: true`).
#[tokio::test]
async fn the_extension_event_runs_before_a_saved_trust_decision() {
    let fixture = switch_trust_fixture().await;
    fixture
        .store()
        .set(&fixture.other, Some(true))
        .expect("save trust");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let resolver: crate::agent_session_runtime::ProjectTrustResolver = Arc::new(
        move |cwd: String, _: Option<String>, ui: Option<Arc<dyn ExtensionUi>>| {
            lock(&seen).push((cwd, ui.is_some()));
            Box::pin(async {
                crate::agent_session_runtime::ResolvedProjectTrust {
                    trusted: false,
                    decided: true,
                    ..Default::default()
                }
            }) as crate::agent_session_runtime::ProjectTrustResolution
        },
    );
    fixture
        .session
        .set_switch_project_trust(Some(SwitchProjectTrust {
            resolver: Some(resolver),
            prompt: true,
            ..Default::default()
        }));
    let asked = fixture.bind_ui(Some("Trust"));

    fixture.switch_into_other().await;

    assert_eq!(*lock(&calls), [(fixture.other.clone(), true)]);
    assert!(lock(&asked).is_empty());
    assert!(!fixture.trusted());
    assert!(!fixture.has_other_skill());
    fixture.session.dispose().await;
}

#[tokio::test]
async fn the_host_resolution_decides_before_the_prompt() {
    let fixture = switch_trust_fixture().await;
    let resolver: crate::agent_session_runtime::ProjectTrustResolver =
        Arc::new(|_: String, _: Option<String>, _| {
            Box::pin(async {
                crate::agent_session_runtime::ResolvedProjectTrust {
                    trusted: true,
                    decided: true,
                    ..Default::default()
                }
            }) as crate::agent_session_runtime::ProjectTrustResolution
        });
    fixture
        .session
        .set_switch_project_trust(Some(SwitchProjectTrust {
            resolver: Some(resolver),
            prompt: true,
            ..Default::default()
        }));
    let asked = fixture.bind_ui(Some("Do not trust"));

    fixture.switch_into_other().await;

    assert!(lock(&asked).is_empty());
    assert!(fixture.trusted());
    assert!(fixture.has_other_skill());
    fixture.session.dispose().await;
}

#[tokio::test]
async fn exact_tree_resume_retains_hook_metadata_off_conversation_tail() {
    let session = test_session();
    let root = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"root","timestamp":0}))
    });
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"branch","timestamp":1}))
    });
    session.rebuild_session_context().expect("context");
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:tree-metadata>",
        session.cwd().into(),
        Default::default(),
    );
    let captured = session.clone();
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionTree,
        vec![Arc::new(move |_, _| {
            captured.with_session_manager_mut(|manager| {
                manager.append_custom("tree-metadata", Some(serde_json::json!({"retained":true})))
            });
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    session
        .navigate_tree(
            root["id"].as_str().expect("root"),
            TreeNavigationOptions {
                intent: Some(TreeNavigationIntent::Resume),
                ..Default::default()
            },
        )
        .await
        .expect("resume");
    assert_eq!(
        session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)),
        root["id"].as_str().map(str::to_owned)
    );
    assert_eq!(session.messages().len(), 1);
    assert!(
        session
            .with_session_manager(|manager| manager.entries())
            .iter()
            .any(|entry| entry["customType"] == "tree-metadata")
    );
}

#[test]
fn eval_helper_calls_use_the_argument_name_the_tool_takes() {
    assert_eq!(eval_helper_call("bash"), "tool.bash({ command: \"...\" })");
    assert_eq!(
        eval_helper_call("grep"),
        "tool.grep({ pattern: \"...\", path: \"...\" })"
    );
    assert_eq!(
        eval_helper_call("workflow"),
        "tool.workflow({ action: \"...\" })"
    );
    assert_eq!(
        eval_helper_call("monitor"),
        "tool.monitor({ description: \"...\", command: \"...\", filter: \"...\" })"
    );
    assert_eq!(eval_helper_call("read"), "tool.read({ ... })");
}

#[test]
fn watchdog_message_notes_the_stream_start_guard() {
    assert!(
        provider_retry_watchdog_abort_message(Some(30_000), None)
            .contains("stream-start guard disabled")
    );
    assert!(
        provider_retry_watchdog_abort_message(Some(30_000), Some(5_000))
            .contains("stream-start guard: 5000ms")
    );
}

#[test]
fn every_rejection_cause_has_a_message() {
    for cause in [
        CompactionRejectionCause::WouldOverflow,
        CompactionRejectionCause::CancelledByExtension,
        CompactionRejectionCause::ExternalOwner,
        CompactionRejectionCause::CircuitBreaker,
        CompactionRejectionCause::PerTurnCap,
        CompactionRejectionCause::StaleRevision,
    ] {
        assert!(describe_compaction_rejection(cause).starts_with("Compaction rejected"));
    }
}

#[test]
fn fast_mode_promotes_the_effective_tier() {
    assert_eq!(resolve_effective_service_tier(None, false), None);
    assert_eq!(
        resolve_effective_service_tier(Some(ServiceTier::Flex), false),
        Some(ServiceTier::Flex)
    );
    assert_eq!(
        resolve_effective_service_tier(None, true),
        Some(ServiceTier::Priority)
    );
}

#[test]
fn a_scoped_entry_tier_wins_over_the_model_preference() {
    let mut model = test_model();
    model.service_tier = Some(ServiceTierPreference::Flex);
    assert_eq!(
        resolve_service_tier(&model, Some(ServiceTier::Priority)),
        Some(ServiceTier::Priority)
    );
    assert_eq!(resolve_service_tier(&model, None), Some(ServiceTier::Flex));
}

#[test]
fn a_null_header_value_is_dropped() {
    let headers: ProviderHeaders = [
        ("Authorization".to_owned(), Some("Bearer x".to_owned())),
        ("X-Deleted".to_owned(), None),
    ]
    .into_iter()
    .collect();
    let filtered = without_deleted_headers(Some(headers)).expect("headers");
    assert_eq!(filtered.len(), 1);
    assert_eq!(
        filtered.get("Authorization").map(String::as_str),
        Some("Bearer x")
    );
}

fn stub_agent() -> Agent {
    let stream_fn: maho_agent::types::StreamFn =
        Arc::new(|_, _, _| maho_ai::types::AssistantMessageEventStream::assistant());
    Agent::new(maho_agent::AgentOptions {
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
}

/// Summaries run through `agent.streamFunction`; route it to the session's model runtime.
fn route_agent_stream_to_model_runtime(session: &AgentSession) {
    let runtime = session.model_runtime().clone();
    session
        .agent
        .set_stream_function(Arc::new(move |model, context, options| {
            runtime.stream_simple(model, context, options.map(|options| options.simple))
        }));
}

/// senpi test/suite/harness.ts:150-175: a session whose faux provider carries a configured key, so
/// `checkAuth` admits model switches (agent-session.ts:5148).
fn keyed_test_session() -> AgentSession {
    let session = test_session_with_stream_function(false);
    let mut runtime = session.model_runtime().clone();
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("configured faux provider");
    session
}

#[tokio::test]
async fn held_input_is_prepared_but_not_consumed_before_settlement() {
    let session = test_session();
    session
        .hold_input(json!({"inputId":"held-a","revision":0,"text":"first"}))
        .expect("hold");
    session
        .hold_input(json!({"inputId":"held-b","revision":0,"text":"second"}))
        .expect("hold");
    let (messages, ids) = session.take_held_input_messages().await.expect("prepare");
    assert_eq!(ids, vec!["held-a", "held-b"]);
    assert_eq!(messages.len(), 2);
    assert_eq!(session.held_inputs().len(), 2);
    session.consume_held_inputs(&ids).expect("settle");
    assert!(session.held_inputs().is_empty());
}

#[tokio::test]
async fn before_prompt_messages_land_before_the_prompt_and_others_append() {
    let session = test_session();
    let custom = |custom_type: &str| {
        session_message_from_value(json!({"role":"custom","customType":custom_type,"content":custom_type,"display":false,"timestamp":0}))
            .expect("custom message")
    };
    let ext = |custom_type: &str| maho_ext_api::CustomMessage {
        custom_type: custom_type.into(),
        content: vec![maho_tools::definition::ToolContent::text(custom_type)],
        display: false,
        details: None,
    };
    // Layout built by `prompt`: held inputs, then the prompt, then consumed next-turn messages.
    let mut messages = vec![
        custom("held"),
        make_user_message("prompt", None),
        custom("consumed"),
    ];
    session
        .apply_before_agent_start_result(
            &mut messages,
            Some(maho_ext_api::BeforeAgentStartCombinedResult {
                before_prompt_messages: vec![ext("A")],
                messages: vec![ext("B")],
                ..Default::default()
            }),
            1,
        )
        .expect("apply");
    let order = messages
        .iter()
        .map(|message| {
            let value = session_message_to_value(message).expect("value");
            value["customType"]
                .as_str()
                .unwrap_or_else(|| value["role"].as_str().expect("role"))
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(order, ["held", "A", "user", "consumed", "B"]);
}

#[tokio::test]
async fn claimed_held_input_does_not_ride_two_turns() {
    let session = test_session();
    session
        .hold_input(json!({"inputId":"held-a","revision":0,"text":"first"}))
        .expect("hold");
    lock(&session.delivered_held_inputs).push(("held-a".into(), 0));
    let (messages, ids) = session.take_held_input_messages().await.expect("prepare");
    assert!(messages.is_empty());
    assert!(ids.is_empty());
    assert_eq!(session.held_inputs().len(), 1);
}

#[tokio::test]
async fn concurrent_held_preparations_reserve_each_input_once() {
    let session = test_session();
    session
        .hold_input(json!({"inputId":"one","revision":0,"text":"one"}))
        .expect("hold");
    let (first, second) = tokio::join!(
        session.take_held_input_messages(),
        session.take_held_input_messages()
    );
    let (_, a) = first.expect("first");
    let (_, b) = second.expect("second");
    assert_eq!(a.len() + b.len(), 1);
    let ids = if a.is_empty() { b } else { a };
    drop(HeldInputReservation {
        session: session.clone(),
        ids,
    });
    assert_eq!(
        session
            .take_held_input_messages()
            .await
            .expect("retry")
            .1
            .len(),
        1
    );
}

#[tokio::test]
async fn reentrant_held_command_preparation_does_not_take_sibling_input() {
    let session = test_session();
    session
        .hold_input(json!({"inputId":"one","revision":0,"text":"one"}))
        .expect("hold");
    let (messages, ids) = HELD_COMMAND_SESSION
        .scope(session.session_id(), session.take_held_input_messages())
        .await
        .expect("reentrant");
    assert!(messages.is_empty());
    assert!(ids.is_empty());
    assert_eq!(session.held_inputs().len(), 1);
}

#[test]
fn custom_read_effect_overrides_conservative_barrier_default() {
    let session = test_session();
    let mut definition = test_definition("custom-reader");
    definition.workspace_effect = Some(crate::session_work_barrier::ToolWorkspaceEffect::Read);
    session.register_tool_definition(definition, empty_source_info(), test_tool("custom-reader"));
    assert_eq!(
        session.get_tool_workspace_effect("custom-reader"),
        crate::session_work_barrier::ToolWorkspaceEffect::Read
    );
    assert_eq!(
        session.get_tool_workspace_effect("unregistered"),
        crate::session_work_barrier::ToolWorkspaceEffect::Write
    );
}

#[tokio::test]
async fn invalid_tool_barrier_is_rejected_before_arming() {
    let session = test_session();
    assert!(
        session
            .arm_tool_barrier(crate::session_work_barrier::ToolBarrierSpec {
                id: String::new(),
                deadline_ms: 1
            })
            .is_err()
    );
    assert!(session.tool_barrier.id().is_none());
}

#[tokio::test]
async fn held_input_request_replay_keeps_one_custody_record() {
    let session = test_session();
    let input = json!({"inputId":"request-id","revision":1,"text":"prepared"});
    let first = session.hold_input(input.clone()).expect("first");
    let second = session.hold_input(input).expect("replay");
    assert_eq!(first["entryId"], second["entryId"]);
    assert_eq!(second["duplicate"], true);
    assert_eq!(session.held_inputs().len(), 1);
}

#[tokio::test]
async fn run_resume_cutoff_cannot_release_a_newer_stop_or_another_parent() {
    let session = test_session();
    let run = crate::run_holds::RunRef {
        kind: "task".into(),
        id: "child".into(),
    };
    let receipt = session
        .pause_for_run("parent", 3, run.clone())
        .await
        .unwrap();
    let crate::run_holds::InheritResult::Inherited { child_seq } = receipt else {
        panic!("inherit refused")
    };
    session.with_run_holds(|holds| holds.stamp());
    assert_eq!(
        session.resume_for_run("other", 3, child_seq).await.unwrap(),
        crate::run_holds::ReleaseInheritedResult::WrongParent
    );
    assert_eq!(
        session
            .resume_for_run("parent", 2, child_seq)
            .await
            .unwrap(),
        crate::run_holds::ReleaseInheritedResult::Stale
    );
    assert_eq!(
        session
            .resume_for_run("parent", 3, child_seq)
            .await
            .unwrap(),
        crate::run_holds::ReleaseInheritedResult::Released {
            other_hold_live: true
        }
    );
    assert!(session.paused());
    assert_eq!(
        session
            .resume_for_run("parent", 3, child_seq)
            .await
            .unwrap(),
        crate::run_holds::ReleaseInheritedResult::NotHeld
    );
    let latest = session.pause_for_run("parent", 4, run).await.unwrap();
    let crate::run_holds::InheritResult::Inherited { child_seq } = latest else {
        panic!("inherit refused")
    };
    assert_eq!(
        session
            .resume_for_run("parent", 4, child_seq)
            .await
            .unwrap(),
        crate::run_holds::ReleaseInheritedResult::Released {
            other_hold_live: false
        }
    );
    assert!(!session.paused());
}

fn test_session() -> AgentSession {
    test_session_with_stream_function(true)
}

struct Recorder(std::sync::Mutex<Vec<String>>);
impl crate::lifecycle_session_binding::LifecycleBindingHost for Recorder {
    fn event(&self, name: &str, _: Value) {
        self.0.lock().unwrap().push(name.to_owned());
    }
    fn facts_failed(&self, _: String) {}
}
/// senpi test/suite/regressions/4149-disposed-binding.test.ts: a disposed binding writes and emits nothing.
#[test]
fn a_disposed_binding_writes_and_emits_nothing() {
    let session = Arc::new(test_session());
    let host = Arc::new(Recorder(std::sync::Mutex::new(Vec::new())));
    let mut binding =
        crate::lifecycle_session_binding::LifecycleSessionBinding::new(session, host.clone());
    binding.start_run();
    binding.dispose();
    binding.start_run();
    binding.idle();
    binding.refresh_facts(Value::Null);
    assert_eq!(*host.0.lock().unwrap(), vec!["start_run".to_owned()]);
    assert!(
        binding
            .consume(&json!({"role":"custom","details":{"ticketId":"t"}}), "r")
            .is_none()
    );
}

struct CustodyProbe(std::sync::Mutex<Vec<(bool, bool)>>);
impl crate::lifecycle_session_binding::LifecycleBindingHost for CustodyProbe {
    fn event(&self, _: &str, _: Value) {}
    fn facts_failed(&self, _: String) {}
    fn mark_interrupted_from_custody(&self, delivered: &dyn Fn(&str) -> bool) {
        self.0
            .lock()
            .unwrap()
            .push((delivered("t1"), delivered("t2")));
    }
}
/// senpi#4204: binding restore asks the controller to mark consumed custody interrupted, once, with the
/// transcript's delivered tickets.
#[tokio::test]
async fn restore_marks_interrupted_from_custody_with_delivered_tickets() {
    let session = Arc::new(test_session());
    session.with_session_manager_mut(|manager| {
        manager.append_custom_message("wake", json!("hi"), false, Some(json!({"ticketId":"t1"})));
    });
    let host = Arc::new(CustodyProbe(std::sync::Mutex::new(Vec::new())));
    let mut binding =
        crate::lifecycle_session_binding::LifecycleSessionBinding::new(session, host.clone());
    binding.restore().await.expect("restore");
    binding.restore().await.expect("second restore is a no-op");
    assert_eq!(*host.0.lock().unwrap(), vec![(true, false)]);
}

fn test_session_with_manager(manager: SessionManager) -> AgentSession {
    let runtime = ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()),
        ..Default::default()
    })
    .expect("valid model runtime");
    AgentSession::new(AgentSessionConfig {
        agent: stub_agent(),
        session_manager: manager,
        settings_manager: SettingsManager::from_storage(
            Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
            false,
        ),
        cwd: "/tmp".to_owned(),
        agent_dir: Some("/tmp/maho-agent".to_owned()),
        fallback_now: None,
        retry_random: None,
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: BTreeMap::new(),
        custom_tools: Vec::new(),
        model_runtime: Some(runtime),
        model_registry: None,
        uses_default_stream_function: Some(true),
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: None,
    })
    .expect("session")
}

#[test]
fn sdk_extension_context_binds_goal_store_file_to_the_encoded_session_directory_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path().to_str().expect("utf8 dir").to_owned();
    let session = test_session_with_manager(SessionManager::create("/workspace", Some(&dir), None));
    let id = session.session_id();
    assert!(!id.is_empty(), "a created session has an id");
    let context = session.extension_context(Arc::new(TestExtensionUi));
    let expected = temp.path().join("extensions/goal").join(format!(
        "{}.json",
        crate::sdk::extension_context::encode_uri_component(&id)
    ));
    assert_eq!(context.goal_store_file.as_deref(), Some(expected.as_path()));
}

fn raw_first_user_text(session: &AgentSession) -> String {
    session
        .agent
        .state()
        .messages()
        .first()
        .map(user_message_text)
        .unwrap_or_default()
}

#[test]
fn excluded_names_and_prefix_patterns_are_withheld_and_recorded() {
    let session = AgentSession::new(AgentSessionConfig {
        agent: stub_agent(),
        session_manager: SessionManager::in_memory("/tmp", None, None),
        settings_manager: SettingsManager::from_storage(
            Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
            false,
        ),
        cwd: "/tmp".to_owned(),
        agent_dir: Some("/tmp/maho-agent".to_owned()),
        fallback_now: None,
        retry_random: None,
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: BTreeMap::new(),
        custom_tools: Vec::new(),
        model_runtime: Some(
            ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
                providers: Some(Vec::new()),
                ..Default::default()
            })
            .expect("valid model runtime"),
        ),
        model_registry: None,
        uses_default_stream_function: Some(true),
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: Some(vec!["bash".to_owned(), "task_*".to_owned()]),
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: None,
    })
    .expect("session");
    for name in ["bash", "task_send", "tasks", "read"] {
        session.register_tool_definition(
            test_definition(name),
            empty_source_info(),
            test_tool(name),
        );
    }
    let registered = session
        .get_all_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        registered,
        BTreeSet::from(["read".to_owned(), "tasks".to_owned()])
    );
    assert_eq!(
        session.withheld_excluded_tool_names(),
        ["bash", "task_send"]
    );
}

fn bash_excluded_session(temp: &tempfile::TempDir) -> AgentSession {
    let cwd = temp.path().to_string_lossy().into_owned();
    AgentSession::new(AgentSessionConfig {
        agent: stub_agent(),
        session_manager: SessionManager::in_memory(&cwd, None, None),
        settings_manager: SettingsManager::from_storage(
            Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
            false,
        ),
        cwd: cwd.clone(),
        agent_dir: Some(temp.path().join("agent").to_string_lossy().into_owned()),
        fallback_now: None,
        retry_random: None,
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: BTreeMap::new(),
        custom_tools: Vec::new(),
        model_runtime: Some(
            ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
                providers: Some(Vec::new()),
                ..Default::default()
            })
            .expect("valid model runtime"),
        ),
        model_registry: None,
        uses_default_stream_function: Some(true),
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: Some(vec!["bash".to_owned()]),
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: None,
    })
    .expect("session")
}

#[tokio::test]
async fn a_restrictive_role_refuses_user_bash_before_any_shell_runs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = bash_excluded_session(&temp);
    session.set_user_bash_denied(true);
    let Err(refused) = session
        .execute_user_bash("touch user-bash-ran", None, true, None)
        .await
    else {
        panic!("user bash ran in a restrictive-role session");
    };
    assert!(refused.starts_with("bash_excluded: "), "{refused}");
    assert!(!temp.path().join("user-bash-ran").exists(), "no shell ran");
}

#[tokio::test]
async fn plain_bash_exclusion_keeps_user_bash() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = bash_excluded_session(&temp);
    let hook_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:user-bash>",
        session.cwd().into(),
        Default::default(),
    );
    let runs = hook_runs.clone();
    extension.handlers.insert(
        maho_ext_api::EventKind::UserBash,
        vec![Arc::new(move |_, _| {
            runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async {
                Ok(maho_ext_api::EventResult::UserBash {
                    operations: None,
                    result: Some(maho_tools::bash_executor::BashResult {
                        output: "user bash ran".into(),
                        exit_code: Some(0),
                        cancelled: false,
                        truncated: false,
                        full_output_path: None,
                    }),
                })
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    // `--exclude-tools bash` / `tools_exclude=bash` hide bash from the model, not from the user.
    let result = session.execute_user_bash("echo hi", None, true, None).await;
    assert_eq!(
        result.map(|result| result.output),
        Ok("user bash ran".to_owned()),
        "a plain bash exclusion keeps `!cmd`"
    );
    assert_eq!(hook_runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    // Control: the host's restrictive flag refuses before the user-bash path runs.
    session.set_user_bash_denied(true);
    let Err(refused) = session.execute_user_bash("echo hi", None, true, None).await else {
        panic!("a restrictive role must refuse user bash");
    };
    assert!(refused.starts_with("bash_excluded: "), "{refused}");
    assert_eq!(
        hook_runs.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the refusal precedes the user-bash hook"
    );
    session.dispose().await;
}

#[test]
fn idle_release_tokenizes_spilled_runtime_strings_and_the_next_read_rehydrates_them() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path().to_str().expect("utf8 dir").to_owned();
    let session = test_session_with_manager(SessionManager::create("/workspace", Some(&dir), None));
    let large = "x".repeat(40 * 1024);
    session
        .agent
        .set_messages(vec![make_user_message(&large, None)]);
    let store = session.with_session_manager(|manager| manager.resident_store().clone());
    store.externalize_string(&large);
    store.spill_resident();
    assert!(store.stats().evicted_count > 0);

    session.release_idle_runtime_messages();
    assert!(
        raw_first_user_text(&session)
            .starts_with(crate::session_resident_store::RESIDENT_STRING_PREFIX)
    );

    assert_eq!(user_message_text(&session.messages()[0]), large);
    assert!(
        raw_first_user_text(&session)
            .starts_with(crate::session_resident_store::RESIDENT_STRING_PREFIX),
        "an idle read must hydrate a view without re-inflating the parked runtime transcript"
    );
    let _ = session.get_context_usage();
    assert!(
        raw_first_user_text(&session)
            .starts_with(crate::session_resident_store::RESIDENT_STRING_PREFIX)
    );

    session.agent_run_active.store(true, Ordering::SeqCst);
    session.hydrate_runtime_messages_for_run();
    assert_eq!(raw_first_user_text(&session), large);
}

#[test]
fn idle_release_keeps_runtime_strings_when_nothing_was_spilled() {
    let session = test_session();
    let large = "y".repeat(40 * 1024);
    session
        .agent
        .set_messages(vec![make_user_message(&large, None)]);
    session.release_idle_runtime_messages();
    assert_eq!(raw_first_user_text(&session), large);
}

#[test]
fn park_idle_child_memory_spills_resident_store_and_rehydrates_on_read() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path().to_str().expect("utf8 dir").to_owned();
    let mut manager = SessionManager::create("/workspace", Some(&dir), None);
    let large_user = "u".repeat(48 * 1024);
    let large_reply = "a".repeat(24 * 1024);
    manager.append_message(serde_json::json!({
        "role": "user",
        "content": [{ "type": "text", "text": large_user }],
        "timestamp": 1
    }));
    manager.append_message(serde_json::json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": large_reply }],
        "api": "faux",
        "provider": "faux",
        "model": "faux-1",
        "usage": maho_ai::types::Usage::default(),
        "stopReason": "stop",
        "timestamp": 2
    }));
    let session = test_session_with_manager(manager);
    session.rebuild_session_context().expect("rebuild context");
    assert_eq!(session.agent.state().messages().len(), 2);

    session.park_idle_child_memory();

    let stats = session.with_session_manager(|m| m.resident_store_stats());
    assert_eq!(stats.blob_bytes, 0);
    assert!(stats.evicted_count >= 2);
    assert!(session.agent.state().messages().is_empty());
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some(large_reply.as_str())
    );
    let rehydrated = session.messages();
    assert_eq!(rehydrated.len(), 2);
    assert_eq!(user_message_text(&rehydrated[0]), large_user);
}

#[test]
fn idle_release_never_restores_messages_into_a_child_parked_mid_release() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path().to_str().expect("utf8 dir").to_owned();
    let mut manager = SessionManager::create("/workspace", Some(&dir), None);
    let large_user = "u".repeat(48 * 1024);
    let large_reply = "a".repeat(24 * 1024);
    manager.append_message(serde_json::json!({
        "role": "user",
        "content": [{ "type": "text", "text": large_user }],
        "timestamp": 1
    }));
    manager.append_message(serde_json::json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": large_reply }],
        "api": "faux",
        "provider": "faux",
        "model": "faux-1",
        "usage": maho_ai::types::Usage::default(),
        "stopReason": "stop",
        "timestamp": 2
    }));
    let session = test_session_with_manager(manager);
    session.rebuild_session_context().expect("rebuild context");
    assert_eq!(session.agent.state().messages().len(), 2);

    // Force a park into the window between the release's transcript read and its commit.
    let parked_mid_release = std::rc::Rc::new(std::cell::Cell::new(false));
    let parker = session.clone();
    let parked_flag = parked_mid_release.clone();
    IDLE_RELEASE_BEFORE_COMMIT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            parker.park_idle_child_memory();
            parked_flag.set(parker.agent.state().messages().is_empty());
        }));
    });

    session.release_idle_runtime_messages();

    assert!(
        parked_mid_release.get(),
        "the park must run inside the release window and clear the runtime messages"
    );
    assert!(
        session.agent.state().messages().is_empty(),
        "a release that read the transcript before a park must not restore it into the parked child"
    );
    let rehydrated = session.messages();
    assert_eq!(rehydrated.len(), 2);
    assert_eq!(user_message_text(&rehydrated[0]), large_user);
}

#[test]
fn sdk_extension_context_omits_the_goal_store_file_without_a_session_directory() {
    let session = test_session();
    let context = session.extension_context(Arc::new(TestExtensionUi));
    assert_eq!(context.goal_store_file, None);
}

fn test_session_with_stream_function(uses_default_stream_function: bool) -> AgentSession {
    let runtime = ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()),
        ..Default::default()
    })
    .expect("valid model runtime");
    AgentSession::new(AgentSessionConfig {
        agent: stub_agent(),
        session_manager: SessionManager::in_memory("/tmp", None, None),
        settings_manager: SettingsManager::from_storage(
            Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
            false,
        ),
        cwd: "/tmp".to_owned(),
        agent_dir: Some("/tmp/maho-agent".to_owned()),
        fallback_now: None,
        retry_random: None,
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: BTreeMap::new(),
        custom_tools: Vec::new(),
        model_runtime: Some(runtime),
        model_registry: None,
        uses_default_stream_function: Some(uses_default_stream_function),
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: None,
    })
    .expect("session")
}

#[tokio::test]
async fn extension_tool_execution_forwards_partial_updates() {
    use maho_ext_api::ExtensionSessionActions;
    let session = test_session();
    let tool = AgentTool {
        label: "updates".to_owned(),
        prepare_arguments: None,
        replay: None,
        execution_mode: None,
        tool: maho_ai::types::Tool {
            name: "updates".to_owned(),
            description: "updates".to_owned(),
            parameters: serde_json::json!({"type":"object","properties":{}}),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
        execute: Arc::new(|_, _, _, update| {
            Box::pin(async move {
                update.expect("update callback")(AgentToolResult::text("partial"));
                AgentToolResult::text("complete")
            })
        }),
    };
    session.agent.set_tools(vec![tool]);
    let updates = Arc::new(Mutex::new(Vec::new()));
    let observed = updates.clone();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let result = actions
        .execute_tool(
            "updates",
            serde_json::json!({}),
            maho_ext_api::ExecuteToolOptions {
                on_update: Some(Arc::new(move |result| {
                    observed.lock().unwrap().push(result)
                })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(result, AgentToolResult::text("complete"));
    assert_eq!(
        *updates.lock().unwrap(),
        vec![AgentToolResult::text("partial")]
    );
}

#[test]
fn extension_settings_read_live_configured_values() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    session.agent.set_model(test_model());
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    assert_eq!(actions.get_prompt_cache_goal_backstop_max_seconds(), 270.0);
    session.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global,
        &serde_json::json!({
            "lookAt":{"enabled":false,"models":["faux/faux-1"]},
            "askUser":{"enabled":false,"timeoutMinutes":200},
            "images":{"autoResize":false,"blockImages":true},
            "compaction":{"reserveTokens":100,"keepRecentTokens":200,"modelOverrides":{"faux/faux-1":{"reserveTokens":300,"keepRecentTokens":400}}},
            "promptCache":{"goalBackstopMaxSeconds":99,"keepAlive":{"enabled":true,"maxRequestsPerSession":7,"maxCostUsdPerSession":0.3,"marginSeconds":11}}
        }).as_object().unwrap().clone())).unwrap();
    assert_eq!(
        actions.get_look_at_settings(),
        maho_ext_api::LookAtSettings {
            enabled: false,
            models: Some(vec!["faux/faux-1".to_owned()])
        }
    );
    assert_eq!(
        actions.get_ask_user_settings(),
        maho_ext_api::AskUserSettings {
            enabled: false,
            timeout_minutes: 120.0
        }
    );
    assert_eq!(
        actions.get_image_settings(),
        maho_ext_api::ImageSettings {
            auto_resize: false,
            block_images: true
        }
    );
    assert_eq!(
        actions.get_compaction_settings(),
        maho_ext_api::CompactionSettings {
            enabled: true,
            reserve_tokens: 300,
            keep_recent_tokens: 400
        }
    );
    assert_eq!(actions.get_prompt_cache_goal_backstop_max_seconds(), 99.0);
    assert_eq!(
        actions.get_prompt_cache_keep_alive_settings(),
        maho_ext_api::PromptCacheKeepAliveSettings {
            enabled: true,
            max_requests_per_session: 7,
            max_cost_usd_per_session: 0.3,
            margin_seconds: 11.0,
        }
    );
}

#[tokio::test]
async fn an_unconfigured_provider_refuses_the_request() {
    let session = test_session();
    let error = session
        .get_required_request_auth(&test_model())
        .await
        .expect_err("refused");
    assert!(error.contains("No API key found for faux"), "{error}");
}

#[test]
fn extension_resolved_settings_project_all_fields_and_follow_live_model_and_override() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    session.agent.set_model(test_model());
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([
        ("compaction".into(), serde_json::json!({
            "reserveTokens":101,"keepRecentTokens":202,
            "speculativeEnabled":false,"speculativeFraction":0.31,"speculativeCooldownMs":321.5,
            "restorationEnabled":false,"restorationMaxItems":2.5,"restorationMaxTokensPerItem":11.5,
            "restorationMaxTotalTokens":22.5,"restorationContextRatio":0.21,
            "idleCompactionEnabled":false,"graceBandEnabled":false,"toolAdmissionEnabled":false,
            "reminderEnabled":false,"reserveScalingEnabled":false,"speculativeLeadTokens":12000.5,
            "summarizationMaxDurationMs":45678.5,
            "modelOverrides":{"faux/faux-1":{"reserveTokens":303,"keepRecentTokens":404}}
        })),
    ]))
    });
    let projected = actions
        .get_resolved_compaction_settings()
        .expect("resolved settings");
    assert_eq!(
        projected,
        maho_ext_api::ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 303,
            keep_recent_tokens: 404,
            speculative_enabled: false,
            speculative_fraction: 0.31,
            speculative_cooldown_ms: 321.5,
            restoration_enabled: false,
            restoration_max_items: 2.5,
            restoration_max_tokens_per_item: 11.5,
            restoration_max_total_tokens: 22.5,
            restoration_context_ratio: 0.21,
            idle_compaction_enabled: false,
            grace_band_enabled: false,
            tool_admission_enabled: false,
            reminder_enabled: false,
            reserve_scaling_enabled: false,
            speculative_lead_tokens: Some(12000.5),
            summarization_max_duration_ms: Some(45678.5),
        }
    );
    let mut model = test_model();
    model.id = "other".into();
    session.agent.set_model(model);
    let changed = actions
        .get_resolved_compaction_settings()
        .expect("live model settings");
    assert_eq!(
        (changed.reserve_tokens, changed.keep_recent_tokens),
        (101, 202)
    );
    session.set_auto_compaction_enabled(false);
    assert!(
        !actions
            .get_resolved_compaction_settings()
            .expect("live enabled override")
            .enabled
    );
    assert!(!actions.get_compaction_settings().enabled);
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".into(),
            serde_json::json!({"reserveTokens":505,"keepRecentTokens":606}),
        )]))
    });
    let revised = actions
        .get_resolved_compaction_settings()
        .expect("live config revision");
    assert_eq!(
        (revised.reserve_tokens, revised.keep_recent_tokens),
        (505, 606)
    );
}

#[tokio::test]
async fn extension_fallback_chain_updates_roundtrip_through_real_settings() {
    use maho_ext_api::ExtensionSessionSettings;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    actions
        .set_fallback_chain("faux-1", &["faux/faux-2".into()])
        .await
        .unwrap();
    assert_eq!(
        actions.get_retry_fallback_settings().chains["faux-1"],
        ["faux/faux-2"]
    );
    actions.remove_fallback_chain("faux-1").await.unwrap();
    assert!(
        !actions
            .get_retry_fallback_settings()
            .chains
            .contains_key("faux-1")
    );
}

#[tokio::test]
async fn extension_model_selection_returns_false_without_configured_auth() {
    use maho_ext_api::ExtensionSessionActions;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    assert!(!actions.set_model(test_model()).await.unwrap());
    assert!(!actions.set_session_model(test_model()).await.unwrap());
    let registry = ExtensionModelRegistryView::new(&session, Default::default());
    assert_eq!(
        maho_ext_api::ModelRegistry::get_all(&registry),
        session.model_registry().get_all()
    );
    assert_eq!(
        maho_ext_api::ModelRegistry::find(&registry, "faux", "faux-1"),
        session.model_registry().find("faux", "faux-1")
    );
    assert!(
        maho_ext_api::ModelRegistry::get_api_key_for_provider(&registry, "faux")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn extension_registry_services_use_configured_runtime_and_shared_credential_kind() {
    use maho_ext_api::ModelRegistry as _;
    let session = test_session();
    let provider = maho_ai::providers::faux::faux_provider(
        maho_ai::providers::faux::RegisterFauxProviderOptions {
            tokens_per_second: Some(0.0),
            ..Default::default()
        },
    )
    .expect("valid faux provider");
    provider.set_responses(vec![
        maho_ai::providers::faux::faux_assistant_message("configured stream", Default::default())
            .into(),
    ]);
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    session
        .model_registry()
        .auth_storage
        .set(
            "faux",
            Some(serde_json::json!({"type":"api_key","key":"fixture-stored"})),
        )
        .expect("stored auth");
    let registry = ExtensionModelRegistryView::new(&session, Default::default());
    let auth = registry
        .get_provider_auth("faux")
        .await
        .expect("configured auth")
        .expect("auth resolution");
    let kind = registry
        .get_stored_credential_type("faux")
        .expect("metadata");
    let absent = registry
        .get_stored_credential_type("absent-fixture")
        .expect("absent metadata");
    let model = provider.get_model(Some("faux-1")).expect("model");
    let stream = registry
        .stream_simple(&model, &maho_ai::types::Context::default(), None)
        .expect("configured stream creation");
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result()).await;
    session.dispose().await;
    // senpi faux auth resolves to `{ auth: {} }` regardless of the stored key (faux.ts:798); the
    // stored credential is still reported through the shared credential-kind view.
    assert!(auth.auth.api_key.is_none());
    assert_eq!(kind, Some(maho_ai::auth::types::CredentialType::ApiKey));
    assert_eq!(absent, None);
    assert_eq!(
        maho_ai::utils::text::content_text(
            &response
                .expect("bounded stream")
                .expect("stream response")
                .content,
            ""
        ),
        "configured stream"
    );
}

#[tokio::test]
async fn extension_exec_honors_cancel_and_preserves_process_results() {
    use maho_ext_api::ExtensionSessionActions;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let signal = maho_ext_api::AbortSignal::default();
    signal.abort();
    let temp_dir = std::env::temp_dir();
    let (cmd, args_exit, args_out) = if cfg!(windows) {
        (
            "node",
            vec!["-e".to_owned(), "process.exit(99)".to_owned()],
            vec![
                "-e".to_owned(),
                "process.stdout.write('out'); process.stderr.write('err'); process.exit(3)"
                    .to_owned(),
            ],
        )
    } else {
        (
            "sh",
            vec!["-c".to_owned(), "exit 99".to_owned()],
            vec![
                "-c".to_owned(),
                "printf out; printf err >&2; exit 3".to_owned(),
            ],
        )
    };
    let killed = actions
        .exec(
            cmd,
            &args_exit,
            &temp_dir,
            maho_ext_api::ExecOptions {
                signal: Some(signal),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(killed.killed);
    let result = actions
        .exec(cmd, &args_out, &temp_dir, Default::default())
        .await
        .unwrap();
    assert_eq!(
        (
            result.stdout.as_str(),
            result.stderr.as_str(),
            result.code,
            result.killed
        ),
        ("out", "err", 3, false)
    );
    let missing_cmd = if cfg!(windows) {
        r"C:\definitely\missing.exe"
    } else {
        "/definitely/missing"
    };
    let missing = actions
        .exec(missing_cmd, &[], &temp_dir, Default::default())
        .await
        .unwrap();
    assert_eq!(missing.code, 1);
    assert!(!missing.stderr.is_empty());
}

#[tokio::test]
async fn queued_account_mutation_rechecks_generation_after_storage_admission() {
    use maho_ext_api::ModelRegistry as _;
    let session = test_session();
    let storage = session.model_registry().auth_storage.clone();
    storage
        .set(
            "fixture-account",
            Some(serde_json::json!({"type":"api_key","key":"fixture-key",
        "accounts":[{"name":"default","key":"fixture-key"}]})),
        )
        .expect("seed");
    let before = storage.get("fixture-account");
    let registry = ExtensionModelRegistryView::new(&session, Default::default());
    let permit = storage.account_mutation.lock().await;
    let mut mutation =
        Box::pin(registry.rename_credential_account("fixture-account", "default", Some("Late")));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(mutation.as_mut(), cx).is_pending()
        ))
        .await
    );
    session.invalidate_extension_runtime().await;
    drop(permit);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), mutation).await;
    session.dispose().await;
    assert!(result.expect("bounded queued admission").is_err());
    assert_eq!(storage.get("fixture-account"), before);
}

#[tokio::test]
async fn account_reset_emits_after_success_and_rejects_retired_admission() {
    use maho_ext_api::ModelRegistry as _;
    let session = test_session();
    let dir = tempfile::tempdir().expect("sidecar directory");
    let storage = session.model_registry().auth_storage.clone();
    storage
        .set(
            "fixture-reset",
            Some(serde_json::json!({"type":"api_key","key":"fixture",
        "accounts":[{"name":"work","key":"fixture","blockReason":"rate_limit","keep":true}]})),
        )
        .expect("seed");
    let events = maho_ext_api::EventBus::default();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&observed);
    let _subscription = events.on(
        "provider-accounts-changed",
        Arc::new(move |event| lock(&captured).push(event.clone())),
    );
    let mut registry = ExtensionModelRegistryView::new(&session, events);
    registry.agent_dir = dir.path().to_string_lossy().into_owned();
    registry
        .reset_credential_account("fixture-reset", "work")
        .await
        .expect("reset");
    assert_eq!(lock(&observed).len(), 1);
    let before = storage.get("fixture-reset");
    assert!(
        before.as_ref().expect("credential")["accounts"][0]
            .get("blockReason")
            .is_none()
    );
    assert!(
        registry
            .reset_credential_account("fixture-reset", "missing")
            .await
            .is_err()
    );
    let permit = storage.account_mutation.lock().await;
    let mut mutation = Box::pin(registry.reset_credential_account("fixture-reset", "work"));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(mutation.as_mut(), cx).is_pending()
        ))
        .await
    );
    session.invalidate_extension_runtime().await;
    drop(permit);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), mutation).await;
    session.dispose().await;
    assert!(result.expect("bounded reset admission").is_err());
    assert_eq!(storage.get("fixture-reset"), before);
    assert_eq!(lock(&observed).len(), 1);
}

#[tokio::test]
async fn returned_deferred_batch_cannot_start_after_runtime_replacement() {
    use maho_ext_api::ExtensionActions as _;
    for cancelled in [false, true] {
        let session = test_session();
        lock(&session.settled_delivery).begin(session.user_abort_generation.load(Ordering::SeqCst));
        SessionExtensionActions(Arc::downgrade(&session.inner))
            .send_user_message(
                maho_ext_api::UserMessageContent::Text("retired deferred fixture".into()),
                Default::default(),
            )
            .expect("defer");
        let batch = lock(&session.settled_delivery)
            .finish(session.user_abort_generation.load(Ordering::SeqCst));
        assert_eq!(batch.turn_claims.len(), 1);
        if cancelled {
            session.abort().await;
        } else {
            session.invalidate_extension_runtime().await;
        }
        let before = session.messages();
        for action in batch.actions {
            action();
        }
        let disposition = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            batch.turn_claims[0].disposition(),
        )
        .await;
        let idle =
            tokio::time::timeout(std::time::Duration::from_secs(5), session.wait_for_idle()).await;
        let after = session.messages();
        session.dispose().await;
        idle.expect("bounded retired action settlement");
        assert_eq!(
            disposition.expect("bounded retired action"),
            Some(crate::agent_settled_delivery::DeferredTurnDisposition::FinishedWithoutStart)
        );
        assert_eq!(after, before);
    }
}

#[tokio::test]
async fn runtime_retirement_discards_pending_settled_delivery_before_execution() {
    for disposed in [false, true] {
        let session = test_session();
        let ran = Arc::new(AtomicU64::new(0));
        let counter = ran.clone();
        {
            let mut delivery = lock(&session.settled_delivery);
            delivery.begin(session.user_abort_generation.load(Ordering::SeqCst));
            assert!(delivery.defer_trigger_turn(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            }));
        }
        if disposed {
            session.dispose().await;
        } else {
            session.invalidate_extension_runtime().await;
        }
        let batch = lock(&session.settled_delivery)
            .finish(session.user_abort_generation.load(Ordering::SeqCst));
        session.dispose().await;
        assert!(batch.actions.is_empty());
        assert!(batch.turn_claims.is_empty());
        assert_eq!(ran.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn session_extension_events_reach_the_bound_runner_bus() {
    let session = test_session();
    let extension = maho_ext_api::LoadedExtension::new(
        "<inline:mcp>",
        session.cwd().into(),
        Default::default(),
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let runner = session
        .extension_runner
        .lock()
        .await
        .clone()
        .expect("bound runner");
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let _subscription = runner.events.on(
        "goal_store_changed",
        Arc::new(move |value| lock(&captured).push(value.clone())),
    );
    session.emit_extension_event(
        "goal_store_changed",
        &serde_json::json!({"threadId":"thread"}),
    );
    assert_eq!(
        lock(&observed).as_slice(),
        [serde_json::json!({"threadId":"thread"})]
    );
    session.dispose().await;
}

#[tokio::test]
async fn tool_renderers_are_absent_without_a_registered_extension_renderer() {
    let session = retry_session(vec![], 0);
    session
        .set_extension_runner(ExtensionRunner::new(
            Vec::new(),
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    assert!(!session.tool_renderers_snapshot().await.contains_key("echo"));
    assert!(session.tool_renderers_snapshot().await.is_empty());
}

#[tokio::test]
async fn live_mcp_wire_status_is_delivered_natively_to_the_session_subscriber() {
    let session = test_session();
    let extension = maho_ext_api::LoadedExtension::new(
        "<inline:mcp>",
        session.cwd().into(),
        Default::default(),
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let _subscription = session
        .subscribe_mcp_wire_status::<serde_json::Value>(Arc::new(move |snapshot| {
            lock(&captured).push(snapshot.clone())
        }))
        .await
        .expect("bound runner");
    assert!(
        session
            .publish_mcp_wire_status(&serde_json::json!({"servers":[]}))
            .await
    );
    assert_eq!(
        lock(&observed).as_slice(),
        [serde_json::json!({"servers":[]})]
    );
    session.dispose().await;
}

#[tokio::test]
async fn binding_mcp_wire_status_replays_the_retained_snapshot_then_delivers_live() {
    let session = test_session();
    let extension = maho_ext_api::LoadedExtension::new(
        "<inline:mcp>",
        session.cwd().into(),
        Default::default(),
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    // The attach inventory is published BEFORE any subscriber (the Task12 miss scenario).
    assert!(
        session
            .publish_mcp_wire_status(&serde_json::json!({"servers":["attach"]}))
            .await
    );
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let _subscription = session
        .bind_mcp_wire_status::<serde_json::Value>(Arc::new(move |snapshot| {
            lock(&captured).push(snapshot.clone())
        }))
        .await
        .expect("bound runner");
    assert_eq!(
        lock(&observed).as_slice(),
        [serde_json::json!({"servers":["attach"]})],
        "the retained attach snapshot is replayed exactly once"
    );
    assert!(
        session
            .publish_mcp_wire_status(&serde_json::json!({"servers":["live"]}))
            .await
    );
    assert_eq!(
        lock(&observed).as_slice(),
        [
            serde_json::json!({"servers":["attach"]}),
            serde_json::json!({"servers":["live"]})
        ]
    );
    // A later bind reads the current retained snapshot once (no duplicate of the replay).
    let second = Arc::new(Mutex::new(Vec::new()));
    let captured_second = second.clone();
    let _second = session
        .bind_mcp_wire_status::<serde_json::Value>(Arc::new(move |snapshot| {
            lock(&captured_second).push(snapshot.clone())
        }))
        .await
        .expect("bound runner");
    assert_eq!(
        lock(&second).as_slice(),
        [serde_json::json!({"servers":["live"]})]
    );
    assert_eq!(
        session.mcp_wire_status_snapshot::<serde_json::Value>(),
        Some(serde_json::json!({"servers":["live"]}))
    );
    session.dispose().await;
}

#[tokio::test]
async fn a_disposed_session_serves_no_retained_mcp_snapshot() {
    let session = test_session();
    let extension = maho_ext_api::LoadedExtension::new(
        "<inline:mcp>",
        session.cwd().into(),
        Default::default(),
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    assert!(
        session
            .publish_mcp_wire_status(&serde_json::json!({"servers":["attach"]}))
            .await
    );
    assert!(
        session
            .mcp_wire_status_snapshot::<serde_json::Value>()
            .is_some()
    );
    session.dispose().await;
    assert!(
        session
            .mcp_wire_status_snapshot::<serde_json::Value>()
            .is_none(),
        "a disposed session must not serve a stale snapshot"
    );
}

#[tokio::test]
async fn an_extension_bus_publication_reaches_the_session_snapshot_and_a_late_bind() {
    let session = test_session();
    let extension = maho_ext_api::LoadedExtension::new(
        "<inline:mcp>",
        session.cwd().into(),
        Default::default(),
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    // The MCP extension publishes on the shared runner bus WITHOUT holding the AgentSession.
    let runner = session
        .extension_runner
        .lock()
        .await
        .clone()
        .expect("bound runner");
    let attach = serde_json::json!({"servers":["attach"]});
    let erased: Arc<dyn std::any::Any + Send + Sync> = Arc::new(attach.clone());
    runner
        .events
        .emit_native::<Arc<dyn std::any::Any + Send + Sync>>(
            MCP_WIRE_STATUS_CHANGED_EVENT,
            &erased,
        );
    // The session retained it through the bind-time retention subscription.
    assert_eq!(
        session.mcp_wire_status_snapshot::<serde_json::Value>(),
        Some(attach.clone()),
        "the extension's bus publication reached the session snapshot"
    );
    // A subscriber that binds afterwards receives the retained initial snapshot.
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let _subscription = session
        .bind_mcp_wire_status::<serde_json::Value>(Arc::new(move |snapshot| {
            lock(&captured).push(snapshot.clone())
        }))
        .await
        .expect("bound runner");
    assert_eq!(lock(&observed).as_slice(), [attach]);
    session.dispose().await;
}

#[tokio::test]
async fn a_bound_runner_exposes_the_session_command_context() {
    let session = test_session();
    let extension = maho_ext_api::LoadedExtension::new(
        "<inline:goal>",
        session.cwd().into(),
        Default::default(),
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let context = session
        .extension_command_context()
        .await
        .expect("command context");
    assert!(context.runtime.assert_active().is_ok());
    session.dispose().await;
}

#[tokio::test]
async fn account_changes_reach_the_process_global_registry() {
    use maho_ext_api::ModelRegistry as _;
    let session = test_session();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let unsubscribe = crate::provider_account_events::subscribe_provider_account_events(Arc::new(
        move |event| {
            if matches!(&event, crate::provider_account_events::ProviderAccountEvent::AccountsChanged { provider } if provider == "registry-fixture")
            {
                lock(&captured).push(event);
            }
        },
    ));
    session
        .model_registry()
        .auth_storage
        .set(
            "registry-fixture",
            Some(serde_json::json!({
                "type":"api_key","key":"fixture-flat-secret",
                "accounts":[{"name":"default","key":"fixture-first-secret","source":"login"},
                    {"name":"work","key":"fixture-second-secret","source":"import"}]
            })),
        )
        .expect("seed shared storage");
    let registry = ExtensionModelRegistryView::new(&session, Default::default());
    registry
        .pin_credential_account("registry-fixture", Some("work"))
        .await
        .expect("pin");
    unsubscribe();
    session.dispose().await;
    assert_eq!(
        lock(&observed).as_slice(),
        [
            crate::provider_account_events::ProviderAccountEvent::AccountsChanged {
                provider: "registry-fixture".to_owned()
            }
        ]
    );
}

#[tokio::test]
async fn extension_account_facade_mutates_shared_store_and_emits_only_success() {
    use maho_ext_api::ModelRegistry as _;
    let session = test_session();
    let dir = tempfile::tempdir().expect("account sidecar");
    let events = maho_ext_api::EventBus::default();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let _subscription = events.on(
        "provider-accounts-changed",
        Arc::new(move |value| lock(&captured).push(value.clone())),
    );
    session
        .model_registry()
        .auth_storage
        .set(
            "fixture-account",
            Some(serde_json::json!({
                "type":"api_key","key":"fixture-flat-secret",
                "accounts":[{"name":"default","key":"fixture-first-secret","source":"login"},
                    {"name":"work","key":"fixture-second-secret","source":"import"}]
            })),
        )
        .expect("seed shared storage");
    let mut registry = ExtensionModelRegistryView::new(&session, events);
    registry.agent_dir = dir.path().to_string_lossy().into_owned();
    let exercise = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let before = registry.get_credential_accounts("fixture-account").await?;
        registry
            .pin_credential_account("fixture-account", Some("work"))
            .await?;
        registry
            .rename_credential_account("fixture-account", "work", Some("Work account"))
            .await?;
        let changed = registry.get_credential_accounts("fixture-account").await?;
        registry
            .pin_credential_account("fixture-account", None)
            .await?;
        let error = registry
            .remove_credential_account("fixture-account", "missing")
            .await;
        registry
            .remove_credential_account("fixture-account", "work")
            .await?;
        let after = registry.get_credential_accounts("fixture-account").await?;
        Ok::<_, maho_ext_api::ExtensionFailure>((before, changed, after, error))
    })
    .await;
    let shared = session.model_runtime().credentials.get("fixture-account");
    session.dispose().await;
    let (before, changed, after, error) = exercise
        .expect("bounded account operations")
        .expect("account facade");
    assert_eq!(before.len(), 2);
    assert_eq!(
        before[1].source,
        maho_ext_api::CredentialAccountSource::Import
    );
    assert!(changed[1].pinned);
    assert_eq!(changed[1].display_name.as_deref(), Some("Work account"));
    assert!(error.is_err());
    assert_eq!(after.len(), 1);
    assert!(!after[0].pinned);
    assert_eq!(
        shared.expect("same runtime store")["accounts"]
            .as_array()
            .expect("accounts")
            .len(),
        1
    );
    assert_eq!(lock(&observed).len(), 4);
    assert!(lock(&observed).iter().all(|value| value
        == &serde_json::json!({"type":"accounts_changed","provider":"fixture-account"})));
    assert!(!format!("{before:?}{changed:?}{after:?}").contains("secret"));
}

#[tokio::test]
async fn extension_command_new_session_reaches_real_session_manager() {
    use maho_ext_api::ExtensionCommandContextActions;
    let session = test_session();
    let original = session.session_id();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    actions.wait_for_idle().await.unwrap();
    let result = actions
        .new_session(maho_ext_api::NewSessionOptions::default())
        .await
        .unwrap();
    assert!(!result.cancelled);
    assert_ne!(session.session_id(), original);
    assert!(session.messages().is_empty());
}

struct ReplacementTestUi;
impl maho_ext_api::ExtensionUi for ReplacementTestUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(
        &self,
        _: &str,
        _: Option<maho_ext_api::WidgetContent>,
        _: maho_ext_api::ExtensionWidgetOptions,
    ) {
    }
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        _: maho_ext_api::ComponentFactory,
        _: maho_ext_api::CustomUiOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, Value> {
        Box::pin(async { Err("UI unavailable".into()) })
    }
    fn theme(&self) -> maho_ext_api::Theme {
        Default::default()
    }
}
fn replacement_test_context(session: &AgentSession) -> maho_ext_api::ExtensionContext {
    maho_ext_api::ExtensionContext {
        ui: Arc::new(ReplacementTestUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: session.cwd().into(),
        agent_dir: session.agent_dir().into(),
        session_manager: Arc::new(ExtensionSessionManagerView {
            session: Arc::downgrade(&session.inner),
            id: session.session_id(),
            file: None,
        }),
        model_registry: Arc::new(ExtensionModelRegistryView::new(session, Default::default())),
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
        get_system_prompt_options_fn: Arc::new(Default::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

#[tokio::test]
async fn replacement_reloads_factories_invalidates_old_handles_and_removes_old_tools() {
    let session = test_session();
    session.register_tool_definition(
        test_definition("base"),
        empty_source_info(),
        test_tool("base"),
    );
    let runtimes = Arc::new(Mutex::new(Vec::<maho_ext_api::ExtensionRuntime>::new()));
    let captured = runtimes.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "replacement".into(),
        source_info: empty_source_info(),
        factory: Arc::new(move |api| {
            let first = lock(&captured).is_empty();
            if let Some(previous) = lock(&captured).last() {
                assert!(
                    previous.assert_active().is_err(),
                    "invalidate before invoking replacement factories"
                );
            }
            lock(&captured).push(api.runtime.clone());
            if first {
                api.register_tool(test_definition("old-only"));
                api.register_lazy_tool_activator(Arc::new(|_| false));
                api.register_removed_tool_hint("old-only", "old hint");
            }
            api.register_command(
                "generation",
                Some(lock(&captured).len().to_string()),
                None,
                Arc::new(|_, _| Box::pin(async { Ok(()) })),
            );
            Box::pin(async { Ok(()) })
        }),
    };
    let runner = ExtensionRunner::from_async_factories(
        vec![factory],
        replacement_test_context(&session),
        Default::default(),
    )
    .await
    .unwrap();
    session.set_extension_runner(runner).await;
    let old = session
        .extension_runner
        .lock()
        .await
        .as_ref()
        .unwrap()
        .create_context()
        .unwrap();
    assert!(session.get_registered_tool("old-only").is_some());
    assert_eq!(session.state().extension_lazy_activators.len(), 1);
    assert_eq!(
        session
            .agent
            .removed_tool_hints()
            .get("old-only")
            .map(String::as_str),
        Some("old hint")
    );
    let command = session
        .extension_runner
        .lock()
        .await
        .as_ref()
        .unwrap()
        .create_command_context(Arc::new(SessionExtensionActions(Arc::downgrade(
            &session.inner,
        ))))
        .unwrap();
    let callback_id = Arc::new(Mutex::new(None));
    let captured_id = callback_id.clone();
    let result = command
        .new_session(maho_ext_api::NewSessionOptions {
            with_session: Some(Arc::new(move |ctx| {
                *lock(&captured_id) = Some(ctx.session_manager.session_id().to_owned());
                Box::pin(async move {
                    ctx.send_message(
                        maho_ext_api::CustomMessage {
                            custom_type: "replacement".into(),
                            content: vec![maho_ext_api::ToolContent::text("ready")],
                            display: true,
                            details: None,
                        },
                        Default::default(),
                    )
                    .await
                })
            })),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!result.cancelled);
    assert_eq!(*lock(&callback_id), Some(session.session_id()));
    assert_eq!(session.messages().last().unwrap().role(), "custom");
    assert!(lock(&runtimes)[0].assert_active().is_err());
    assert!(old.actions().is_err());
    assert!(session.get_registered_tool("old-only").is_none());
    assert!(session.state().extension_lazy_activators.is_empty());
    assert!(!session.agent.removed_tool_hints().contains_key("old-only"));
    assert!(session.get_registered_tool("base").is_some());
    let current = session
        .extension_runner
        .lock()
        .await
        .as_ref()
        .unwrap()
        .create_context()
        .unwrap();
    assert_eq!(current.session_manager.session_id(), session.session_id());
    current
        .ui
        .notify("new runtime", maho_ext_api::NotificationType::Info);
    assert!(!session.reload().await.unwrap().cancelled);
    assert!(current.actions().is_err());
    assert_eq!(lock(&runtimes).len(), 3);
    assert_eq!(
        session
            .get_commands()
            .iter()
            .find(|command| command.name == "generation")
            .unwrap()
            .description
            .as_deref(),
        Some("3")
    );
}

#[test]
fn extension_next_turn_message_is_retained_until_next_admission() {
    use maho_ext_api::ExtensionActions;
    let session = test_session();
    session.state().extension_mode = ExtensionMode::Rpc;
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription =
        session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    actions
        .send_message(
            maho_ext_api::CustomMessage {
                custom_type: "notice".into(),
                content: vec![maho_ext_api::ToolContent::text("later")],
                display: true,
                details: None,
            },
            maho_ext_api::SendMessageOptions {
                trigger_turn: false,
                deliver_as: Some(maho_ext_api::DeliverAs::NextTurn),
            },
        )
        .unwrap();
    assert!(session.messages().is_empty());
    assert_eq!(session.state().pending_next_turn_messages.len(), 1);
    assert_eq!(session.pending_message_count(), 0);
    session.flush_pending_next_turn_messages().unwrap();
    assert_eq!(session.messages()[0].role(), "custom");
    assert_eq!(session.pending_message_count(), 0);
    assert_eq!(
        session.with_session_manager(|manager| manager.entries())[0]["type"],
        "custom_message"
    );
    assert!(matches!(
        &lock(&events)[0],
        AgentSessionEvent::EntryAppended { .. }
    ));
}

#[tokio::test]
async fn extension_user_message_text_blocks_are_separated_by_newlines() {
    use maho_ext_api::ExtensionActions;
    let session = test_session();
    let provider =
        maho_ai::providers::faux::faux_provider(Default::default()).expect("valid faux provider");
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::Agent(maho_agent::types::AgentEvent::MessageStart {
            message, ..
        }) = event
            && message.role() == "user"
        {
            let _ = sender.send(user_message_text(message));
        }
    }));
    SessionExtensionActions(Arc::downgrade(&session.inner))
        .send_user_message(
            maho_ext_api::UserMessageContent::Blocks(vec![
                maho_ext_api::ToolContent::text("first"),
                maho_ext_api::ToolContent::text("second"),
            ]),
            Default::default(),
        )
        .unwrap();
    let text = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(text, "first\nsecond");
}

#[test]
fn extension_append_entry_publishes_saved_entry_without_changing_messages() {
    use maho_ext_api::ExtensionActions;
    let session = test_session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription =
        session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
    SessionExtensionActions(Arc::downgrade(&session.inner))
        .append_entry("state", Some(serde_json::json!({"value":1})))
        .unwrap();
    assert!(session.messages().is_empty());
    let events = lock(&events);
    let AgentSessionEvent::EntryAppended { entry } = &events[0] else {
        panic!("saved entry event")
    };
    assert_eq!(entry.kind, "custom");
    assert_eq!(entry.data["data"]["value"], 1);
}

#[test]
fn session_name_change_queues_extension_metadata_event() {
    let session = test_session();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    session.state().extension_event_sender = Some(sender);
    session.set_session_name("renamed");
    assert!(
        matches!(receiver.try_recv().unwrap(), maho_ext_api::ExtensionEvent::SessionInfoChanged { name: Some(name) } if name == "renamed")
    );
}

#[test]
fn extension_abort_defaults_to_user_without_marking_system_cancellation_as_user() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    actions.abort(Some(maho_ext_api::AbortSource::System));
    assert!(!session.state().user_aborted);
    actions.abort(None);
    assert!(session.state().user_aborted);
}

#[tokio::test]
async fn event_context_signal_observes_cancellation_only_during_invocation() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let controller = maho_ai::utils::abort::AbortController::new();
    let (subscription, signal) = ExtensionEventSignalSubscription::new(controller.signal());
    EXTENSION_EVENT_SIGNAL
        .scope(signal, async {
            let captured = actions.get_signal().unwrap();
            assert!(!captured.is_aborted());
            controller.abort(None);
            assert!(captured.is_aborted());
        })
        .await;
    drop(subscription);
    assert!(actions.get_signal().is_none());
}

#[tokio::test]
async fn compaction_preparation_details_are_scoped_to_the_hook() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let details = maho_ext_api::CompactionPreparationDetails {
        preparation: maho_ext_api::CompactionPreparation {
            settings: actions.get_compaction_settings(),
            messages_to_summarize: Vec::new(),
            turn_prefix_messages: Vec::new(),
            tokens_before: 1,
            first_kept_entry_id: "kept".into(),
            previous_summary: None,
        },
        source_messages: Some(vec![make_user_message("prefix", None)]),
        turn_prefix_source_messages: Some(vec![make_user_message("turn prefix", None)]),
        is_split_turn: true,
        file_ops: maho_ext_api::CompactionFileOperations {
            read: vec!["file".into()],
            ..Default::default()
        },
    };
    EXTENSION_COMPACTION_PREPARATION
        .scope(details.clone(), async {
            assert_eq!(actions.get_compaction_preparation(), Some(details));
        })
        .await;
    assert!(actions.get_compaction_preparation().is_none());
}

#[tokio::test]
async fn extension_compaction_rejects_stale_warm_anchor_before_writing() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let result = actions
        .apply_compaction(
            maho_ext_api::CompactionResult {
                summary: "summary".into(),
                first_kept_entry_id: "missing".into(),
                tokens_before: 100,
                details: None,
            },
            maho_ext_api::ApplyCompactionOptions {
                reason: maho_ext_api::CompactionReason::Extension,
                expected_revision: None,
                signal: None,
                expected_warm_anchor: Some(maho_ext_api::WarmAnchorSnapshot {
                    first_kept_entry_id: "missing".into(),
                    prefix_entry_ids: vec!["old".into()],
                    latest_compaction_entry_id: None,
                }),
            },
        )
        .await
        .unwrap();
    assert_eq!(result, maho_ext_api::ApplyCompactionResult::Stale);
    assert!(
        session
            .with_session_manager(|manager| manager.entries())
            .is_empty()
    );
}

#[tokio::test]
async fn rejected_extension_compaction_ends_its_feedback_operation() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let ends = Arc::new(Mutex::new(Vec::new()));
    let observed = ends.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::CompactionEnd { error_message, .. } = event {
            lock(&observed).push(error_message.clone());
        }
    }));
    let signal = actions
        .begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Extension,
        })
        .unwrap();
    let admission = session.prompt_admission.lock().await;
    let result = actions
        .apply_compaction(
            maho_ext_api::CompactionResult {
                summary: "summary".into(),
                first_kept_entry_id: String::new(),
                tokens_before: 100,
                details: None,
            },
            maho_ext_api::ApplyCompactionOptions {
                reason: maho_ext_api::CompactionReason::Extension,
                expected_revision: None,
                expected_warm_anchor: None,
                signal: Some(signal),
            },
        )
        .await
        .unwrap();
    drop(admission);
    assert_eq!(result, maho_ext_api::ApplyCompactionResult::Rejected);
    assert_eq!(
        *lock(&ends),
        [Some("Compaction did not apply: session busy".to_owned())]
    );
    assert!(!session.is_compacting());
}

#[tokio::test]
async fn extension_compaction_signal_observes_real_session_cancellation() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let signal = actions
        .begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Extension,
        })
        .unwrap();
    session.abort_compaction();
    tokio::time::timeout(std::time::Duration::from_secs(1), signal.cancelled())
        .await
        .unwrap();
    assert!(signal.is_aborted());
    actions.end_compaction(maho_ext_api::EndCompactionOptions {
        reason: maho_ext_api::CompactionReason::Extension,
        signal: Some(signal),
        aborted: Some(true),
        error_message: None,
    });
    assert!(!session.is_compacting());
}

#[tokio::test]
async fn repeated_compaction_hook_receives_previous_summary() {
    let session = retry_session(Vec::new(), 0);
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".into(),
            serde_json::json!({"keepRecentTokens":1,"reserveTokens":0}),
        )]))
    });
    for text in ["old task".repeat(200), "recent task".repeat(200)] {
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0}))
        });
    }
    session.rebuild_session_context().expect("history");
    let summaries = Arc::new(Mutex::new(Vec::new()));
    let captured = summaries.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:repeat-compaction>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(move |event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("preparation");
            };
            let has_summary = EXTENSION_COMPACTION_PREPARATION.with(|details| {
                details
                    .source_messages
                    .as_ref()
                    .expect("source messages")
                    .iter()
                    .any(|message| message.role() == "compactionSummary")
            });
            lock(&captured).push(has_summary);
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(maho_ext_api::CompactionResult {
                            summary: "digest".into(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                            tokens_before: event.preparation.tokens_before,
                            details: None,
                        }),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    session.compact(None).await.expect("first compaction");
    session.with_session_manager_mut(|manager| {
        manager.append_message(
            serde_json::json!({"role":"user","content":"next task".repeat(200),"timestamp":0}),
        )
    });
    session.rebuild_session_context().expect("new entry");
    session.compact(None).await.expect("second compaction");
    assert_eq!(*lock(&summaries), [false, true]);
}

#[tokio::test]
async fn a_native_stream_function_falls_back_to_the_bare_model() {
    let session = test_session_with_stream_function(false);
    let auth = session
        .get_summarization_request_auth(&test_model())
        .await
        .expect("auth");
    assert_eq!(auth.model.id, "faux-1");
    assert!(auth.api_key.is_none());
}

#[tokio::test]
async fn the_default_stream_function_requires_request_auth() {
    let session = test_session();
    let error = session
        .get_summarization_request_auth(&test_model())
        .await
        .expect_err("refused");
    assert!(error.contains("No API key found for faux"), "{error}");
}

#[test]
fn a_session_without_model_access_is_refused() {
    let result = AgentSession::new(AgentSessionConfig {
        agent: stub_agent(),
        session_manager: SessionManager::in_memory("/tmp", None, None),
        settings_manager: SettingsManager::from_storage(
            Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
            false,
        ),
        cwd: "/tmp".to_owned(),
        agent_dir: None,
        fallback_now: None,
        retry_random: None,
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: BTreeMap::new(),
        custom_tools: Vec::new(),
        model_runtime: None,
        model_registry: None,
        uses_default_stream_function: None,
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: None,
    });
    assert!(result.is_err());
}

#[tokio::test]
async fn listeners_receive_events_until_they_unsubscribe() {
    let session = test_session();
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    let subscription = session.subscribe(Arc::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    session.emit(AgentSessionEvent::AgentIdle);
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    drop(subscription);
    session.emit(AgentSessionEvent::AgentIdle);
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dispose_clears_the_listeners() {
    let session = test_session();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    session.state().extension_event_sender = Some(sender);
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    let _subscription = session.subscribe(Arc::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    session.dispose().await;
    session.emit(AgentSessionEvent::AgentIdle);
    assert_eq!(seen.load(Ordering::SeqCst), 0);
    assert!(receiver.recv().await.is_none());
}

#[test]
fn appending_a_session_entry_stores_it() {
    let session = test_session();
    session.append_session_entry(serde_json::json!({
        "id": "entry-1", "type": "message", "timestamp": "2026-01-01T00:00:00.000Z",
        "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }], "timestamp": 0 }
    }));
    assert!(
        session
            .with_session_manager(|manager| manager.entry("entry-1"))
            .is_some()
    );
}

#[test]
fn reserving_queued_input_order_is_monotonic() {
    let session = test_session();
    assert_eq!(session.reserve_queued_input_order(), 1);
    assert_eq!(session.reserve_queued_input_order(), 2);
}

#[test]
fn clear_queue_returns_messages_in_recovery_order() {
    let session = test_session();
    session.record_queued_input("b", StreamingBehavior::FollowUp, Some(2), None, None);
    session.record_queued_input("a", StreamingBehavior::Steer, Some(1), None, None);
    session.state().steering_messages.push("a".to_owned());
    session.state().follow_up_messages.push("b".to_owned());
    let cleared = session.clear_queue(false);
    let ordered: Vec<&str> = cleared
        .ordered
        .iter()
        .map(|input| input.text.as_str())
        .collect();
    assert_eq!(ordered, vec!["a", "b"]);
    assert_eq!(cleared.steering, vec!["a".to_owned()]);
    assert_eq!(cleared.follow_up, vec!["b".to_owned()]);
    assert_eq!(session.pending_message_count(), 0);
}

#[tokio::test]
async fn clear_queue_settles_only_genuinely_cleared_custom_receipts() {
    let session = test_session();

    // 1. Unqueued custom message and receipt simulating consumed in-flight
    let unqueued_msg = maho_agent::harness::messages::CustomMessage {
        role: "custom".into(),
        custom_type: "in_flight".into(),
        content: maho_agent::harness::messages::CustomMessageContent::Text(
            "in flight payload".into(),
        ),
        display: true,
        details: Some(serde_json::json!({"flight_id": 42})),
        timestamp: 0,
    };
    let unqueued_receipt = maho_ext_api::MessageDeliveryReceipt::pending();
    session.track_queued_custom_receipt(unqueued_msg.clone(), unqueued_receipt.clone());

    // 2. Genuinely queued custom message and receipt
    let queued_msg = maho_agent::harness::messages::CustomMessage {
        role: "custom".into(),
        custom_type: "queued_turn".into(),
        content: maho_agent::harness::messages::CustomMessageContent::Text("queued payload".into()),
        display: true,
        details: Some(serde_json::json!({"queued_id": 7})),
        timestamp: 0,
    };
    let queued_agent_msg = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(
        queued_msg.clone(),
    ));
    session.agent.steer(queued_agent_msg);
    let queued_receipt = maho_ext_api::MessageDeliveryReceipt::pending();
    session.track_queued_custom_receipt(queued_msg.clone(), queued_receipt.clone());

    // 3. Appended-but-still-queued custom message to verify transcript guard
    let appended_msg = maho_agent::harness::messages::CustomMessage {
        role: "custom".into(),
        custom_type: "appended_already".into(),
        content: maho_agent::harness::messages::CustomMessageContent::Text(
            "appended payload".into(),
        ),
        display: true,
        details: Some(serde_json::json!({"appended_id": 99})),
        timestamp: 0,
    };
    let appended_agent_msg = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(
        appended_msg.clone(),
    ));
    session.agent.steer(appended_agent_msg);
    let appended_receipt = maho_ext_api::MessageDeliveryReceipt::pending();
    session.track_queued_custom_receipt(appended_msg.clone(), appended_receipt.clone());
    session.with_session_manager_mut(|manager| {
        manager.append_custom_message(
            &appended_msg.custom_type,
            serde_json::to_value(&appended_msg.content).expect("content to value"),
            appended_msg.display,
            appended_msg.details.clone(),
        );
    });

    // 4. Pending next-turn message and receipt (must NOT be cleared or failed)
    let next_turn_msg = maho_agent::harness::messages::CustomMessage {
        role: "custom".into(),
        custom_type: "next_turn".into(),
        content: maho_agent::harness::messages::CustomMessageContent::Text(
            "next turn payload".into(),
        ),
        display: true,
        details: Some(serde_json::json!({"next_turn_id": 123})),
        timestamp: 0,
    };
    let next_turn_agent_msg = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(
        next_turn_msg.clone(),
    ));
    session
        .state()
        .pending_next_turn_messages
        .push(next_turn_agent_msg.clone());
    let next_turn_receipt = maho_ext_api::MessageDeliveryReceipt::pending();
    session.track_queued_custom_receipt(next_turn_msg.clone(), next_turn_receipt.clone());

    // 5. Pending custom message (must NOT be cleared or failed)
    let pending_streaming_msg = maho_agent::harness::messages::CustomMessage {
        role: "custom".into(),
        custom_type: "streaming_pending".into(),
        content: maho_agent::harness::messages::CustomMessageContent::Text(
            "streaming pending payload".into(),
        ),
        display: true,
        details: Some(serde_json::json!({"streaming_id": 456})),
        timestamp: 0,
    };
    session
        .state()
        .pending_custom_messages
        .push(pending_streaming_msg.clone());
    let pending_streaming_receipt = maho_ext_api::MessageDeliveryReceipt::pending();
    session.track_queued_custom_receipt(
        pending_streaming_msg.clone(),
        pending_streaming_receipt.clone(),
    );

    // Register oneshot settlement callbacks before the action
    let (unqueued_tx, unqueued_rx) = tokio::sync::oneshot::channel();
    unqueued_receipt.on_settled(move |outcome| {
        let _ = unqueued_tx.send(outcome);
    });

    let (queued_tx, queued_rx) = tokio::sync::oneshot::channel();
    queued_receipt.on_settled(move |outcome| {
        let _ = queued_tx.send(outcome);
    });

    let (appended_tx, appended_rx) = tokio::sync::oneshot::channel();
    appended_receipt.on_settled(move |outcome| {
        let _ = appended_tx.send(outcome);
    });

    // Act: clear the queue
    session.clear_queue(false);

    // Queued receipt must be failed by clear_queue
    let queued_outcome = tokio::time::timeout(std::time::Duration::from_secs(5), queued_rx)
        .await
        .expect("queued receipt did not settle within timeout")
        .expect("queued oneshot closed");
    assert!(queued_outcome.is_err());

    // Unqueued receipt must remain pending (not failed by clear_queue)
    assert!(unqueued_receipt.outcome().is_none());

    // Appended-but-still-queued receipt must also remain pending due to transcript guard
    assert!(appended_receipt.outcome().is_none());

    // Pending next-turn message and receipt must remain preserved in state and pending
    assert!(next_turn_receipt.outcome().is_none());
    assert_eq!(
        session.state().pending_next_turn_messages,
        vec![next_turn_agent_msg]
    );

    // Pending streaming custom message and receipt must remain preserved in state and pending
    assert!(pending_streaming_receipt.outcome().is_none());
    assert_eq!(
        session.state().pending_custom_messages,
        vec![pending_streaming_msg]
    );

    // Manually settle unqueued receipt as delivered and await exact outcome
    session.settle_queued_custom_receipt(&unqueued_msg, Ok(()));
    let unqueued_outcome = tokio::time::timeout(std::time::Duration::from_secs(5), unqueued_rx)
        .await
        .expect("unqueued receipt did not settle within timeout")
        .expect("unqueued oneshot closed");
    assert_eq!(unqueued_outcome, Ok(()));

    // Manually settle appended receipt as delivered and await exact outcome
    session.settle_queued_custom_receipt(&appended_msg, Ok(()));
    let appended_outcome = tokio::time::timeout(std::time::Duration::from_secs(5), appended_rx)
        .await
        .expect("appended receipt did not settle within timeout")
        .expect("appended oneshot closed");
    assert_eq!(appended_outcome, Ok(()));
}

#[test]
fn setting_a_queue_mode_persists_and_emits() {
    let session = test_session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let _subscription = session.subscribe(Arc::new(move |event| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
    }));
    session.set_steering_mode(maho_agent::types::QueueMode::All);
    assert_eq!(session.steering_mode(), maho_agent::types::QueueMode::All);
    let emitted = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(emitted.iter().any(|event| matches!(
        event,
        AgentSessionEvent::SessionSettingsChanged { steering_mode, .. } if steering_mode == "all"
    )));
    assert_eq!(
        session.with_settings_manager(|manager| manager.get_string("steeringMode")),
        Some("all".to_owned())
    );
}

#[test]
fn auto_compaction_override_emits_settings_without_changing_persisted_defaults() {
    let session = test_session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        lock(&sink).push(event.clone());
    }));
    session.set_auto_compaction_enabled(false);
    assert!(!session.auto_compaction_enabled());
    assert!(session.with_settings_manager(|manager| manager.get_value("compaction").is_none()));
    assert!(matches!(
        lock(&events).last(),
        Some(AgentSessionEvent::SessionSettingsChanged {
            auto_compaction_enabled: false,
            ..
        })
    ));
}

/// The user texts of the agent's native queues, steering then follow-up (the order a turn delivers them in).
fn native_queued_texts(session: &AgentSession) -> Vec<String> {
    let mut messages = session
        .agent
        .queued_messages(maho_agent::types::PendingQueue::Steering);
    messages.extend(
        session
            .agent
            .queued_messages(maho_agent::types::PendingQueue::FollowUp),
    );
    messages.iter().map(user_message_text).collect()
}

/// Delivers the native queue's next message as a turn would: the agent's `message_start` of that user message.
async fn deliver_next_queued(session: &AgentSession) {
    let message = session
        .agent
        .peek_queued_messages(true)
        .into_iter()
        .next()
        .expect("a queued message");
    assert!(session.agent.remove_queued_message(&message));
    session
        .process_agent_event(
            maho_agent::types::AgentEvent::MessageStart {
                message,
                streaming: None,
            },
            maho_ai::utils::abort::AbortController::new().signal(),
        )
        .await;
}

/// senpi test/suite/regressions/1971-queued-input-dequeue-edit.test.ts: "removes only the dequeued steer from
/// both queues and delivers the other".
#[tokio::test]
async fn dequeue_removes_only_that_steer_from_both_queues() {
    let session = test_session();
    session
        .steer("drop me", None, QueuedInputOptions::default())
        .await
        .expect("steer");
    session
        .steer("keep me", None, QueuedInputOptions::default())
        .await
        .expect("steer");
    let queued = session.get_queued_inputs();
    let (first, second) = (queued[0].clone(), queued[1].clone());
    assert!(first.queued_id.starts_with("q_") && first.queued_id[2..].parse::<u64>().is_ok());
    assert_ne!(second.queued_id, first.queued_id);

    assert_eq!(
        session.dequeue_queued_input(&first.queued_id, None),
        QueuedInputChange::Removed
    );
    assert_eq!(session.get_steering_messages(), ["keep me"]);
    assert_eq!(
        session
            .get_queued_inputs()
            .iter()
            .map(|input| input.queued_id.clone())
            .collect::<Vec<_>>(),
        std::slice::from_ref(&second.queued_id)
    );
    assert_eq!(native_queued_texts(&session), ["keep me"]);
    deliver_next_queued(&session).await;

    assert_eq!(
        session.dequeue_queued_input(&second.queued_id, None),
        QueuedInputChange::AlreadyDelivered
    );
    assert_eq!(
        session.dequeue_queued_input("q_999", None),
        QueuedInputChange::NotFound
    );
}

/// senpi 1971: "delivers the edited text in the input's original place".
#[tokio::test]
async fn an_edit_keeps_the_input_in_its_original_place() {
    let session = test_session();
    session
        .steer("first steer", None, QueuedInputOptions::default())
        .await
        .expect("steer");
    session
        .follow_up("typo follw-up", None, QueuedInputOptions::default())
        .await
        .expect("follow_up");
    let follow_up = session
        .get_queued_inputs()
        .into_iter()
        .find(|input| input.mode == StreamingBehavior::FollowUp)
        .expect("follow-up");

    assert_eq!(
        session.edit_queued_input(&follow_up.queued_id, "fixed follow-up", None),
        QueuedInputChange::Edited
    );
    assert_eq!(session.get_follow_up_messages(), ["fixed follow-up"]);
    assert_eq!(
        native_queued_texts(&session),
        ["first steer", "fixed follow-up"]
    );
}

/// senpi 1971: "tells two queued inputs with the same text apart".
#[tokio::test]
async fn two_queued_inputs_with_the_same_text_are_told_apart() {
    let session = test_session();
    session
        .steer("same", None, QueuedInputOptions::default())
        .await
        .expect("steer");
    session
        .steer("same", None, QueuedInputOptions::default())
        .await
        .expect("steer");
    let second = session.get_queued_inputs()[1].clone();

    assert_eq!(
        session.edit_queued_input(&second.queued_id, "changed", None),
        QueuedInputChange::Edited
    );
    assert_eq!(session.get_steering_messages(), ["same", "changed"]);
    assert_eq!(native_queued_texts(&session), ["same", "changed"]);
}

/// senpi test/suite/regressions/3459-queued-input-expected-revision.test.ts: "starts every queued input at
/// revision 1 and bumps it on each edit".
#[tokio::test]
async fn every_queued_input_starts_at_revision_one_and_each_edit_bumps_it() {
    let session = test_session();
    session
        .steer("draft", None, QueuedInputOptions::default())
        .await
        .expect("steer");
    let input = session.get_queued_inputs()[0].clone();
    assert_eq!(input.revision, 1);

    assert_eq!(
        session.edit_queued_input(&input.queued_id, "second", Some(1)),
        QueuedInputChange::Edited
    );
    assert_eq!(session.get_queued_inputs()[0].revision, 2);
    assert_eq!(
        session.edit_queued_input(&input.queued_id, "third", None),
        QueuedInputChange::Edited
    );
    assert_eq!(session.get_queued_inputs()[0].revision, 3);
    assert_eq!(native_queued_texts(&session), ["third"]);
}

/// senpi 3459: "refuses the second of two edits based on the same revision and keeps the first".
#[tokio::test]
async fn a_second_edit_on_the_same_revision_conflicts_and_the_first_is_kept() {
    let session = test_session();
    session
        .steer("draft", None, QueuedInputOptions::default())
        .await
        .expect("steer");
    let seen = session.get_queued_inputs()[0].clone();

    assert_eq!(
        session.edit_queued_input(&seen.queued_id, "client B", Some(seen.revision)),
        QueuedInputChange::Edited
    );
    assert_eq!(
        session.edit_queued_input(&seen.queued_id, "client A", Some(seen.revision)),
        QueuedInputChange::Conflict
    );
    let current = session.get_queued_inputs();
    assert_eq!(current.len(), 1);
    assert_eq!(
        (
            current[0].queued_id.as_str(),
            current[0].text.as_str(),
            current[0].revision
        ),
        (seen.queued_id.as_str(), "client B", 2)
    );
    assert_eq!(session.get_steering_messages(), ["client B"]);
    assert_eq!(native_queued_texts(&session), ["client B"]);
}

/// senpi 3459: "refuses a dequeue based on a revision another edit replaced, then removes it at the current
/// revision".
#[tokio::test]
async fn a_stale_dequeue_conflicts_and_the_current_revision_removes() {
    let session = test_session();
    session
        .follow_up("draft", None, QueuedInputOptions::default())
        .await
        .expect("follow_up");
    let seen = session.get_queued_inputs()[0].clone();
    assert_eq!(
        session.edit_queued_input(&seen.queued_id, "edited by B", Some(1)),
        QueuedInputChange::Edited
    );

    assert_eq!(
        session.dequeue_queued_input(&seen.queued_id, Some(1)),
        QueuedInputChange::Conflict
    );
    assert_eq!(session.get_follow_up_messages(), ["edited by B"]);
    assert_eq!(
        session.dequeue_queued_input(&seen.queued_id, Some(2)),
        QueuedInputChange::Removed
    );
    assert!(native_queued_texts(&session).is_empty());
}

/// senpi 3459: "answers not_found or already_delivered rather than conflict once the input is gone".
#[tokio::test]
async fn a_gone_input_answers_not_found_or_already_delivered_not_conflict() {
    let session = test_session();
    session
        .steer("removed first", None, QueuedInputOptions::default())
        .await
        .expect("steer");
    session
        .steer("delivered", None, QueuedInputOptions::default())
        .await
        .expect("steer");
    let queued = session.get_queued_inputs();
    let (removed, delivered) = (queued[0].clone(), queued[1].clone());
    assert_eq!(
        session.dequeue_queued_input(&removed.queued_id, None),
        QueuedInputChange::Removed
    );
    assert_eq!(
        session.edit_queued_input(&removed.queued_id, "late", Some(7)),
        QueuedInputChange::NotFound
    );
    deliver_next_queued(&session).await;
    assert_eq!(
        session.edit_queued_input(&delivered.queued_id, "late", Some(7)),
        QueuedInputChange::AlreadyDelivered
    );
    assert_eq!(
        session.dequeue_queued_input(&delivered.queued_id, Some(7)),
        QueuedInputChange::AlreadyDelivered
    );
}

/// senpi 3459: "refuses a stale revision on an input held back after a compaction, and edits it at the current
/// one".
#[tokio::test]
async fn a_deferred_post_compaction_input_is_revision_checked_and_edited() {
    let session = test_session();
    // The post-compaction window: queued input waits in the deferred lists until the next prompt starts.
    {
        let mut state = session.state();
        state.prompt_start_pending = true;
        state.post_compaction_assistant_generation = Some(0);
    }
    session
        .follow_up("deferred draft", None, QueuedInputOptions::default())
        .await
        .expect("follow_up");
    assert!(native_queued_texts(&session).is_empty());
    let seen = session.get_queued_inputs()[0].clone();
    assert_eq!(
        session.edit_queued_input(&seen.queued_id, "edited once", Some(1)),
        QueuedInputChange::Edited
    );

    assert_eq!(
        session.edit_queued_input(&seen.queued_id, "stale edit", Some(1)),
        QueuedInputChange::Conflict
    );
    assert_eq!(
        session.dequeue_queued_input(&seen.queued_id, Some(1)),
        QueuedInputChange::Conflict
    );
    let current = session.get_queued_inputs();
    assert_eq!(
        (current[0].text.as_str(), current[0].revision),
        ("edited once", 2)
    );
    assert_eq!(session.get_follow_up_messages(), ["edited once"]);
    assert_eq!(
        session.edit_queued_input(&seen.queued_id, "edited twice", Some(2)),
        QueuedInputChange::Edited
    );
    let current = session.get_queued_inputs();
    assert_eq!(
        (current[0].text.as_str(), current[0].revision),
        ("edited twice", 3)
    );
    let deferred = session
        .state()
        .post_compaction_deferred_follow_up_messages
        .clone();
    assert_eq!(
        deferred.iter().map(user_message_text).collect::<Vec<_>>(),
        ["edited twice"]
    );
}

/// senpi `getPendingInputs` with nothing in durable custody: no inputs, and every known id `not_admitted`.
#[tokio::test]
async fn pending_inputs_answer_every_known_id_not_admitted_when_nothing_is_held() {
    let session = test_session();
    session
        .steer("plain", None, QueuedInputOptions::default())
        .await
        .expect("steer");
    let snapshot = session
        .get_pending_inputs(&["c-1".to_owned(), "never-sent".to_owned()])
        .await
        .expect("snapshot");
    assert!(snapshot.inputs.is_empty());
    assert_eq!(snapshot.through_cursor, 0);
    assert!(!snapshot.lifetime_id.is_empty());
    assert_eq!(
        snapshot.outcomes,
        vec![
            crate::pending_input_session::PendingInputOutcome::NotAdmitted {
                client_message_id: "c-1".into()
            },
            crate::pending_input_session::PendingInputOutcome::NotAdmitted {
                client_message_id: "never-sent".into()
            },
        ]
    );
}

#[tokio::test]
async fn queued_user_start_updates_queue_before_message_event() {
    let session = test_session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        lock(&sink).push(event.clone());
    }));
    session
        .steer("queued", None, QueuedInputOptions::default())
        .await
        .expect("steer");
    assert_eq!(session.get_steering_messages(), vec!["queued"]);
    session
        .process_agent_event(
            maho_agent::types::AgentEvent::MessageStart {
                message: make_user_message("queued", None),
                streaming: None,
            },
            maho_ai::utils::abort::AbortController::new().signal(),
        )
        .await;
    assert_eq!(session.pending_message_count(), 0);
    let events = lock(&events);
    assert!(
        matches!(&events[1], AgentSessionEvent::QueueUpdate { steering, .. } if steering.is_empty())
    );
    assert!(matches!(
        &events[2],
        AgentSessionEvent::Agent(maho_agent::types::AgentEvent::MessageStart { .. })
    ));
}

#[tokio::test]
async fn aborting_pending_input_emits_session_abort_once() {
    let session = test_session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        lock(&sink).push(event.clone());
    }));
    session
        .follow_up("later", None, QueuedInputOptions::default())
        .await
        .expect("follow up");
    session.clear_queue(true);
    session.abort().await;
    session.abort().await;
    assert_eq!(
        lock(&events)
            .iter()
            .filter(|event| matches!(event, AgentSessionEvent::SessionAbort))
            .count(),
        1
    );
}

#[tokio::test]
async fn aborting_retry_backoff_emits_session_abort() {
    let session = test_session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        lock(&sink).push(event.clone());
    }));
    let backoff = maho_ai::utils::abort::AbortController::new();
    session.state().retry_abort_controller = Some(backoff.clone());
    session.abort().await;
    assert!(backoff.signal().aborted());
    assert_eq!(
        lock(&events)
            .iter()
            .filter(|event| matches!(event, AgentSessionEvent::SessionAbort))
            .count(),
        1
    );
}

#[tokio::test]
async fn aborting_idle_compaction_emits_session_abort() {
    let session = test_session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        lock(&sink).push(event.clone());
    }));
    session.state().compaction_abort_controller =
        Some(crate::compaction::lifecycle::CompactionAbortController::new());
    session.abort().await;
    assert_eq!(
        lock(&events)
            .iter()
            .filter(|event| matches!(event, AgentSessionEvent::SessionAbort))
            .count(),
        1
    );
}

#[test]
fn clamping_prefers_the_next_higher_available_level() {
    let available = [
        ModelThinkingLevel::Off,
        ModelThinkingLevel::Low,
        ModelThinkingLevel::Max,
    ];
    assert_eq!(
        clamp_thinking_level(ModelThinkingLevel::Medium, &available),
        ModelThinkingLevel::Max
    );
    assert_eq!(
        clamp_thinking_level(ModelThinkingLevel::Minimal, &available),
        ModelThinkingLevel::Low
    );
    assert_eq!(
        clamp_thinking_level(ModelThinkingLevel::Off, &available),
        ModelThinkingLevel::Off
    );
    assert_eq!(
        clamp_thinking_level(ModelThinkingLevel::Max, &[ModelThinkingLevel::Off]),
        ModelThinkingLevel::Off
    );
}

#[test]
fn only_non_off_levels_reach_the_thinking_level_event() {
    assert_eq!(
        thinking_level_from_model_level(ModelThinkingLevel::Off),
        None
    );
    assert_eq!(
        thinking_level_from_model_level(ModelThinkingLevel::High),
        Some(ThinkingLevel::High)
    );
}

#[test]
fn a_non_reasoning_model_supports_no_thinking() {
    let session = test_session();
    assert!(!session.supports_thinking());
    assert_eq!(session.cycle_thinking_level(), None);
    assert_eq!(
        session.get_available_thinking_levels(),
        vec![ModelThinkingLevel::Off]
    );
}

#[test]
fn a_model_switch_clamps_the_default_to_the_supported_levels() {
    let session = test_session();
    assert_eq!(
        session
            .get_thinking_for_model_switch(&test_model(), None, None)
            .0,
        ModelThinkingLevel::Off
    );
    assert_eq!(
        session.get_thinking_for_model_switch(&test_model(), Some(ModelThinkingLevel::Xhigh), None),
        (
            ModelThinkingLevel::Off,
            Some(explicit_thinking_selection(ModelThinkingLevel::Off))
        )
    );
}

#[test]
fn a_models_thinking_level_names_the_candidate_it_came_from() {
    use crate::session_thinking_memory::ThinkingLevelSource;
    let session = test_session();
    let mut reasoning = test_model();
    reasoning.id = "faux-other".into();
    reasoning.reasoning = true;
    let mut plain = test_model();
    plain.id = "faux-plain".into();
    let (level, source, _) = session.thinking_level_for_model(&reasoning);
    assert_eq!(
        (level, source),
        (ModelThinkingLevel::Medium, ThinkingLevelSource::Default)
    );
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "modelThinkingLevels".into(),
            serde_json::json!({"faux/faux-other": "low", "faux/faux-plain": "high"}),
        )]));
    });
    let (level, source, _) = session.thinking_level_for_model(&reasoning);
    assert_eq!(
        (level, source),
        (ModelThinkingLevel::Low, ThinkingLevelSource::Global)
    );
    // A remembered level the model cannot run is clamped, and says so.
    let (level, source, available) = session.thinking_level_for_model(&plain);
    assert_eq!(
        (level, source, available),
        (
            ModelThinkingLevel::Off,
            ThinkingLevelSource::Clamped,
            vec![ModelThinkingLevel::Off]
        )
    );
}

#[test]
fn continue_from_leaf_refuses_an_empty_session_and_a_prompt_leaf() {
    use crate::continue_from_leaf::ContinueFromLeafCode;
    let session = test_session();
    assert_eq!(
        session
            .continue_from_leaf_refusal()
            .map(|refusal| refusal.code),
        Some(ContinueFromLeafCode::NothingToContinue)
    );
    let prompt: AgentMessage = serde_json::from_value(
        serde_json::json!({"role": "user", "content": "hi", "timestamp": 0}),
    )
    .expect("message");
    session.agent.set_messages(vec![prompt]);
    assert_eq!(
        session
            .continue_from_leaf_refusal()
            .map(|refusal| refusal.code),
        Some(ContinueFromLeafCode::LeafNotAssistant)
    );
}

fn reasoning_faux_session(models: Vec<(&str, u64)>) -> (AgentSession, Vec<Model>) {
    use maho_ai::providers::faux::{
        FauxModelDefinition, RegisterFauxProviderOptions, faux_provider,
    };
    let provider = faux_provider(RegisterFauxProviderOptions {
        models: Some(
            models
                .into_iter()
                .map(|(id, window)| FauxModelDefinition {
                    id: id.to_owned(),
                    reasoning: Some(true),
                    context_window: Some(window),
                    max_tokens: Some(1_024),
                    ..Default::default()
                })
                .collect(),
        ),
        ..Default::default()
    })
    .expect("valid faux provider");
    let session = test_session();
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    let models = provider.models.clone();
    session.agent.set_model(models[0].clone());
    (session, models)
}

#[tokio::test]
async fn a_thinking_change_dispatches_thinking_level_select_to_extensions() {
    let (session, _) = reasoning_faux_session(vec![("think-1", 128_000)]);
    session.agent.set_thinking_level(ModelThinkingLevel::Low);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:thinking>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::ThinkingLevelSelect,
        vec![Arc::new(move |event, _| {
            if let maho_ext_api::ExtensionEvent::ThinkingLevelSelect {
                level,
                previous_level,
            } = event
            {
                let _ = sender.send((*level, *previous_level));
            }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    session.set_session_thinking_level(ModelThinkingLevel::High);
    let received = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .expect("bounded dispatch");
    session.dispose().await;
    assert_eq!(
        received,
        Some((ModelThinkingLevel::High, ModelThinkingLevel::Low))
    );
}

#[tokio::test]
async fn a_persistent_thinking_change_records_selection_and_global_defaults() {
    let (session, models) = reasoning_faux_session(vec![("think-1", 128_000)]);
    session.set_thinking_level(ModelThinkingLevel::High);
    assert_eq!(
        session.thinking_selection(),
        Some(explicit_thinking_selection(ModelThinkingLevel::High))
    );
    let key = format!("{}/{}", models[0].provider, models[0].id);
    let (default, last_on) = session.with_settings_manager(|manager| {
        (
            manager.get_string("defaultThinkingLevel"),
            manager
                .get_value("modelLastOnThinkingLevels")
                .and_then(|levels| levels.get(&key))
                .cloned(),
        )
    });
    assert_eq!(default.as_deref(), Some("high"));
    assert_eq!(last_on, Some(Value::from("high")));
    let entry = session
        .with_session_manager(|manager| manager.entries())
        .into_iter()
        .rev()
        .find(|entry| entry["type"] == "thinking_level_change")
        .expect("thinking entry");
    assert_eq!(
        entry["thinkingSelection"],
        serde_json::json!({"level": "high", "source": "explicit"})
    );
}

#[tokio::test]
async fn an_astra_thinking_change_sets_the_reasoning_baseline_and_configuration_update() {
    let (session, mut models) = reasoning_faux_session(vec![("think-1", 128_000)]);
    let mut astra = models.remove(0);
    astra.id = "gpt-6-astra".to_owned();
    astra.provider = "openai".to_owned();
    session.agent.set_model(astra);
    session.agent.set_thinking_level(ModelThinkingLevel::Medium);
    session.set_session_thinking_level(ModelThinkingLevel::High);
    assert_eq!(
        session.agent.reasoning_baseline().as_deref(),
        Some("medium")
    );
    let entries = session.with_session_manager(|manager| manager.entries());
    assert!(
        entries
            .iter()
            .any(|entry| entry["type"] == "configuration_update"
                && entry["reasoning"]["effort"] == "high")
    );
    assert!(
        session
            .messages()
            .iter()
            .any(|message| message.role() == "configurationUpdate")
    );
}

#[test]
fn a_remembered_level_becomes_an_explicit_selection_on_model_switch() {
    let (session, models) = reasoning_faux_session(vec![("think-1", 128_000)]);
    session.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global,
        &Map::from_iter([("modelThinkingLevels".to_owned(), serde_json::json!({ format!("{}/{}", models[0].provider, models[0].id): "low" }))])))
        .expect("settings");
    assert_eq!(
        session.get_thinking_for_model_switch(&models[0], None, None),
        (
            ModelThinkingLevel::Low,
            Some(explicit_thinking_selection(ModelThinkingLevel::Low))
        )
    );
}

#[tokio::test]
async fn cycling_applies_the_favorite_entry_thinking_level() {
    let (session, models) =
        reasoning_faux_session(vec![("think-1", 128_000), ("think-2", 128_000)]);
    session.state().uses_default_stream_function = false;
    session.set_favorite_models(
        models
            .iter()
            .map(|model| SessionModelEntry {
                model: model.clone(),
                thinking_level: (model.id == "think-2").then_some(ThinkingLevel::High),
                thinking_selection: None,
                service_tier: None,
            })
            .collect(),
    );
    let result = session
        .cycle_model(true)
        .await
        .expect("cycle")
        .expect("result");
    assert_eq!(result.model.id, "think-2");
    assert_eq!(session.thinking_level(), ModelThinkingLevel::High);
    assert_eq!(
        session.thinking_selection(),
        Some(explicit_thinking_selection(ModelThinkingLevel::High))
    );
}

#[tokio::test]
async fn cycling_to_a_single_inadmissible_alternative_surfaces_the_budget_error() {
    let (session, models) =
        reasoning_faux_session(vec![("think-1", 128_000), ("think-tiny", 1_000)]);
    session.state().uses_default_stream_function = false;
    session.set_favorite_models(
        models
            .iter()
            .map(|model| SessionModelEntry {
                model: model.clone(),
                thinking_level: None,
                thinking_selection: None,
                service_tier: None,
            })
            .collect(),
    );
    let result = session.cycle_model(true).await;
    assert!(result.is_err(), "{result:?}");
    assert_eq!(session.model().id, "think-1");
}

#[test]
fn favorites_are_narrowed_by_the_scoped_set() {
    let session = test_session();
    let model = test_model();
    session.set_favorite_models(vec![SessionModelEntry {
        model: model.clone(),
        thinking_level: None,
        thinking_selection: None,
        service_tier: None,
    }]);
    session.set_scoped_models(vec![SessionModelEntry {
        model: model.clone(),
        thinking_level: None,
        thinking_selection: None,
        service_tier: None,
    }]);
    assert_eq!(session.favorite_models().len(), 1);
}

#[test]
fn a_favorite_outside_the_catalog_is_dropped() {
    let session = test_session();
    session.set_favorite_models(vec![SessionModelEntry {
        model: test_model(),
        thinking_level: None,
        thinking_selection: None,
        service_tier: None,
    }]);
    assert!(session.get_current_favorite_models().is_empty());
}

/// senpi `_emitServerFallbackAborted`: `chainConfigured` comes from the controller's canonical
/// chains, so a chain keyed by the bare model family counts for the session model.
#[tokio::test]
async fn server_fallback_abort_reports_a_family_keyed_chain_as_configured() {
    let session = retry_session(Vec::new(), 1);
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "retry".to_owned(),
            serde_json::json!({"enabled":true,"modelFallback":true,"fallbackChains":{"faux":["faux/faux-1"]}}),
        )]))
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::ServerFallbackAborted {
            chain_configured, ..
        } = event
        {
            lock(&sink).push(*chain_configured);
        }
    }));
    let mut message = maho_ai::providers::faux::faux_assistant_message("", Default::default());
    maho_ai::utils::server_fallback_receipt::apply_server_fallback_abort(
        &mut message,
        &maho_ai::utils::server_fallback_receipt::ServerFallbackReceipt {
            from: "faux-1".to_owned(),
            to: "faux-2".to_owned(),
        },
    );
    session.emit_server_fallback_aborted(&message);
    assert_eq!(*lock(&events), vec![true]);
}

#[test]
fn a_message_without_the_fallback_receipt_emits_nothing() {
    let session = test_session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let _subscription = session.subscribe(Arc::new(move |event| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
    }));
    let message: maho_ai::types::AssistantMessage = serde_json::from_value(serde_json::json!({
        "content": [], "api": "faux", "provider": "faux", "model": "faux-1",
        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
        "stopReason": "error", "timestamp": 0
    }))
    .expect("assistant");
    session.emit_server_fallback_aborted(&message);
    assert!(
        events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
}

#[test]
fn session_stats_aggregate_over_entries() {
    let session = test_session();
    session.append_session_entry(serde_json::json!({
        "id": "u1", "type": "message", "timestamp": "2026-01-01T00:00:00.000Z",
        "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }], "timestamp": 0 }
    }));
    session.append_session_entry(serde_json::json!({
        "id": "a1", "type": "message", "timestamp": "2026-01-01T00:00:01.000Z",
        "message": { "role": "assistant", "content": [{ "type": "text", "text": "hello" }], "api": "faux",
            "provider": "faux", "model": "faux-1", "stopReason": "stop", "timestamp": 1,
            "usage": { "input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 15,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0.5 } } }
    }));
    let stats = session.get_session_stats();
    assert_eq!(stats.user_messages, 1);
    assert_eq!(stats.assistant_messages, 1);
    assert_eq!(stats.total_messages, 2);
    assert_eq!(stats.tokens.input, 10);
    assert_eq!(stats.tokens.output, 5);
    assert_eq!(stats.tokens.total, 15);
    assert_eq!(stats.cost, 0.5);
    assert!(!stats.cost_unknown);

    session.append_session_entry(serde_json::json!({
        "id": "a2", "type": "message", "timestamp": "2026-01-01T00:00:02.000Z",
        "message": { "role": "assistant", "content": [{ "type": "text", "text": "unpriced" }], "api": "faux",
            "provider": "faux", "model": "faux-unpriced", "stopReason": "stop", "timestamp": 2,
            "usage": { "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2, "costUnknown": true,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } } }
    }));
    let stats = session.get_session_stats();
    assert!(stats.cost_unknown);
}

#[test]
fn message_fingerprint_formats_correctly() {
    let msg = AgentMessage::Llm(maho_ai::types::Message::User(maho_ai::types::UserMessage {
        content: maho_ai::types::UserContent::Text("hello".to_owned()),
        timestamp: 0,
    }));
    assert_eq!(
        message_fingerprint(Some(&msg)),
        "5::::undefined:undefined:false"
    );

    let mut asst = maho_ai::types::AssistantMessage {
        credential_served_recently: None,
        content: vec![maho_ai::types::ContentBlock::text("answer")],
        api: "a".into(),
        provider: "p".into(),
        model: "m".into(),
        response_model: None,
        response_id: None,
        thinking_level: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: maho_ai::types::Usage {
            input: 10,
            output: 20,
            total_tokens: 30,
            context_usage: Some(Some(maho_ai::types::ContextUsage {
                input: 15,
                output: 5,
            })),
            ..Default::default()
        },
        stop_reason: maho_ai::types::StopReason::Stop,
        stop_details: None,
        deferred: None,
        error_message: None,
        error_cause: None,
        provider_diagnostic: None,
        abort_source: None,
        failure_source: None,
        raw_stop_reason: None,
        end_turn: None,
        prompt_cache_diagnostics: None,
        prompt_cache_breakpoint: None,
        timing: None,
        timestamp: 1,
    };
    let asst_msg = AgentMessage::Llm(maho_ai::types::Message::Assistant(std::sync::Arc::new(
        asst.clone(),
    )));
    assert_eq!(
        message_fingerprint(Some(&asst_msg)),
        "7:10:20:30:15:5:false"
    );

    asst.usage.context_usage = Some(None);
    let asst_null = AgentMessage::Llm(maho_ai::types::Message::Assistant(std::sync::Arc::new(
        asst,
    )));
    assert_eq!(
        message_fingerprint(Some(&asst_null)),
        "7:10:20:30:undefined:undefined:true"
    );
}

#[test]
fn session_stats_incremental_fold_matches_full_scan() {
    let session = test_session();
    let full_scan = |session: &AgentSession| {
        session.state().session_stats_cache = None;
        session.get_session_stats()
    };
    let assistant = |id: &str, input: u64, tool_call: bool| {
        let mut content = vec![serde_json::json!({ "type": "text", "text": "ok" })];
        if tool_call {
            content.push(serde_json::json!({ "type": "toolCall", "id": format!("c-{id}"), "name": "read", "arguments": {} }));
        }
        serde_json::json!({
            "id": id, "type": "message", "timestamp": "2026-01-01T00:00:01.000Z",
            "message": { "role": "assistant", "content": content, "api": "faux",
                "provider": "faux", "model": "faux-1", "stopReason": "stop", "timestamp": 1,
                "usage": { "input": input, "output": 5, "cacheRead": 2, "cacheWrite": 1, "totalTokens": input + 8,
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0.25 } } }
        })
    };
    let user = |id: &str| {
        serde_json::json!({
            "id": id, "type": "message", "timestamp": "2026-01-01T00:00:00.000Z",
            "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }], "timestamp": 0 }
        })
    };
    let tool_result = |id: &str| {
        serde_json::json!({
            "id": id, "type": "message", "timestamp": "2026-01-01T00:00:02.000Z",
            "message": { "role": "toolResult", "toolCallId": "c", "toolName": "read",
                "content": [{ "type": "text", "text": "r" }], "isError": false, "timestamp": 2 }
        })
    };
    let steps: Vec<Vec<Value>> = vec![
        vec![user("u1")],
        vec![assistant("a1", 10, true), tool_result("t1")],
        vec![assistant("a2", 7, false)],
        vec![
            serde_json::json!({ "id": "s1", "type": "session_info", "timestamp": "2026-01-01T00:00:03.000Z", "name": "x" }),
        ],
        vec![
            user("u2"),
            assistant("a3", 3, true),
            tool_result("t2"),
            user("u3"),
        ],
    ];
    let mut previous = session.get_session_stats();
    for step in steps {
        for entry in step {
            session.append_session_entry(entry);
        }
        previous = session.get_session_stats();
        assert_eq!(previous, full_scan(&session));
    }
    assert_eq!(previous.total_messages, 8);
    assert_eq!(previous.tool_calls, 2);
    assert_eq!(previous.tool_results, 2);
    assert_eq!(previous.tokens.input, 20);

    // A session switch must rescan rather than fold onto the previous session's totals.
    session.with_session_manager_mut(|manager| manager.new_session(None));
    session.append_session_entry(user("n1"));
    let switched = session.get_session_stats();
    assert_eq!(switched, full_scan(&session));
    assert_eq!(switched.total_messages, 1);
    assert_eq!(switched.tokens.input, 0);
    session.append_session_entry(assistant("n2", 4, false));
    let appended = session.get_session_stats();
    assert_eq!(appended, full_scan(&session));
    assert_eq!(appended.tokens.input, 4);
}

#[test]
fn forking_lists_user_messages_in_order() {
    let session = test_session();
    session.append_session_entry(serde_json::json!({
        "id": "u1", "type": "message", "timestamp": "2026-01-01T00:00:00.000Z",
        "message": { "role": "user", "content": [{ "type": "text", "text": "first" }], "timestamp": 0 }
    }));
    session.append_session_entry(serde_json::json!({
        "id": "u2", "type": "message", "timestamp": "2026-01-01T00:00:01.000Z",
        "message": { "role": "user", "content": [{ "type": "text", "text": "second" }], "timestamp": 1 }
    }));
    assert_eq!(
        session.get_user_messages_for_forking(),
        vec![
            ("u1".to_owned(), "first".to_owned()),
            ("u2".to_owned(), "second".to_owned())
        ]
    );
}

#[test]
fn a_model_with_a_context_window_reports_usage() {
    let session = test_session();
    assert!(
        session.get_context_usage().is_none(),
        "the default agent model has no context window"
    );
    session.agent().set_model(test_model());
    let usage = session.get_context_usage().expect("usage");
    assert_eq!(usage.context_window, 128_000);
    assert!(usage.tokens.is_some());
}

#[test]
fn exporting_uses_selected_leaf_and_empty_selection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let session = test_session();
    let selected = session.with_session_manager_mut(|manager| {
        let selected = manager
            .append_message(serde_json::json!({"role":"user","content":"selected","timestamp":0}));
        manager
            .append_message(serde_json::json!({"role":"user","content":"abandoned","timestamp":1}));
        selected["id"].as_str().expect("id").to_owned()
    });
    for leaf in [Some(selected.as_str()), None] {
        session
            .with_session_manager_mut(|manager| manager.set_leaf(leaf))
            .expect("leaf");
        let path = dir.path().join("selected.jsonl");
        session
            .export_to_jsonl(Some(path.to_str().expect("path")))
            .expect("export");
        let entries = crate::session_manager::load_entries_from_file(path.to_str().expect("path"));
        assert_eq!(entries.len(), if leaf.is_some() { 2 } else { 1 });
        if leaf.is_some() {
            assert_eq!(entries[1]["id"], selected);
        }
    }
}

#[test]
fn exporting_linearizes_the_branch_parent_ids() {
    let dir = tempfile::tempdir().expect("tempdir");
    let session = test_session();
    session.append_session_entry(serde_json::json!({
        "id": "u1", "type": "message", "timestamp": "2026-01-01T00:00:00.000Z",
        "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }], "timestamp": 0 }
    }));
    let path = dir.path().join("out.jsonl");
    let written = session
        .export_to_jsonl(Some(path.to_str().expect("path")))
        .expect("export");
    let content = std::fs::read_to_string(&written).expect("read");
    let mut lines = content.lines();
    let header: Value = serde_json::from_str(lines.next().expect("header")).expect("header json");
    assert_eq!(header.get("type").and_then(Value::as_str), Some("session"));
    let first: Value = serde_json::from_str(lines.next().expect("entry")).expect("entry json");
    assert!(first.get("parentId").expect("parentId").is_null());
}

#[test]
fn the_session_name_change_is_published() {
    let session = test_session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let _subscription = session.subscribe(Arc::new(move |event| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
    }));
    session.set_session_name("lane 21");
    assert_eq!(session.session_name(), Some("lane 21".to_owned()));
    assert!(
        events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|event| matches!(
                event,
                AgentSessionEvent::SessionInfoChanged { name } if name.as_deref() == Some("lane 21")
            ))
    );
}

fn test_tool(name: &str) -> AgentTool {
    let tool = maho_ai::types::Tool {
        name: name.to_owned(),
        description: format!("{name} tool"),
        parameters: serde_json::json!({ "type": "object" }),
        output_schema: None,
        freeform: None,
        constrained_sampling: None,
    };
    AgentTool {
        label: name.to_owned(),
        prepare_arguments: None,
        execute: Arc::new(|_, _, _, _| Box::pin(async { AgentToolResult::text("ok") })),
        replay: None,
        execution_mode: None,
        tool,
    }
}

#[tokio::test]
async fn direct_tool_execution_validates_and_coerces_prepared_arguments() {
    let session = test_session();
    let mut tool = test_tool("validated");
    tool.tool.parameters = serde_json::json!({"type":"object", "properties":{"count":{"type":"number"}}, "required":["count"]});
    tool.execute = Arc::new(|_, args, _, _| {
        Box::pin(async move {
            AgentToolResult {
                details: args,
                terminate: Some(true),
                ..AgentToolResult::text("executed")
            }
        })
    });
    session.agent().set_tools(vec![tool]);
    let error = session
        .execute_tool("validated", serde_json::json!({}), Default::default())
        .await
        .expect_err("required argument");
    assert_eq!(error.code, "invalid_params");
    assert_eq!(error.tool_name, "validated");
    let result = session
        .execute_tool(
            "validated",
            serde_json::json!({"count":"3"}),
            Default::default(),
        )
        .await
        .expect("coerced argument");
    assert_eq!(result.details, serde_json::json!({"count":3}));
    assert_eq!(result.terminate, Some(true));
}

#[tokio::test]
async fn next_turn_asides_stay_out_of_history_until_admitted() {
    let session = test_session();
    session.agent().set_model(test_model());
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    maho_ext_api::ExtensionActions::send_message(
        &actions,
        maho_ext_api::CustomMessage {
            custom_type: "aside".to_owned(),
            content: vec![maho_tools::definition::ToolContent::text(
                "x".repeat(600_000),
            )],
            display: false,
            details: None,
        },
        maho_ext_api::SendMessageOptions {
            deliver_as: Some(maho_ext_api::DeliverAs::NextTurn),
            trigger_turn: false,
        },
    )
    .expect("queue aside");
    assert!(session.messages().is_empty());
    assert!(
        session
            .with_session_manager(|manager| manager.entries())
            .is_empty()
    );
    assert_eq!(session.state().pending_next_turn_messages.len(), 1);
    let additions = session.state().pending_next_turn_messages.clone();
    assert!(
        session
            .enforce_final_provider_admission(&additions)
            .await
            .is_err()
    );
    assert_eq!(session.state().pending_next_turn_messages.len(), 1);
    assert!(
        session
            .enforce_final_provider_admission(&[make_user_message(&"x".repeat(600_000), None)])
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn prompt_delivers_next_turn_aside_once_after_user_message() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "reply",
            Default::default(),
        )],
        0,
    );
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    maho_ext_api::ExtensionActions::send_message(
        &actions,
        maho_ext_api::CustomMessage {
            custom_type: "aside".to_owned(),
            content: vec![maho_tools::definition::ToolContent::text("remember")],
            display: false,
            details: None,
        },
        maho_ext_api::SendMessageOptions {
            deliver_as: Some(maho_ext_api::DeliverAs::NextTurn),
            trigger_turn: false,
        },
    )
    .expect("queue");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("input", Default::default()),
    )
    .await
    .expect("bounded prompt")
    .expect("prompt");
    let messages = session.messages();
    assert_eq!(
        messages.iter().map(AgentMessage::role).collect::<Vec<_>>(),
        ["user", "custom", "assistant"]
    );
    assert!(session.state().pending_next_turn_messages.is_empty());
    let entries = session.with_session_manager(|manager| manager.entries());
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry["type"] == "custom_message" && entry["customType"] == "aside")
            .count(),
        1
    );
}

#[tokio::test]
async fn rejected_prompt_restores_next_turn_asides_without_provider_call() {
    let session = test_session_with_stream_function(false);
    session.agent().set_model(test_model());
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(
        maho_ai::providers::faux::faux_provider(Default::default())
            .expect("valid faux provider")
            .provider,
    );
    let calls = Arc::new(AtomicU64::new(0));
    let captured = calls.clone();
    session
        .agent()
        .set_stream_function(Arc::new(move |_, _, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
            stream.end(Some(maho_ai::providers::faux::faux_assistant_message(
                "unexpected",
                Default::default(),
            )));
            stream
        }));
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    for (kind, content) in [
        ("oversized", "x".repeat(600_000)),
        ("second", "retained".to_owned()),
    ] {
        maho_ext_api::ExtensionActions::send_message(
            &actions,
            maho_ext_api::CustomMessage {
                custom_type: kind.to_owned(),
                content: vec![maho_tools::definition::ToolContent::text(content)],
                display: false,
                details: None,
            },
            maho_ext_api::SendMessageOptions {
                deliver_as: Some(maho_ext_api::DeliverAs::NextTurn),
                trigger_turn: false,
            },
        )
        .expect("queue");
    }
    for _ in 0..2 {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session.prompt("input", Default::default()),
        )
        .await
        .expect("bounded rejection")
        .expect_err("required compaction");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(session.messages().is_empty());
        assert!(
            session
                .with_session_manager(|manager| conversation_entries(manager.entries()))
                .is_empty()
        );
        let state = session.state();
        let kinds = state
            .pending_next_turn_messages
            .iter()
            .map(|message| match message {
                AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(message)) => {
                    message.custom_type.as_str()
                }
                _ => panic!("custom aside"),
            })
            .collect::<Vec<_>>();
        assert_eq!(kinds, ["oversized", "second"]);
    }
}

#[test]
fn hook_source_context_uses_branded_json_paths() {
    let session = test_session();
    let sources = maho_ext_api::ExtensionContextActions::get_loaded_hook_sources(
        &SessionExtensionActions(Arc::downgrade(&session.inner)),
    );
    assert_eq!(
        sources.global_hooks_path,
        std::path::Path::new(&session.agent_dir()).join("hooks.json")
    );
    assert_eq!(
        sources.project_hooks_path,
        std::path::Path::new(&session.cwd())
            .join(crate::config::config_dir_name())
            .join("hooks.json")
    );
}

#[test]
fn compaction_context_reports_model_and_session_overrides() {
    let session = test_session();
    session.agent().set_model(test_model());
    session.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global,
        &Map::from_iter([("compaction".to_owned(), serde_json::json!({"enabled":true,"reserveTokens":100,
            "keepRecentTokens":200,"modelOverrides":{"faux/faux-1":{"reserveTokens":300,"keepRecentTokens":400}}}))])))
        .expect("settings");
    session.set_auto_compaction_enabled(false);
    let settings = maho_ext_api::ExtensionContextActions::get_compaction_settings(
        &SessionExtensionActions(Arc::downgrade(&session.inner)),
    );
    assert!(!settings.enabled);
    assert_eq!(settings.reserve_tokens, 300);
    assert_eq!(settings.keep_recent_tokens, 400);
}

fn test_definition(name: &str) -> ToolDefinition {
    ToolDefinition::new(
        name,
        &format!("{name} tool"),
        serde_json::json!({ "type": "object" }),
        Arc::new(|_| Box::pin(async { Ok(maho_tools::definition::ToolResult::text("ok")) })),
    )
}

#[tokio::test]
async fn lazy_activation_releases_session_state_before_extension_callback() {
    let session = test_session();
    session.register_tool_definition(
        test_definition("hidden"),
        empty_source_info(),
        test_tool("hidden"),
    );
    let weak = Arc::downgrade(&session.inner);
    session.add_lazy_tool_activator(Arc::new(move |name| {
        let inner = weak.upgrade().expect("live session");
        assert!(
            inner.state.try_lock().is_ok(),
            "extension callbacks must not inherit the session lock"
        );
        AgentSession { inner }.set_active_tools_by_name(vec![name.to_owned()]);
        true
    }));
    let result = session
        .execute_tool(
            "hidden",
            serde_json::json!({}),
            ExecuteToolOptions {
                activate_inactive_tool: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("lazy tool executes");
    assert!(!result.content.is_empty());
    assert!(
        session
            .get_active_tool_names()
            .contains(&"hidden".to_owned())
    );
}

#[test]
fn an_unresolvable_tool_name_is_returned_unchanged() {
    let session = test_session();
    assert_eq!(session.resolve_tool_call_name("missing"), "missing");
}

#[test]
fn registered_tools_are_reachable_by_name() {
    let session = test_session();
    session.register_tool_definition(
        test_definition("read"),
        empty_source_info(),
        test_tool("read"),
    );
    assert!(session.get_registered_tool("read").is_some());
    assert!(session.get_tool_definition("read").is_some());
    assert_eq!(session.resolve_tool_call_name("read"), "read");
}

#[test]
fn late_extension_tool_registration_installs_into_live_agent() {
    use maho_ext_api::ExtensionSessionActions;
    let session = test_session();
    let runtime = maho_ext_api::ExtensionRuntime::default();
    let factory: maho_ext_host::wrapper::ToolContextFactory =
        Arc::new(|| Err(maho_ext_api::ExtensionFailure::new("unused context")));
    session.state().extension_tool_context = Some((runtime, factory));
    SessionExtensionActions(Arc::downgrade(&session.inner))
        .install_registered_tool(maho_ext_api::RegisteredTool {
            definition: test_definition("late"),
            source_info: empty_source_info(),
        })
        .unwrap();
    assert!(session.get_registered_tool("late").is_some());
    assert!(session.get_active_tool_names().contains(&"late".to_owned()));
}

#[test]
fn unknown_names_are_ignored_when_setting_active_tools() {
    let session = test_session();
    session.register_tool_definition(
        test_definition("read"),
        empty_source_info(),
        test_tool("read"),
    );
    let revision = session.message_revision();
    session.set_active_tools_by_name(vec!["read".to_owned(), "nope".to_owned()]);
    assert_eq!(session.get_active_tool_names(), vec!["read".to_owned()]);
    assert!(!session.system_prompt().is_empty());
    assert_eq!(session.message_revision(), revision + 1);
    session.state().system_prompt_override = Some("turn hook override".to_owned());
    session.set_active_tools_by_name(vec!["read".to_owned()]);
    assert_eq!(session.system_prompt(), "turn hook override");
    assert_eq!(session.message_revision(), revision + 1);
}

#[tokio::test]
async fn active_tool_change_rebuilds_prompt_in_next_provider_request() {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(vec![
        maho_ai::providers::faux::faux_assistant_message("done", Default::default()).into(),
    ]);
    let streams = faux_streams(provider.core.clone());
    let session = test_session_with_stream_function(false);
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    let mut definition = test_definition("read");
    definition.prompt_snippet = Some("fixture-active-tool-contributor".to_owned());
    session.register_tool_definition(definition, empty_source_info(), test_tool("read"));
    session.set_active_tools_by_name(Vec::new());
    let old_prompt = session.system_prompt();
    let received = Arc::new(Mutex::new(Vec::new()));
    let observed = received.clone();
    session
        .agent
        .set_stream_function(Arc::new(move |model, context, options| {
            lock(&observed).push(maho_ai::utils::transcript::get_current_system_prompt(
                &context.messages,
            ));
            streams.stream_simple(model, context, options.map(|options| options.simple))
        }));

    session.set_active_tools_by_name(vec!["read".to_owned()]);
    let rebuilt = session.system_prompt();
    assert_ne!(
        rebuilt, old_prompt,
        "tool change must distinguish the rebuilt prompt from stale context"
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt(
            "input",
            PromptOptions {
                session_title_prompt: Some(SessionTitlePrompt::Disabled),
                ..Default::default()
            },
        ),
    )
    .await
    .expect("bounded faux request")
    .expect("prompt");
    session.dispose().await;
    assert_eq!(*lock(&received), vec![rebuilt]);
}

#[tokio::test]
async fn concurrent_reload_keeps_each_runtime_active_until_its_shutdown() {
    let session = test_session();
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let blocked = release.clone();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "<inline:reload-order>".into(),
        source_info: Default::default(),
        factory: Arc::new(move |api| {
            let runtime = api.runtime.clone();
            let started = started.clone();
            let blocked = blocked.clone();
            let count = count.clone();
            api.on(
                maho_ext_api::EventKind::SessionShutdown,
                Arc::new(move |_, _| {
                    let runtime = runtime.clone();
                    let started = started.clone();
                    let blocked = blocked.clone();
                    let count = count.clone();
                    Box::pin(async move {
                        if count.fetch_add(1, Ordering::SeqCst) == 0 {
                            started.send(()).expect("shutdown observer");
                            blocked.notified().await;
                        }
                        runtime.assert_active()?;
                        Ok(maho_ext_api::EventResult::None)
                    })
                }),
            );
            Box::pin(async { Ok(()) })
        }),
    };
    let runner = maho_ext_host::ExtensionRunner::from_async_factories(
        vec![factory],
        test_extension_context(&session),
        Default::default(),
    )
    .await
    .expect("factory");
    session.set_extension_runner(runner).await;
    let first_session = session.clone();
    let first = tokio::spawn(async move { first_session.reload().await });
    tokio::time::timeout(std::time::Duration::from_secs(5), starts.recv())
        .await
        .expect("shutdown starts")
        .expect("shutdown event");
    let second = session.reload();
    tokio::pin!(second);
    assert!(futures::poll!(&mut second).is_pending());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        assert!(
            !first
                .await
                .expect("first task")
                .expect("first reload")
                .cancelled
        );
        assert!(!second.await.expect("second reload").cancelled);
    })
    .await
    .expect("bounded reloads");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    session.dispose().await;
}

/// A `turn_start` handler that tells `started` when it begins and never settles.
fn held_turn_start_extension(
    session: &AgentSession,
    started: tokio::sync::mpsc::UnboundedSender<()>,
) -> maho_ext_api::LoadedExtension {
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:held>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::TurnStart,
        vec![Arc::new(move |_, _| {
            let started = started.clone();
            Box::pin(async move {
                let _ = started.send(());
                std::future::pending::<
                    Result<maho_ext_api::EventResult, maho_ext_api::ExtensionFailure>,
                >()
                .await
            })
        })],
    );
    extension
}

/// A session whose session.log lands in `agent_dir`.
fn test_session_in_agent_dir(agent_dir: &Path) -> AgentSession {
    let runtime = ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()),
        ..Default::default()
    })
    .expect("valid model runtime");
    AgentSession::new(AgentSessionConfig {
        agent: stub_agent(),
        session_manager: SessionManager::in_memory("/tmp", None, None),
        settings_manager: SettingsManager::from_storage(
            Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
            false,
        ),
        cwd: "/tmp".to_owned(),
        agent_dir: Some(agent_dir.to_string_lossy().into_owned()),
        fallback_now: None,
        retry_random: None,
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: BTreeMap::new(),
        custom_tools: Vec::new(),
        model_runtime: Some(runtime),
        model_registry: None,
        uses_default_stream_function: Some(false),
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: None,
    })
    .expect("session")
}

fn handler_budget_settings(session: &AgentSession, warn_ms: u64, timeout_ms: u64) {
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([
            ("extensionEventHandlerWarnMs".to_owned(), json!(warn_ms)),
            (
                "extensionEventHandlerTimeoutMs".to_owned(),
                json!(timeout_ms),
            ),
        ]))
    });
}

fn collect_extension_errors(session: &AgentSession) -> Arc<Mutex<Vec<ExtensionError>>> {
    let errors = Arc::new(Mutex::new(Vec::new()));
    let observed = errors.clone();
    session.state().extension_error_listener = Some(Arc::new(move |error: &ExtensionError| {
        lock(&observed).push(error.clone())
    }));
    errors
}

fn stalled_log_stages(agent_dir: &Path, handler_event: &str) -> Vec<String> {
    std::fs::read_to_string(agent_dir.join("logs").join("session.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| {
            line["event"] == "extension_handler_stalled" && line["handlerEvent"] == handler_event
        })
        .filter_map(|line| line["stage"].as_str().map(str::to_owned))
        .collect()
}

// senpi#3650: the session's turn deadline / abort path releases the current run's held handler waits.
#[tokio::test]
async fn a_held_lifecycle_handler_is_released_through_the_session() {
    let session = test_session();
    handler_budget_settings(&session, 0, 0);
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let extension = held_turn_start_extension(&session, started);
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    assert_eq!(session.release_held_extension_handlers().await, 0);
    let release = async {
        // The emission registers its held wait before the handler first runs.
        starts.recv().await.expect("held handler started");
        session.release_held_extension_handlers().await
    };
    let ((), released) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            session.dispatch_extension_event(maho_ext_api::ExtensionEvent::TurnStart {
                turn_index: 0,
                timestamp: 0,
            }),
            release
        )
    })
    .await
    .expect("the release ends the held emission");
    assert_eq!(released, 1);
}

// senpi#3650: the settings budget is bound to the runner: a handler past extensionEventHandlerTimeoutMs is
// stopped with an extension error, and the stall is logged to session.log.
#[tokio::test(start_paused = true)]
async fn the_settings_handler_timeout_stops_a_held_handler_and_logs_the_stall() {
    let agent_dir = tempfile::tempdir().expect("agent dir");
    let session = test_session_in_agent_dir(agent_dir.path());
    handler_budget_settings(&session, 0, 50);
    let errors = collect_extension_errors(&session);
    let (started, _starts) = tokio::sync::mpsc::unbounded_channel();
    let extension = held_turn_start_extension(&session, started);
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;

    // Without the bound budget the held handler is never stopped and this times out.
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        session.dispatch_extension_event(maho_ext_api::ExtensionEvent::TurnStart {
            turn_index: 0,
            timestamp: 0,
        }),
    )
    .await
    .expect("the handler budget stops the held handler");

    let errors = lock(&errors).clone();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].event, "turn_start");
    assert_eq!(errors[0].error, "handler timed out after 50ms");
    assert_eq!(
        stalled_log_stages(agent_dir.path(), "turn_start"),
        ["timeout"]
    );
}

// senpi#3650 "lets Esc release a held handler when the budget and the deadline are both disabled": the
// abort grace runs out on a run held by an extension handler, so the turn's held waits are released.
#[tokio::test(start_paused = true)]
async fn esc_releases_a_held_turn_start_handler_after_the_abort_grace() {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
    let agent_dir = tempfile::tempdir().expect("agent dir");
    let session = test_session_in_agent_dir(agent_dir.path());
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(vec![
        maho_ai::providers::faux::faux_assistant_message("unreached", Default::default()).into(),
    ]);
    let streams = faux_streams(provider.core.clone());
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    session
        .agent
        .set_stream_function(Arc::new(move |model, context, options| {
            streams.stream_simple(model, context, options.map(|options| options.simple))
        }));
    handler_budget_settings(&session, 0, 0);
    let errors = collect_extension_errors(&session);
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let extension = held_turn_start_extension(&session, started);
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;

    let esc = async {
        starts.recv().await.expect("held handler started");
        session.abort().await;
    };
    // Without the release after the abort grace, the held turn_start never settles and this times out.
    let (_prompted, ()) = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::join!(session.prompt("start", Default::default()), esc)
    })
    .await
    .expect("the abort grace releases the held handler and the run settles");

    assert!(!session.is_streaming());
    let errors = lock(&errors).clone();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].event, "turn_start");
    assert_eq!(errors[0].error, "handler released by the turn deadline");
    assert_eq!(
        stalled_log_stages(agent_dir.path(), "turn_start"),
        ["released"]
    );
    session.dispose().await;
}

// senpi#3227 / #3650: the session's own turn deadline, armed at prompt admission, stops a run held by an
// extension handler no abort reaches: it records the durable stop, captures it for the run's lifecycle,
// and releases the turn's held handler waits after the grace so the run settles.
#[tokio::test(start_paused = true)]
async fn the_turn_deadline_stops_a_held_run_and_releases_its_handlers() {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
    let agent_dir = tempfile::tempdir().expect("agent dir");
    let session = test_session_in_agent_dir(agent_dir.path());
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(vec![
        maho_ai::providers::faux::faux_assistant_message("unreached", Default::default()).into(),
    ]);
    let streams = faux_streams(provider.core.clone());
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    session
        .agent
        .set_stream_function(Arc::new(move |model, context, options| {
            streams.stream_simple(model, context, options.map(|options| options.simple))
        }));
    // No handler budget and no inactivity lease: only the max-runtime cap can end the held run.
    handler_budget_settings(&session, 0, 0);
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([
            ("turn_inactivity_ms".to_owned(), json!(0)),
            ("turn_max_runtime_ms".to_owned(), json!(1_000)),
        ]))
    });
    let errors = collect_extension_errors(&session);
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let extension = held_turn_start_extension(&session, started);
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    assert_eq!(session.current_deadline(), None);

    let held = async {
        starts.recv().await.expect("held handler started");
        // The run is live and held; nothing but the deadline acts on it from here.
        assert!(session.agent_run_active.load(Ordering::SeqCst));
        lock(&session.lifecycle).run_id.clone().expect("run id")
    };
    // Without the armed deadline the held turn_start never settles and this times out.
    let (_prompted, run_id) = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::join!(session.prompt("start", Default::default()), held)
    })
    .await
    .expect("the turn deadline ends the held run");

    assert!(!session.is_streaming());
    let deadline = session
        .current_deadline()
        .expect("the deadline that stopped the turn");
    assert_eq!(
        deadline.kind,
        crate::turn_deadline::TurnDeadlineKind::MaxRuntime
    );
    assert_eq!(deadline.limit_ms, 1_000);
    // The held handler is turn_start, which runs before the prompt's own message is appended, so the
    // durable entry is found on the branch rather than as a stop pending after the last user message.
    let recorded: Vec<Value> = session.with_session_manager(|manager| {
        manager
            .branch(None)
            .into_iter()
            .filter(|entry| entry["customType"] == crate::turn_deadline::TURN_DEADLINE_ENTRY_TYPE)
            .collect()
    });
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["data"]["kind"], "max_runtime");
    assert_eq!(recorded[0]["data"]["limitMs"], 1_000);
    let captured = lock(&session.lifecycle_deadline_stop)
        .clone()
        .expect("the lifecycle captured the stop");
    assert_eq!(captured.run_id, run_id);
    assert_eq!(
        captured.kind,
        crate::session_lifecycle_types::LifecycleDeadlineKind::MaxRuntime
    );
    assert_eq!(captured.persisted, None);
    let errors = lock(&errors).clone();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].event, "turn_start");
    assert_eq!(errors[0].error, "handler released by the turn deadline");
    session.dispose().await;
}

#[tokio::test]
async fn unbound_reload_keeps_flag_values_and_skips_session_start() {
    let session = test_session();
    let starts = Arc::new(Mutex::new(0_usize));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:reload-flags>",
        session.cwd().into(),
        Default::default(),
    );
    let counted = starts.clone();
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |_, _| {
            *lock(&counted) += 1;
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    let retained = extension.clone();
    let runtime = maho_ext_api::ExtensionRuntime::default();
    runtime.set_flag("no-ask-user", FlagValue::Boolean(true));
    let mut runner = maho_ext_host::ExtensionRunner::new(
        vec![extension],
        runtime,
        Default::default(),
        test_extension_context(&session),
    );
    runner.set_runtime_factory(Arc::new(move |context| {
        let extension = retained.clone();
        Box::pin(async move {
            Ok(maho_ext_host::ExtensionRunner::new(
                vec![extension],
                Default::default(),
                Default::default(),
                context,
            ))
        })
    }));
    session.set_extension_runner(runner).await;
    assert!(!session.reload().await.expect("reload").cancelled);
    let flag = session
        .extension_runner
        .lock()
        .await
        .as_ref()
        .and_then(|runner| runner.runtime.get_flag("no-ask-user"));
    assert_eq!(flag, Some(FlagValue::Boolean(true)));
    assert_eq!(*lock(&starts), 0);
    session.dispose().await;
}

fn register_window(runtime: &mut ModelRuntime, window: f64) {
    runtime
        .register_provider(
            "fixture",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api: Some("openai-completions".into()),
                    base_url: Some("http://127.0.0.1:9".into()),
                    models: Some(vec![crate::model_config_schema::ModelsJsonModel {
                        id: "live".into(),
                        context_window: Some(window),
                        max_tokens: Some(100.0),
                        ..Default::default()
                    }]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("register catalog model");
}

#[tokio::test]
async fn reload_replaces_the_live_model_with_the_refreshed_catalog_window() {
    let session = test_session();
    let mut runtime = session.model_runtime().clone();
    register_window(&mut runtime, 1_000.0);
    let model = session
        .model_runtime()
        .get_model("fixture", "live")
        .expect("catalog model");
    session.agent.set_model(model);
    register_window(&mut runtime, 8_000.0);
    assert_eq!(
        session.model().context_window,
        1_000,
        "a catalog rewrite must not mutate the session's copied model"
    );
    session.refresh_current_model_from_registry();
    assert_eq!(session.model().context_window, 8_000);

    register_window(&mut runtime, 16_000.0);
    assert!(!session.reload().await.expect("reload").cancelled);
    assert_eq!(session.model().context_window, 16_000);

    let runner = ExtensionRunner::new(
        Vec::new(),
        Default::default(),
        Default::default(),
        test_extension_context(&session),
    );
    session.set_extension_runner(runner).await;
    let extension_runtime = session
        .extension_runner
        .lock()
        .await
        .as_ref()
        .expect("runner")
        .runtime
        .clone();
    extension_runtime
        .register_provider(
            maho_ext_api::ProviderRegistration::Config {
                name: "fixture".into(),
                config: Box::new(maho_ext_api::ProviderConfig {
                    api: Some("openai-completions".into()),
                    models: Some(vec![maho_ext_api::ProviderModelConfig {
                        model_type: None,
                        output: None,
                        image_cost: None,
                        input_limits: None,
                        prompt_cache: None,
                        supports_assistant_prefill: None,
                        default_thinking_level: None,
                        id: "live".into(),
                        name: "live".into(),
                        upstream_model_id: None,
                        api: None,
                        base_url: Some("http://127.0.0.1:9".into()),
                        reasoning: false,
                        recover_text_tool_calls: None,
                        thinking_level_map: None,
                        input: vec![maho_ai::types::InputModality::Text],
                        cost: Default::default(),
                        context_window: 32_000,
                        max_tokens: 100,
                        headers: None,
                        extra_body: None,
                        compat: None,
                    }]),
                    ..Default::default()
                }),
            },
            "fixture",
        )
        .expect("reregister");
    assert_eq!(session.model().context_window, 32_000);
    session.dispose().await;
}

#[tokio::test]
async fn dispatched_context_reflects_bound_ui_and_mode_and_survives_runner_recreation() {
    let session = test_session();
    let seen = Arc::new(Mutex::new(Vec::<(ExtensionMode, bool)>::new()));
    let observed = seen.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:binding-contract>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |_, context| {
            lock(&observed).push((context.mode, context.has_ui));
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    let retained = extension.clone();
    let mut runner = maho_ext_host::ExtensionRunner::new(
        vec![extension],
        Default::default(),
        Default::default(),
        test_extension_context(&session),
    );
    runner.set_runtime_factory(Arc::new(move |context| {
        let extension = retained.clone();
        Box::pin(async move {
            Ok(maho_ext_host::ExtensionRunner::new(
                vec![extension],
                Default::default(),
                Default::default(),
                context,
            ))
        })
    }));
    session.set_extension_runner(runner).await;
    session
        .bind_extensions(ExtensionBindings {
            mode: Some(ExtensionMode::Print),
            ..Default::default()
        })
        .await;
    session
        .bind_extensions(ExtensionBindings {
            mode: Some(ExtensionMode::Json),
            ..Default::default()
        })
        .await;
    session
        .bind_extensions(ExtensionBindings {
            ui_context: Some(Arc::new(TestExtensionUi)),
            mode: Some(ExtensionMode::Rpc),
            ..Default::default()
        })
        .await;
    session
        .bind_extensions(ExtensionBindings {
            ui_context: Some(Arc::new(TestExtensionUi)),
            mode: Some(ExtensionMode::Tui),
            ..Default::default()
        })
        .await;
    session
        .reload()
        .await
        .expect("reload re-applies the bound mode and UI");
    assert_eq!(
        *lock(&seen),
        vec![
            (ExtensionMode::Print, false),
            (ExtensionMode::Json, false),
            (ExtensionMode::Rpc, true),
            (ExtensionMode::Tui, true),
            (ExtensionMode::Tui, true),
        ]
    );
}

#[tokio::test]
async fn binding_waits_for_startup_user_message_admission() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "welcome",
            Default::default(),
        )],
        0,
    );
    let captured = session.clone();
    let admitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = admitted.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:startup-admission>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(
                &SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("start".into()),
                Default::default(),
            )
            .expect("startup message");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::InputDisposition,
        vec![Arc::new(move |event, _| {
            if matches!(
                event,
                maho_ext_api::ExtensionEvent::InputDisposition {
                    disposition: maho_ext_api::InputDisposition::Started,
                    ..
                }
            ) {
                observed.store(true, Ordering::SeqCst);
            }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.bind_extensions(Default::default()),
    )
    .await
    .expect("bounded binding");
    let ready_at_return = admitted.load(Ordering::SeqCst);
    tokio::time::timeout(std::time::Duration::from_secs(5), session.wait_for_idle())
        .await
        .expect("cleanup turn");
    assert!(
        ready_at_return,
        "binding returned before startup user-message admission"
    );
}

#[tokio::test]
async fn startup_user_binding_does_not_wait_for_provider_completion() {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
    let (started, entered) = tokio::sync::oneshot::channel();
    let started = Arc::new(Mutex::new(Some(started)));
    let (release, released) = tokio::sync::watch::channel(false);
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        scheduler_hook: Some(Arc::new(move || {
            let started = started.clone();
            let mut released = released.clone();
            Box::pin(async move {
                if let Some(started) = lock(&started).take() {
                    started.send(()).expect("provider observer");
                }
                released
                    .wait_for(|value| *value)
                    .await
                    .expect("release provider");
            })
        })),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(vec![
        maho_ai::providers::faux::faux_assistant_message("welcome", Default::default()).into(),
    ]);
    let streams = faux_streams(provider.core.clone());
    let session = test_session_with_stream_function(false);
    // senpi test/suite/harness.ts: the faux provider is registered, so `prompt()`'s auth gate admits it.
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    session
        .agent
        .set_stream_function(Arc::new(move |model, context, options| {
            streams.stream_simple(model, context, options.map(|options| options.simple))
        }));
    let captured = session.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:startup-held>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(
                &SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("start".into()),
                Default::default(),
            )
            .expect("startup message");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(session.bind_extensions(Default::default()), async {
            entered.await.expect("provider entered");
        });
        assert!(
            session.get_last_assistant_text().is_none(),
            "provider is still held"
        );
    })
    .await;
    release.send_replace(true);
    tokio::time::timeout(std::time::Duration::from_secs(5), session.wait_for_idle())
        .await
        .expect("cleanup provider");
    result.expect("binding must finish without provider completion");
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some("welcome")
    );
}

#[tokio::test]
async fn bound_shutdown_handler_replaces_session_dispose() {
    let session = test_session();
    let requested = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = requested.clone();
    session
        .bind_extensions(ExtensionBindings {
            shutdown_handler: Some(Arc::new(move || {
                observed.fetch_add(1, Ordering::SeqCst);
            })),
            ..Default::default()
        })
        .await;
    maho_ext_api::ExtensionContextActions::shutdown(&*session.extension_context_actions());
    assert_eq!(requested.load(Ordering::SeqCst), 1);
    assert!(!session.state().disposed);
}

#[tokio::test]
async fn extension_rpc_events_reach_session_listeners() {
    let session = test_session();
    let events = maho_ext_api::EventBus::default();
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            Vec::new(),
            Default::default(),
            events.clone(),
            test_extension_context(&session),
        ))
        .await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observed = seen.clone();
    session.add_extension_rpc_event_listener(Arc::new(move |name, data| {
        lock(&observed).push((name.to_owned(), data.clone()))
    }));
    events.emit(
        crate::event_bus::EXTENSION_RPC_EVENT_CHANNEL,
        &serde_json::json!({"name":"memory_updated","data":{"n":1}}),
    );
    assert_eq!(
        *lock(&seen),
        [("memory_updated".to_owned(), serde_json::json!({"n":1}))]
    );
}

#[tokio::test]
async fn bound_error_listener_receives_runner_errors_once() {
    let session = test_session();
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            Vec::new(),
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let errors = Arc::new(Mutex::new(Vec::new()));
    let observed = errors.clone();
    session
        .bind_extensions(ExtensionBindings {
            on_error: Some(Arc::new(move |error| lock(&observed).push(error.clone()))),
            ..Default::default()
        })
        .await;
    let error = ExtensionError {
        extension_path: "<inline:x>".to_owned(),
        event: "tool_call".to_owned(),
        error: "boom".to_owned(),
        stack: None,
    };
    session
        .extension_runner
        .lock()
        .await
        .as_mut()
        .expect("bound runner")
        .emit_error(error.clone());
    assert_eq!(*lock(&errors), [error]);
}

#[tokio::test]
async fn attach_rebind_installs_context_without_repeating_startup() {
    let session = test_session();
    let dispatches = Arc::new(Mutex::new(Vec::<(
        ExtensionMode,
        bool,
        maho_ext_api::SessionReason,
    )>::new()));
    let observed = dispatches.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:attach-rebind>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |event, context| {
            if let maho_ext_api::ExtensionEvent::SessionStart(event) = event {
                lock(&observed).push((context.mode, context.has_ui, event.reason));
            }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    let retained = extension.clone();
    let mut runner = maho_ext_host::ExtensionRunner::new(
        vec![extension],
        Default::default(),
        Default::default(),
        test_extension_context(&session),
    );
    runner.set_runtime_factory(Arc::new(move |context| {
        let extension = retained.clone();
        Box::pin(async move {
            Ok(maho_ext_host::ExtensionRunner::new(
                vec![extension],
                Default::default(),
                Default::default(),
                context,
            ))
        })
    }));
    session.set_extension_runner(runner).await;

    assert!(!session.has_started_extension_lifecycle());
    session
        .bind_extensions(ExtensionBindings {
            mode: Some(ExtensionMode::Rpc),
            ..Default::default()
        })
        .await;
    assert_eq!(
        *lock(&dispatches),
        vec![(
            ExtensionMode::Rpc,
            false,
            maho_ext_api::SessionReason::Startup
        )]
    );
    assert!(session.has_started_extension_lifecycle());

    session
        .rebind_extension_context(ExtensionBindings {
            ui_context: Some(Arc::new(TestExtensionUi)),
            mode: Some(ExtensionMode::Rpc),
            ..Default::default()
        })
        .await;
    assert_eq!(lock(&dispatches).len(), 1);
    assert_eq!(session.state().extension_mode, ExtensionMode::Rpc);
    assert!(session.state().extension_ui_context.is_some());

    session
        .reload()
        .await
        .expect("reload re-applies the bound mode and UI");
    assert_eq!(
        *lock(&dispatches),
        vec![
            (
                ExtensionMode::Rpc,
                false,
                maho_ext_api::SessionReason::Startup
            ),
            (
                ExtensionMode::Rpc,
                true,
                maho_ext_api::SessionReason::Reload
            ),
        ]
    );
}

#[tokio::test]
async fn startup_binding_releases_readiness_for_handled_input() {
    let session = test_session();
    let captured = session.clone();
    let handled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = handled.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:startup-handled>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(
                &SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("handled".into()),
                Default::default(),
            )
            .expect("startup message");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::Input,
        vec![Arc::new(move |_, _| {
            observed.store(true, Ordering::SeqCst);
            Box::pin(async {
                Ok(maho_ext_api::EventResult::Input(
                    maho_ext_api::InputEventResult::Handled,
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.bind_extensions(Default::default()),
    )
    .await
    .expect("handled input releases binding");
    assert!(handled.load(Ordering::SeqCst));
    assert!(lock(&session.binding_readiness).is_none());
    assert!(!session.work_barrier.has_active_work());
    assert!(session.messages().is_empty());
}

#[tokio::test]
async fn startup_binding_accepts_queued_input_during_compaction() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let signal = actions
        .begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual,
        })
        .expect("active compaction");
    let captured = session.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:startup-queued>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(
                &SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("queued startup".into()),
                maho_ext_api::SendUserMessageOptions {
                    deliver_as: Some(maho_ext_api::StreamingBehavior::FollowUp),
                    ..Default::default()
                },
            )
            .expect("startup message");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.bind_extensions(Default::default()),
    )
    .await;
    let queued = session.get_follow_up_messages();
    let compacting_at_return = session.is_compacting();
    session.clear_queue(false);
    actions.end_compaction(maho_ext_api::EndCompactionOptions {
        reason: maho_ext_api::CompactionReason::Manual,
        signal: Some(signal),
        aborted: Some(false),
        error_message: None,
    });
    result.expect("queued input releases binding without waiting for compaction");
    assert!(compacting_at_return);
    assert_eq!(queued, ["queued startup"]);
    assert!(lock(&session.binding_readiness).is_none());
}

#[tokio::test]
async fn rejected_startup_input_releases_binding_readiness() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let signal = actions
        .begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual,
        })
        .expect("active compaction");
    let errors = Arc::new(Mutex::new(Vec::new()));
    let observed = errors.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::ContinuationError { error_message } = event {
            lock(&observed).push(error_message.clone());
        }
    }));
    let captured = session.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:startup-rejected>",
        session.cwd().into(),
        Default::default(),
    );
    let input_calls = Arc::new(AtomicU64::new(0));
    let calls = input_calls.clone();
    extension.handlers.insert(
        maho_ext_api::EventKind::Input,
        vec![Arc::new(move |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(maho_ext_api::EventResult::Input(
                    maho_ext_api::InputEventResult::Transform {
                        text: "must not transform retained input".into(),
                        images: None,
                    },
                ))
            })
        })],
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(
                &SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("no queue mode".into()),
                Default::default(),
            )
            .expect("dispatch startup");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.bind_extensions(Default::default()),
    )
    .await;
    let retained = session.get_follow_up_messages();
    session.clear_queue(false);
    actions.end_compaction(maho_ext_api::EndCompactionOptions {
        reason: maho_ext_api::CompactionReason::Manual,
        signal: Some(signal),
        aborted: Some(false),
        error_message: None,
    });
    result.expect("rejected input releases binding");
    assert_eq!(retained, ["no queue mode"]);
    assert_eq!(input_calls.load(Ordering::SeqCst), 0);
    assert_eq!(lock(&errors).len(), 1);
    assert!(lock(&session.binding_readiness).is_none());
    assert!(!session.work_barrier.has_active_work());
}

#[tokio::test]
async fn rejected_startup_images_survive_one_later_provider_turn() {
    use maho_ext_api::ExtensionContextActions;
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "delivered",
            Default::default(),
        )],
        0,
    );
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let signal = actions
        .begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual,
        })
        .expect("compaction");
    let captured = session.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:retained-images>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(
                &SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Blocks(vec![
                    maho_ext_api::ToolContent::text("first"),
                    maho_ext_api::ToolContent::Image {
                        data: "aW1hZ2Ux".into(),
                        mime_type: "image/png".into(),
                    },
                    maho_ext_api::ToolContent::text("second"),
                    maho_ext_api::ToolContent::Image {
                        data: "aW1hZ2Uy".into(),
                        mime_type: "image/jpeg".into(),
                    },
                ]),
                Default::default(),
            )
            .expect("startup message");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.bind_extensions(Default::default()),
    )
    .await
    .expect("binding");
    assert_eq!(session.get_follow_up_messages(), ["first\nsecond"]);
    actions.end_compaction(maho_ext_api::EndCompactionOptions {
        reason: maho_ext_api::CompactionReason::Manual,
        signal: Some(signal),
        aborted: Some(false),
        error_message: None,
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.continue_session(),
    )
    .await
    .expect("bounded delivery")
    .expect("delivery");
    let users: Vec<_> = session
        .messages()
        .into_iter()
        .filter(|message| message.role() == "user")
        .collect();
    assert_eq!(users.len(), 1);
    let value = serde_json::to_value(&users[0]).expect("user payload");
    assert_eq!(value["content"][0]["text"], "first\nsecond");
    assert_eq!(value["content"][1]["data"], "aW1hZ2Ux");
    assert_eq!(value["content"][2]["data"], "aW1hZ2Uy");
    assert_eq!(value["content"][1]["mimeType"], "image/png");
    assert_eq!(value["content"][2]["mimeType"], "image/jpeg");
    assert!(!session.agent.has_queued_messages());
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some("delivered")
    );
}

#[tokio::test]
async fn pre_admission_settings_error_preserves_requested_steering() {
    let session = test_session();
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"enabled":true,"reserveTokens":"invalid"}),
        )]))
    });
    let captured = session.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:admission-rejected>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(
                &SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("retained steering".into()),
                maho_ext_api::SendUserMessageOptions {
                    deliver_as: Some(maho_ext_api::StreamingBehavior::Steer),
                    ..Default::default()
                },
            )
            .expect("dispatch");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.bind_extensions(Default::default()),
    )
    .await
    .expect("bounded admission");
    let retained = session.get_steering_messages();
    let follow_up = session.get_follow_up_messages();
    session.clear_queue(false);
    assert_eq!(retained, ["retained steering"]);
    assert!(follow_up.is_empty());
    assert!(session.messages().is_empty());
    assert!(!session.work_barrier.has_active_work());
}

#[tokio::test]
async fn started_extension_failure_does_not_requeue_user_input() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "accepted",
            Default::default(),
        )],
        0,
    );
    let errors = Arc::new(Mutex::new(Vec::new()));
    let captured_errors = errors.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::ContinuationError { error_message } = event {
            lock(&captured_errors).push(error_message.clone());
        }
    }));
    let post_admission = session.clone();
    let captured = session.clone();
    let started = Arc::new(AtomicU64::new(0));
    let observed = started.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:started-error>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(
                &SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("once".into()),
                Default::default(),
            )
            .expect("dispatch");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::InputDisposition,
        vec![Arc::new(move |event, _| {
            if matches!(
                event,
                maho_ext_api::ExtensionEvent::InputDisposition {
                    disposition: maho_ext_api::InputDisposition::Started,
                    ..
                }
            ) {
                observed.fetch_add(1, Ordering::SeqCst);
                post_admission.with_settings_manager_mut(|manager| {
                    manager.apply_overrides(&Map::from_iter([(
                        "compaction".to_owned(),
                        serde_json::json!({"enabled":true,"reserveTokens":"invalid"}),
                    )]))
                });
            }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        session.bind_extensions(Default::default()).await;
        session.wait_for_idle().await;
    })
    .await
    .expect("bounded failed turn");
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(
        session
            .messages()
            .iter()
            .filter(|message| message.role() == "user")
            .count(),
        1
    );
    assert_eq!(session.pending_message_count(), 0);
    assert!(!session.agent.has_queued_messages());
    assert_eq!(
        lock(&errors).len(),
        1,
        "completion must reach the adapter error branch"
    );
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some("accepted")
    );
}

#[tokio::test]
async fn extension_prompt_waits_for_pending_manual_compaction() {
    use std::{future::Future, task::Poll};
    let session = test_session();
    let admission = session.prompt_admission.lock().await;
    let mut compact = Box::pin(session.compact(None));
    std::future::poll_fn(|cx| {
        assert!(
            compact.as_mut().poll(cx).is_pending(),
            "compaction waits for admission"
        );
        Poll::Ready(())
    })
    .await;
    assert!(session.state().pending_compaction_admission.is_some());
    let mut prompt = Box::pin(session.prompt(
        "extension input",
        PromptOptions {
            source: Some(InputSource::Extension),
            ..Default::default()
        },
    ));
    let pending =
        std::future::poll_fn(|cx| Poll::Ready(prompt.as_mut().poll(cx).is_pending())).await;
    drop(prompt);
    drop(compact);
    drop(admission);
    assert!(!session.work_barrier.has_active_work());
    assert!(session.state().pending_compaction_admission.is_none());
    assert!(
        pending,
        "extension prompt must wait rather than reject during manual compaction"
    );
}

#[tokio::test]
async fn extension_prompt_stays_queued_after_pending_compaction_is_cancelled() {
    use std::{future::Future, task::Poll};
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "after compaction",
            Default::default(),
        )],
        0,
    );
    let admission = session.prompt_admission.lock().await;
    let mut compact = Box::pin(session.compact(None));
    std::future::poll_fn(|cx| {
        assert!(compact.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let mut prompt = Box::pin(session.prompt(
        "waiting extension",
        PromptOptions {
            source: Some(InputSource::Extension),
            ..Default::default()
        },
    ));
    std::future::poll_fn(|cx| {
        assert!(prompt.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    session.abort_compaction();
    drop(admission);
    let (compacted, prompted) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(compact, prompt)
    })
    .await
    .expect("released admission");
    assert_eq!(
        compacted.expect_err("cancelled compaction"),
        "Compaction cancelled"
    );
    assert_eq!(
        prompted.expect("extension retained"),
        PromptDisposition::Queued
    );
    assert!(session.get_last_assistant_text().is_none());
    assert!(session.messages().is_empty());
    assert_eq!(session.get_steering_messages(), ["waiting extension"]);
    assert!(session.agent.has_queued_messages());
    session.clear_queue(false);
    assert!(!session.work_barrier.has_active_work());
    assert!(!session.is_compacting());
}

#[tokio::test]
async fn extension_prompt_stays_queued_after_pending_compaction_fails() {
    use std::{future::Future, task::Poll};
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "must not run",
            Default::default(),
        )],
        0,
    );
    let admission = session.prompt_admission.lock().await;
    let mut compact = Box::pin(session.compact(None));
    std::future::poll_fn(|cx| {
        assert!(compact.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let mut prompt = Box::pin(session.prompt(
        "after failed compaction",
        PromptOptions {
            source: Some(InputSource::Extension),
            ..Default::default()
        },
    ));
    std::future::poll_fn(|cx| {
        assert!(prompt.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(admission);
    let (compacted, prompted) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(compact, prompt)
    })
    .await
    .expect("released admission");
    assert!(compacted.is_err());
    let queued = session.get_steering_messages();
    session.clear_queue(false);
    assert_eq!(
        prompted.expect("retained prompt"),
        PromptDisposition::Queued
    );
    assert_eq!(queued, ["after failed compaction"]);
    assert!(session.messages().is_empty());
    assert!(!session.work_barrier.has_active_work());
}

#[tokio::test]
async fn extension_prompt_runs_after_pending_compaction_succeeds() {
    use std::{future::Future, task::Poll};
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "after digest",
            Default::default(),
        )],
        0,
    );
    session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
        ("compaction".into(), serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false,"keepRecentTokens":1})),
    ])));
    for text in ["old task".repeat(200), "recent task".repeat(200)] {
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0}))
        });
    }
    session.rebuild_session_context().expect("history");
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:accepted-compaction>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("preparation");
            };
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(maho_ext_api::CompactionResult {
                            summary: "digest".into(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                            tokens_before: event.preparation.tokens_before,
                            details: None,
                        }),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let admission = session.prompt_admission.lock().await;
    let mut compact = Box::pin(session.compact(None));
    std::future::poll_fn(|cx| {
        assert!(compact.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let mut prompt = Box::pin(session.prompt(
        "after successful compaction",
        PromptOptions {
            source: Some(InputSource::Extension),
            ..Default::default()
        },
    ));
    std::future::poll_fn(|cx| {
        assert!(prompt.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(admission);
    let (compacted, prompted) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(compact, prompt)
    })
    .await
    .expect("successful release");
    assert_eq!(compacted.expect("compaction").summary, "digest");
    assert_eq!(prompted.expect("provider turn"), PromptDisposition::Started);
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some("after digest")
    );
    assert!(
        session
            .with_session_manager(|manager| manager.entries())
            .iter()
            .any(|entry| entry["type"] == "compaction")
    );
    assert_eq!(session.pending_message_count(), 0);
    assert!(!session.agent.has_queued_messages());
    assert!(!session.work_barrier.has_active_work());
}

#[tokio::test]
async fn extension_prompt_cancels_without_releasing_pending_compaction() {
    use std::{future::Future, task::Poll};
    let session = test_session();
    let admission = session.prompt_admission.lock().await;
    let mut compact = Box::pin(session.compact(None));
    std::future::poll_fn(|cx| {
        assert!(compact.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let controller = maho_ai::utils::abort::AbortController::new();
    let mut prompt = Box::pin(session.prompt(
        "cancelled extension",
        PromptOptions {
            source: Some(InputSource::Extension),
            signal: Some(controller.signal()),
            ..Default::default()
        },
    ));
    std::future::poll_fn(|cx| {
        assert!(prompt.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    controller.abort(None);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), &mut prompt)
        .await
        .expect("cancelled admission");
    assert_eq!(result, Err("Prompt cancelled".into()));
    assert!(session.state().pending_compaction_admission.is_some());
    assert!(session.messages().is_empty());
    drop(prompt);
    drop(compact);
    drop(admission);
    assert!(!session.work_barrier.has_active_work());
    assert!(session.state().pending_compaction_admission.is_none());
}

#[tokio::test]
async fn cancelled_binding_clears_its_readiness_scope() {
    let session = test_session();
    let (entered, observed) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:cancel-binding>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |_, _| {
            let entered = entered.clone();
            Box::pin(async move {
                lock(&entered)
                    .take()
                    .expect("single startup")
                    .send(())
                    .expect("observer");
                std::future::pending::<()>().await;
                Ok(maho_ext_api::EventResult::None)
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let mut binding = Box::pin(session.bind_extensions(Default::default()));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            () = &mut binding => panic!("startup handler must stay pending"),
            result = observed => result.expect("startup entered"),
        }
    })
    .await
    .expect("bounded startup signal");
    assert!(lock(&session.binding_readiness).is_some());
    drop(binding);
    assert!(lock(&session.binding_readiness).is_none());
    assert!(!session.work_barrier.has_active_work());
}

#[tokio::test]
async fn agent_end_publishes_the_session_envelope_with_will_retry() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "done",
            Default::default(),
        )],
        0,
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription =
        session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
    session
        .prompt("hello", PromptOptions::default())
        .await
        .expect("prompt");
    let events = lock(&events);
    let (messages, will_retry, aborted) = events
        .iter()
        .find_map(|event| match event {
            AgentSessionEvent::AgentEnd {
                messages,
                will_retry,
                aborted,
                ..
            } => Some((messages, *will_retry, *aborted)),
            _ => None,
        })
        .expect("session agent_end envelope");
    assert_eq!((will_retry, aborted), (false, false));
    assert!(
        messages
            .iter()
            .any(|message| message.as_assistant().is_some())
    );
    assert!(events.iter().all(|event| !matches!(
        event,
        AgentSessionEvent::Agent(maho_agent::types::AgentEvent::AgentEnd { .. })
    )));
}

/// senpi test/suite/regressions/3228-agent-end-provider-failure.test.ts and 4328-provider-failure-route:
/// the agent_end projection names the failing route and, when a fallback ran it, the selected model.
#[tokio::test]
async fn agent_end_failure_names_the_selected_model_of_an_active_fallback() {
    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, FauxModelDefinition, RegisterFauxProviderOptions,
        faux_assistant_message, faux_provider, faux_streams,
    };
    let failed = || {
        faux_assistant_message(
            "",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("provider failed".into()),
                ..Default::default()
            },
        )
    };
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        models: Some(
            ["faux-1", "faux-2"]
                .into_iter()
                .map(|id| FauxModelDefinition {
                    id: id.to_owned(),
                    ..Default::default()
                })
                .collect(),
        ),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(vec![failed().into(), failed().into()]);
    let streams = faux_streams(provider.core.clone());
    let session = test_session_with_stream_function(false);
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    session
        .agent
        .set_stream_function(Arc::new(move |model, context, options| {
            streams.stream_simple(model, context, options.map(|options| options.simple))
        }));
    session.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global, &serde_json::from_value(
        serde_json::json!({"retry":{"enabled":false,"maxRetries":0,"modelFallback":false,"fallbackRevertPolicy":"never"}})
    ).expect("settings")).expect("save settings"));
    let prompt = |text: &'static str| {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            session.prompt(text, PromptOptions::default()),
        )
    };
    // The request ran on the selected model: no selection is reported beside it.
    prompt("first")
        .await
        .expect("bounded prompt")
        .expect("prompt");
    let failure = session.last_provider_failure().expect("failure");
    assert_eq!(
        (failure.provider.as_str(), failure.model.as_deref()),
        ("faux", Some("faux-1"))
    );
    assert_eq!(
        (failure.selected_provider, failure.selected_model),
        (None, None)
    );
    // A fallback rung ran the request: the chain original is the selection.
    session
        .retry_fallback
        .lock()
        .await
        .as_mut()
        .expect("controller")
        .state = Some(crate::retry_fallback::controller::ActiveFallbackState {
        chain_key: "fixture".to_owned(),
        original_selector: "faux/faux-2".to_owned(),
        original_thinking_level: None,
        last_applied_thinking_level: None,
        pinned_by_refusal: false,
        pinned_by_billing: false,
        pinned: false,
    });
    prompt("second")
        .await
        .expect("bounded prompt")
        .expect("prompt");
    let failure = session.last_provider_failure().expect("failure");
    assert_eq!(failure.model.as_deref(), Some("faux-1"));
    assert_eq!(
        (
            failure.selected_provider.as_deref(),
            failure.selected_model.as_deref()
        ),
        (Some("faux"), Some("faux-2"))
    );
    assert!(
        crate::provider_failure::format_provider_failure(&failure)
            .starts_with("faux-1 on faux (selected: faux-2 on faux) failed: ")
    );
}

#[tokio::test]
async fn begin_compaction_bridges_the_controller_abort_to_the_extension_signal() {
    let session = retry_session(Vec::new(), 0);
    let signal = session
        .extension_context_actions()
        .begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual,
        })
        .expect("feedback signal");
    let controller = session
        .state()
        .compaction_abort_controller
        .clone()
        .expect("controller");
    controller.abort();
    tokio::time::timeout(std::time::Duration::from_secs(2), signal.cancelled())
        .await
        .expect("controller abort propagates to extension signal");
    assert!(signal.is_aborted());
}

#[tokio::test]
async fn an_extension_abort_reaches_the_compaction_controller() {
    let session = retry_session(Vec::new(), 0);
    let signal = session
        .extension_context_actions()
        .begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual,
        })
        .expect("feedback signal");
    let controller = session
        .state()
        .compaction_abort_controller
        .clone()
        .expect("controller");
    signal.abort();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        controller.signal().cancelled(),
    )
    .await
    .expect("extension abort propagates to controller");
    assert!(controller.aborted());
}

#[tokio::test]
async fn binding_enforces_builtin_defaults_without_removing_user_tools() {
    let session = test_session();
    session.state().default_tool_names = Some(BTreeSet::from(["read".to_owned()]));
    for (name, builtin) in [("read", true), ("bash", true), ("user-tool", false)] {
        let mut source = empty_source_info();
        source.source = if builtin { "builtin" } else { "inline" }.to_owned();
        source.path = if builtin {
            format!("<builtin:{name}>")
        } else {
            format!("<inline:{name}>")
        };
        session.register_tool_definition(test_definition(name), source, test_tool(name));
    }
    session.set_active_tools_by_name(vec![
        "read".to_owned(),
        "bash".to_owned(),
        "user-tool".to_owned(),
    ]);
    session.bind_extensions(Default::default()).await;
    assert_eq!(session.get_active_tool_names(), ["read", "user-tool"]);
    assert!(session.get_tool_definition("bash").is_some());
}

#[test]
fn all_tools_includes_inactive_definitions_with_source_metadata() {
    let session = test_session();
    let mut definition = test_definition("inactive");
    definition.label = "Inactive label".to_owned();
    definition.exposure = Some(ToolExposure::Search);
    definition.prompt_guidelines = Some(vec!["use with context".to_owned()]);
    let source = SourceInfo {
        path: "inline-extension".to_owned(),
        source: "inline".to_owned(),
        scope: SourceScope::Project,
        origin: SourceOrigin::Package,
        base_dir: Some("/tmp".to_owned()),
    };
    session.register_tool_definition(definition, source.clone(), test_tool("inactive"));
    assert!(
        !session
            .get_active_tool_names()
            .contains(&"inactive".to_owned())
    );
    let tools = session.get_all_tools();
    let info = tools.iter().find(|tool| tool.name == "inactive").unwrap();
    assert_eq!(info.label, "Inactive label");
    assert_eq!(info.source_info, source);
    assert_eq!(info.exposure, ToolExposure::Search);
    assert_eq!(
        info.prompt_guidelines,
        Some(vec!["use with context".to_owned()])
    );
    let exposed = maho_ext_api::ExtensionActions::get_all_tools(&SessionExtensionActions(
        Arc::downgrade(&session.inner),
    ))
    .unwrap();
    assert_eq!(
        exposed
            .iter()
            .find(|tool| tool.name == "inactive")
            .unwrap()
            .label,
        "Inactive label"
    );
}

#[test]
fn all_tools_includes_inactive_definitions_and_preserves_labels() {
    use maho_ext_api::ExtensionActions;
    let session = test_session();
    let mut definition = test_definition("read");
    definition.label = "Read files".into();
    definition.search_keywords = Some(vec!["files".into()]);
    session.register_tool_definition(definition, empty_source_info(), test_tool("read"));
    assert!(session.get_active_tool_names().is_empty());
    let tools = SessionExtensionActions(Arc::downgrade(&session.inner))
        .get_all_tools()
        .unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].label, "Read files");
    assert_eq!(tools[0].search_keywords, ["files"]);
}

#[test]
fn late_search_tool_registration_does_not_expose_it_as_direct() {
    use maho_ext_api::ExtensionSessionActions;
    let session = test_session();
    session.state().extension_tool_context = Some((
        maho_ext_api::ExtensionRuntime::default(),
        Arc::new(|| Err("unused context".into())),
    ));
    let mut definition = test_definition("lookup");
    definition.exposure = Some(ToolExposure::Search);
    SessionExtensionActions(Arc::downgrade(&session.inner))
        .install_registered_tool(maho_ext_api::RegisteredTool {
            definition,
            source_info: empty_source_info(),
        })
        .unwrap();
    assert!(session.get_active_tool_names().is_empty());
    assert_eq!(session.get_all_tools()[0].exposure, ToolExposure::Search);
    assert!(session.get_registered_tool("lookup").is_some());
}

#[test]
fn armed_eval_only_tools_are_withheld_and_restorable() {
    let session = test_session_with_eval_only(vec!["workflow".to_owned()]);
    session.register_tool_definition(
        test_definition("eval"),
        empty_source_info(),
        test_tool("eval"),
    );
    session.register_tool_definition(
        test_definition("workflow"),
        empty_source_info(),
        test_tool("workflow"),
    );
    session.register_tool_definition(
        test_definition("read"),
        empty_source_info(),
        test_tool("read"),
    );
    session.set_active_tools_by_name(vec!["workflow".to_owned(), "read".to_owned()]);
    assert_eq!(session.get_active_tool_names(), vec!["read".to_owned()]);
    assert!(
        session
            .requested_active_tool_names()
            .unwrap()
            .contains(&"workflow".to_owned())
    );
    assert!(
        session
            .agent()
            .removed_tool_hints()
            .contains_key("workflow")
    );
}

#[test]
fn eval_exposed_tools_are_withheld_by_default_once_eval_is_registered() {
    let session = test_session();
    let mut bash = test_definition("bash");
    bash.exposure = Some(ToolExposure::Eval);
    session.register_tool_definition(bash, empty_source_info(), test_tool("bash"));
    session.register_tool_definition(
        test_definition("read"),
        empty_source_info(),
        test_tool("read"),
    );
    session.set_active_tools_by_name(vec!["bash".to_owned(), "read".to_owned()]);
    assert_eq!(
        session.get_active_tool_names(),
        vec!["bash".to_owned(), "read".to_owned()]
    );

    session.register_tool_definition(
        test_definition("eval"),
        empty_source_info(),
        test_tool("eval"),
    );
    session.set_active_tools_by_name(vec![
        "eval".to_owned(),
        "bash".to_owned(),
        "read".to_owned(),
    ]);
    assert_eq!(
        session.get_active_tool_names(),
        vec!["eval".to_owned(), "read".to_owned()]
    );
    assert!(session.agent().removed_tool_hints().contains_key("bash"));
}

#[test]
fn rebuilt_prompt_uses_dynamic_builder_without_custom_prompt() {
    let session = test_session();
    session.rebuild_system_prompt();
    let prompt = session.system_prompt();
    assert!(prompt.contains("## Intent Gate"));
    assert!(prompt.contains("<workstation>"));
    assert!(!prompt.contains("Current date: "));
}

#[test]
fn custom_system_prompt_is_used_verbatim_with_appends() {
    let session = test_session();
    session.set_system_prompt_sources(
        Some("custom baseline".to_owned()),
        vec!["append context".to_owned()],
    );
    assert_eq!(session.system_prompt(), "custom baseline\n\nappend context");
}

#[test]
fn armed_eval_only_shell_and_grep_get_eval_routing_sentences() {
    let session = test_session_with_eval_only(vec!["bash".to_owned(), "grep".to_owned()]);
    session.register_tool_definition(
        test_definition("eval"),
        empty_source_info(),
        test_tool("eval"),
    );
    session.register_tool_definition(
        test_definition("bash"),
        empty_source_info(),
        test_tool("bash"),
    );
    session.register_tool_definition(
        test_definition("grep"),
        empty_source_info(),
        test_tool("grep"),
    );
    session.set_active_tools_by_name(vec![
        "eval".to_owned(),
        "bash".to_owned(),
        "grep".to_owned(),
    ]);
    let prompt = session.system_prompt();
    assert!(prompt.contains("Shell commands run ONLY inside eval cells via tool.bash({ command: \"...\" }); hooks and permissions still apply."));
    assert!(prompt.ends_with("Text search runs ONLY inside eval cells via tool.grep({ pattern: \"...\", path: \"...\" }); it returns structured matches, respects .gitignore, and applies hooks and permissions - prefer it over rg/grep in tool.bash."));
}

#[test]
fn extension_prompt_options_carry_snippets_and_guidelines_of_withheld_eval_only_tools() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session_with_eval_only(vec!["grep".to_owned()]);
    let mut grep = test_definition("grep");
    grep.prompt_snippet = Some("Search file contents".to_owned());
    grep.prompt_guidelines = Some(vec!["Prefer grep for search".to_owned()]);
    session.register_tool_definition(
        test_definition("eval"),
        empty_source_info(),
        test_tool("eval"),
    );
    session.register_tool_definition(grep, empty_source_info(), test_tool("grep"));
    session.set_active_tools_by_name(vec!["eval".to_owned(), "grep".to_owned()]);
    let options =
        SessionExtensionActions(Arc::downgrade(&session.inner)).get_system_prompt_options();
    assert!(!options.tools.contains(&"grep".to_owned()));
    assert_eq!(
        options.tool_snippets.get("grep").map(String::as_str),
        Some("Search file contents")
    );
    assert_eq!(
        options.prompt_guidelines,
        vec!["Prefer grep for search".to_owned()]
    );
}

fn test_session_with_eval_only(names: Vec<String>) -> AgentSession {
    let runtime = ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()),
        ..Default::default()
    })
    .expect("valid model runtime");
    AgentSession::new(AgentSessionConfig {
        agent: stub_agent(),
        session_manager: SessionManager::in_memory("/tmp", None, None),
        settings_manager: SettingsManager::from_storage(
            Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
            false,
        ),
        cwd: "/tmp".to_owned(),
        agent_dir: None,
        fallback_now: None,
        retry_random: None,
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: BTreeMap::new(),
        custom_tools: Vec::new(),
        model_runtime: Some(runtime),
        model_registry: None,
        uses_default_stream_function: None,
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: Some(names),
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: None,
    })
    .expect("session")
}

fn test_model() -> Model {
    serde_json::from_value(serde_json::json!({
        "id": "faux-1", "name": "faux-1", "api": "faux", "provider": "faux",
        "baseUrl": "", "reasoning": false, "input": [], "contextWindow": 128000, "maxTokens": 4096,
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
    }))
    .expect("model")
}

#[test]
fn stream_request_settings_fill_unpinned_timeouts_retries_and_service_tier() {
    let session = test_session();
    let mut model = test_model();
    model.api = "openai-responses".to_owned();
    session.agent.set_model(model.clone());
    session.set_session_fast_mode(true);
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([
            ("httpIdleTimeoutMs".into(), serde_json::json!(0)),
            ("websocketConnectTimeoutMs".into(), serde_json::json!(4_000)),
            (
                "retry".into(),
                serde_json::json!({"provider": {"maxRetries": 4, "maxRetryDelayMs": 9_000}}),
            ),
        ]))
    });
    let mut options = maho_ai::types::SimpleStreamOptions::default();
    session
        .apply_stream_request_settings(&model, &mut options)
        .expect("valid settings");
    assert_eq!(options.stream.request.timeout_ms, Some(i32::MAX as u64));
    assert_eq!(options.stream.request.max_retries, Some(4));
    assert_eq!(options.stream.request.max_retry_delay_ms, Some(9_000));
    assert_eq!(options.stream.websocket_connect_timeout_ms, Some(4_000));
    assert_eq!(options.service_tier, Some(ServiceTierPreference::Priority));
    let mut pinned = maho_ai::types::SimpleStreamOptions::default();
    pinned.stream.request.timeout_ms = Some(5);
    session
        .apply_stream_request_settings(&model, &mut pinned)
        .expect("valid settings");
    assert_eq!(pinned.stream.request.timeout_ms, Some(5));
}

#[test]
fn declared_provider_request_policy_owns_the_transport_retry_budget() {
    let session = test_session();
    session.agent.set_model(test_model());
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "retry".into(),
            serde_json::json!({"provider": {"maxRetries": 4}}),
        )]))
    });
    let mut runtime = session.model_runtime().clone();
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                retry_policy: Some(
                    maho_ai::utils::retry_profile::profiles::KIMI_CODE_RETRY_PROFILE.clone(),
                ),
                ..Default::default()
            },
        )
        .expect("provider");
    let mut options = maho_ai::types::SimpleStreamOptions::default();
    session
        .apply_stream_request_settings(&test_model(), &mut options)
        .expect("valid settings");
    assert_eq!(options.stream.request.max_retries, Some(0));
}

#[test]
fn request_headers_merge_over_provider_attribution_headers() {
    let session = test_session();
    // SAFETY-DIVERGENCE(senpi packages/coding-agent/src/core/telemetry.ts): attribution headers are opt-in; enable explicitly to cover the merge.
    session.with_settings_manager_mut(|manager| {
        manager
            .set(
                crate::settings_manager::SettingsScope::Global,
                &crate::settings_manager::Settings::from_iter([(
                    "enableInstallTelemetry".to_owned(),
                    Value::Bool(true),
                )]),
            )
            .expect("enable install telemetry");
    });
    let mut model = test_model();
    model.provider = "openrouter".to_owned();
    let request: ProviderHeaders = [
        ("HTTP-Referer".to_owned(), None),
        ("x-request".to_owned(), Some("1".to_owned())),
    ]
    .into_iter()
    .collect();
    let headers = session.merge_request_attribution_headers(&model, None, request);
    assert_eq!(
        headers.get("X-OpenRouter-Categories"),
        Some(&Some("cli-agent".to_owned()))
    );
    assert_eq!(headers.get("HTTP-Referer"), Some(&None));
    assert_eq!(headers.get("x-request"), Some(&Some("1".to_owned())));
}

#[test]
fn request_headers_carry_no_attribution_headers_by_default() {
    let session = test_session();
    let mut model = test_model();
    model.provider = "openrouter".to_owned();
    let request: ProviderHeaders = [("x-request".to_owned(), Some("1".to_owned()))]
        .into_iter()
        .collect();
    let headers = session.merge_request_attribution_headers(&model, None, request);
    assert!(!headers.contains_key("X-OpenRouter-Categories"));
    assert_eq!(headers.get("x-request"), Some(&Some("1".to_owned())));
}

#[test]
fn cursor_admission_caps_tool_results_only_for_cursor_providers() {
    let session = test_session();
    let long = "a".repeat(5000);
    let message: AgentMessage = serde_json::from_value(serde_json::json!({
        "role": "toolResult", "toolCallId": "1", "toolName": "read", "isError": false, "timestamp": 0,
        "content": [{ "type": "text", "text": long }]
    })).expect("message");
    session.agent.set_model(test_model());
    assert_eq!(
        session.admit_cursor_context(vec![message.clone()]),
        vec![message.clone()]
    );
    let mut cursor = test_model();
    cursor.provider = "cursor".to_owned();
    cursor.id = "maho-test-cursor-admission-unobserved".to_owned();
    session.agent.set_model(cursor);
    let admitted = serde_json::to_value(session.admit_cursor_context(vec![message])).expect("json");
    let text = admitted[0]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(text.ends_with("...[truncated]"));
    assert!(text.chars().count() <= crate::cursor_history_admission::CURSOR_TOOL_RESULT_MAX_CHARS);
}

#[test]
fn declared_retry_profile_ignores_global_budget_and_accepts_provider_override() {
    let session = test_session();
    session.agent.set_model(test_model());
    session
        .with_settings_manager_mut(|manager| {
            manager.set(
                crate::settings_manager::SettingsScope::Global,
                &Map::from_iter([(
                    "retry".to_owned(),
                    serde_json::json!({"maxRetries": 2, "baseDelayMs": 1}),
                )]),
            )
        })
        .expect("settings");
    let mut runtime = session.model_runtime().clone();
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                retry_policy: Some(
                    maho_ai::utils::retry_profile::profiles::KIMI_CODE_RETRY_PROFILE.clone(),
                ),
                ..Default::default()
            },
        )
        .expect("provider");
    let profile = session.resolve_retry_profile();
    assert_eq!(
        (profile.turn.max_retries, profile.turn.backoff.base_delay_ms),
        (9, 500.0)
    );
    session
        .with_settings_manager_mut(|manager| {
            manager.set(
                crate::settings_manager::SettingsScope::Global,
                &Map::from_iter([(
                    "retry".to_owned(),
                    serde_json::json!({"providers": {"faux": {
                        "turn": {"maxRetries": 3, "baseDelayMs": 7, "enabled": false}
                    }}}),
                )]),
            )
        })
        .expect("override");
    let profile = session.resolve_retry_profile();
    assert_eq!(
        (profile.turn.max_retries, profile.turn.backoff.base_delay_ms),
        (3, 7.0)
    );
    assert!(!profile.turn.enabled);
}

#[test]
fn extension_context_on_user_activity_forwards_until_unsubscribed() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let seen: Arc<Mutex<Vec<UserActivityKind>>> = Arc::default();
    let recorded = Arc::clone(&seen);
    let unsubscribe = actions
        .on_user_activity(Arc::new(move |kind| lock(&recorded).push(kind)))
        .expect("session reports activity");
    session.record_user_activity(UserActivityKind::Draft);
    session.record_user_activity(UserActivityKind::Prompt);
    unsubscribe();
    session.record_user_activity(UserActivityKind::Draft);
    assert_eq!(
        lock(&seen).clone(),
        vec![UserActivityKind::Draft, UserActivityKind::Prompt]
    );
}

#[tokio::test]
async fn extension_context_prompt_cache_prefix_request_is_the_empty_turn_prefix() {
    use maho_ext_api::{
        ExtensionContextActions, PromptCachePrefixRequestOptions, PromptCachePrefixResult,
    };
    let session = test_session();
    session.agent.set_model(test_model());
    session.register_tool_definition(
        test_definition("read"),
        empty_source_info(),
        test_tool("read"),
    );
    session.set_active_tools_by_name(vec!["read".to_owned()]);
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let result = actions
        .get_prompt_cache_prefix_request(PromptCachePrefixRequestOptions {
            sizes_only: true,
            signal: None,
        })
        .await
        .expect("live session");
    let PromptCachePrefixResult::Ready(request) = result else {
        panic!("prefix skipped");
    };
    assert_eq!(request.model.id, "faux-1");
    assert!(request.context.messages.is_empty());
    assert_eq!(
        request.context.system_prompt.as_deref(),
        Some(session.state().base_system_prompt.as_str())
    );
    assert_eq!(
        request
            .context
            .tools
            .unwrap_or_default()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["read"]
    );
    assert_eq!(
        request.options.stream.session_id,
        Some(session.session_id())
    );
    assert!(request.options.stream.request.api_key.is_none());

    let controller = maho_ai::utils::abort::AbortController::new();
    controller.abort(None);
    let cancelled = actions
        .get_prompt_cache_prefix_request(PromptCachePrefixRequestOptions {
            sizes_only: true,
            signal: Some(controller.signal()),
        })
        .await
        .expect("live session");
    assert!(matches!(cancelled, PromptCachePrefixResult::Skipped(_)));
}

/// senpi#2115 and `ready`: a prefix build waits for the session start to settle, and a prompt
/// composing its turn cancels every in-flight build with the prompt-started reason.
#[tokio::test]
async fn prefix_builds_wait_for_session_start_and_a_prompt_cancels_them() {
    use maho_ext_api::{PromptCachePrefixRequestOptions, PromptCachePrefixResult};
    fn pending<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> bool {
        future
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    }
    let options = || PromptCachePrefixRequestOptions {
        sizes_only: true,
        signal: None,
    };
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "done",
            Default::default(),
        )],
        0,
    );
    let settlement = session.begin_session_start_settlement();
    let mut cancelled = std::pin::pin!(session.prompt_cache_prefix_request(options()));
    assert!(pending(cancelled.as_mut()));
    session
        .prompt("hello", PromptOptions::default())
        .await
        .expect("prompt");
    match cancelled.await {
        PromptCachePrefixResult::Skipped(reason) => assert_eq!(
            reason,
            crate::prompt_cache_prefix_request::PROMPT_STARTED_REASON
        ),
        PromptCachePrefixResult::Ready(_) => panic!("a prompt must cancel the build"),
    }
    let mut waiting = std::pin::pin!(session.prompt_cache_prefix_request(options()));
    assert!(pending(waiting.as_mut()));
    drop(settlement);
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .expect("settled build completes"),
        PromptCachePrefixResult::Ready(_)
    ));
}

#[test]
fn extension_context_reads_configured_settings_and_timeout_bounds() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    session.register_tool_definition(
        test_definition("read"),
        empty_source_info(),
        test_tool("read"),
    );
    session.set_active_tools_by_name(vec!["read".to_owned()]);
    let options =
        SessionExtensionActions(Arc::downgrade(&session.inner)).get_system_prompt_options();
    assert_eq!(options.tools, vec!["read"]);
    assert_eq!(options.cwd, std::path::PathBuf::from(session.cwd()));
    session.set_system_prompt_sources(
        Some("custom baseline".to_owned()),
        vec!["append context".to_owned()],
    );
    let options =
        SessionExtensionActions(Arc::downgrade(&session.inner)).get_system_prompt_options();
    assert_eq!(options.custom_prompt.as_deref(), Some("custom baseline"));
    assert_eq!(
        options.append_system_prompt.as_deref(),
        Some("append context")
    );
    session
        .agent
        .set_system_prompt("hook selected prompt".to_owned());
    assert_eq!(session.system_prompt(), "hook selected prompt");
    session.with_settings_manager_mut(|manager| manager.set(
        crate::settings_manager::SettingsScope::Global,
        &Map::from_iter([
            ("promptCache".to_owned(), serde_json::json!({"goalBackstopMaxSeconds": 99, "cacheAwareTimeouts": false,
                "keepAlive": {"enabled": true, "maxRequestsPerSession": 8, "maxCostUsdPerSession": 0.2, "marginSeconds": 12}})),
            ("lookAt".to_owned(), serde_json::json!({"enabled": false, "models": ["faux/faux-1"]})),
            ("askUser".to_owned(), serde_json::json!({"enabled": false, "timeoutMinutes": 130.5})),
            ("images".to_owned(), serde_json::json!({"autoResize": false, "blockImages": true})),
        ]),
    )).expect("settings");
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    assert_eq!(actions.get_prompt_cache_safe_wait_seconds(), None);
    assert_eq!(actions.get_prompt_cache_goal_backstop_max_seconds(), 99.0);
    let keep_alive = actions.get_prompt_cache_keep_alive_settings();
    assert!(keep_alive.enabled);
    assert_eq!(
        (
            keep_alive.max_requests_per_session,
            keep_alive.margin_seconds
        ),
        (8, 12.0)
    );
    assert_eq!(keep_alive.max_cost_usd_per_session, 0.2);
    assert!(!actions.get_look_at_settings().enabled);
    assert_eq!(
        actions.get_look_at_settings().models,
        Some(vec!["faux/faux-1".to_owned()])
    );
    assert!(!actions.get_ask_user_settings().enabled);
    assert_eq!(actions.get_ask_user_settings().timeout_minutes, 120.0);
    assert!(!actions.get_image_settings().auto_resize);
    assert!(actions.get_image_settings().block_images);
}

#[test]
fn provider_turn_override_applies_all_validated_backoff_knobs() {
    let session = test_session();
    session.agent.set_model(test_model());
    let mut runtime = session.model_runtime().clone();
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                retry_policy: Some(
                    maho_ai::utils::retry_profile::profiles::KIMI_CODE_RETRY_PROFILE.clone(),
                ),
                ..Default::default()
            },
        )
        .expect("provider");
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "retry".to_owned(),
            serde_json::json!({"providers": {"faux": {"turn": {
                "growthFactor": 3, "perAttemptCapMs": null, "serverHintMaxDelayMs": 45,
                "jitter": {"mode": "subtractive", "ratio": 0.5}
            }}}}),
        )]))
    });
    let profile = session.resolve_retry_profile();
    assert_eq!(profile.turn.backoff.growth_factor, 3.0);
    assert_eq!(profile.turn.backoff.per_attempt_cap_ms, None);
    assert_eq!(
        profile.turn.backoff.jitter,
        maho_ai::utils::retry_profile::types::RetryJitterPolicy::Subtractive { ratio: 0.5 }
    );
    assert!(matches!(profile.turn.server_hint,
        maho_ai::utils::retry_profile::types::RetryServerHintPolicy::Override { ceiling, .. }
            if ceiling.max_delay_ms == Some(45)));
}

#[tokio::test]
async fn extension_compaction_signal_tracks_abort_and_feedback_completion() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let signal = actions
        .begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual,
        })
        .expect("feedback admitted");
    assert!(session.is_compacting());
    assert!(
        actions
            .begin_compaction(maho_ext_api::BeginCompactionOptions {
                reason: maho_ext_api::CompactionReason::Manual,
            })
            .is_none()
    );
    let cancelled = signal.cancelled();
    tokio::pin!(cancelled);
    session.abort_compaction();
    tokio::time::timeout(std::time::Duration::from_secs(1), cancelled)
        .await
        .expect("abort signal");
    actions.end_compaction(maho_ext_api::EndCompactionOptions {
        reason: maho_ext_api::CompactionReason::Manual,
        signal: Some(signal.clone()),
        aborted: Some(true),
        error_message: None,
    });
    assert!(signal.is_aborted());
    assert!(!session.is_compacting());
    assert_eq!(session.compaction_state().status(), "aborted");
    assert_eq!(session.compaction_state().generation(), 1);
}

#[tokio::test]
async fn custom_message_end_persists_custom_entry() {
    // Given a session and an extension custom message.
    let session = test_session();
    let message = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(
        maho_agent::harness::messages::CustomMessage {
            role: "custom".to_owned(),
            custom_type: "notice".to_owned(),
            content: maho_agent::harness::messages::CustomMessageContent::Text(
                "payload".to_owned(),
            ),
            display: true,
            details: Some(serde_json::json!({"origin": "extension"})),
            timestamp: 0,
        },
    ));
    // When the finalized message passes through session persistence.
    session
        .process_agent_event(
            maho_agent::types::AgentEvent::MessageEnd { message },
            maho_ai::utils::abort::AbortController::new().signal(),
        )
        .await;
    // Then it is stored as a custom entry, not an LLM message.
    let entries = session.with_session_manager(|manager| manager.entries());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["type"], "custom_message");
    assert_eq!(entries[0]["customType"], "notice");
    assert_eq!(entries[0]["content"], "payload");
    assert_eq!(entries[0]["details"]["origin"], "extension");
    assert_eq!(session.message_revision(), 1);
}

#[tokio::test]
async fn nontriggering_custom_messages_flush_at_turn_end() {
    let session = test_session();
    session
        .state()
        .pending_custom_messages
        .push(maho_agent::harness::messages::CustomMessage {
            role: "custom".into(),
            custom_type: "aside".into(),
            content: maho_agent::harness::messages::CustomMessageContent::Text("after turn".into()),
            display: true,
            details: None,
            timestamp: 0,
        });
    session
        .process_agent_event(
            maho_agent::types::AgentEvent::TurnEnd {
                message: make_user_message("turn", None),
                tool_results: Vec::new(),
            },
            maho_ai::utils::abort::AbortController::new().signal(),
        )
        .await;
    assert!(session.state().pending_custom_messages.is_empty());
    assert_eq!(session.messages()[0].role(), "custom");
    assert_eq!(
        session
            .with_session_manager(|manager| manager.entries())
            .len(),
        1
    );
}

#[tokio::test]
async fn replacement_is_retained_in_later_turn_events() {
    // Given an earlier message_end replacement and a later turn_end carrying the original.
    let session = test_session();
    let original = make_user_message("original", None);
    let replacement = make_user_message("replacement", None);
    session
        .state()
        .message_replacements
        .push((original.clone(), replacement.clone()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription =
        session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
    // When the turn ends.
    session
        .process_agent_event(
            maho_agent::types::AgentEvent::TurnEnd {
                message: original,
                tool_results: Vec::new(),
            },
            maho_ai::utils::abort::AbortController::new().signal(),
        )
        .await;
    // Then listeners receive the replacement, and the next turn advances.
    assert!(matches!(&lock(&events)[0], AgentSessionEvent::Agent(
        maho_agent::types::AgentEvent::TurnEnd { message, .. }
    ) if message == &replacement));
    assert_eq!(session.state().turn_index, 1);
}

#[tokio::test]
async fn extension_idle_tracks_prompt_work_before_streaming() {
    let session = test_session();
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    assert!(maho_ext_api::ExtensionContextActions::is_idle(&actions));
    let work = session.work_barrier.begin();
    // senpi: input hooks run before `_isAgentRunActive` flips, so admission work alone is idle.
    assert!(maho_ext_api::ExtensionContextActions::is_idle(&actions));
    session.agent_run_active.store(true, Ordering::SeqCst);
    assert!(!maho_ext_api::ExtensionContextActions::is_idle(&actions));
    drop(work);
    session.emit_agent_settled().await;
    assert!(maho_ext_api::ExtensionContextActions::is_idle(&actions));
    session.dispose().await;
}

#[tokio::test]
async fn agent_start_resets_turn_index() {
    // Given a session whose previous run ended on a later turn.
    let session = test_session();
    session.state().turn_index = 4;
    // When a new run starts.
    session
        .process_agent_event(
            maho_agent::types::AgentEvent::AgentStart,
            maho_ai::utils::abort::AbortController::new().signal(),
        )
        .await;
    // Then its first extension turn starts at zero.
    assert_eq!(session.state().turn_index, 0);
}

#[tokio::test]
async fn agent_start_closes_previous_agent_end_boundary() {
    // Given a run that ended and left its agent_end boundary open (retry/continuation).
    let session = test_session();
    let boundary = session
        .state()
        .abort_provenance
        .begin_agent_end(Vec::new(), true, false);
    session.state().abort_provenance.end_agent_end(&boundary);
    // When the next run starts.
    session
        .process_agent_event(
            maho_agent::types::AgentEvent::AgentStart,
            maho_ai::utils::abort::AbortController::new().signal(),
        )
        .await;
    // Then a user abort is no longer swallowed as a late join of the finished run.
    let mut state = session.state();
    assert!(!state.abort_provenance.has_open_agent_end_boundary());
    assert!(
        state
            .abort_provenance
            .join_open_boundary(maho_ext_api::AbortSource::User)
            .is_none()
    );
}

fn retry_session(
    responses: Vec<maho_ai::types::AssistantMessage>,
    max_retries: u32,
) -> AgentSession {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(responses.into_iter().map(Into::into).collect());
    let streams = faux_streams(provider.core.clone());
    let session = test_session_with_stream_function(false);
    // senpi test/suite/harness.ts: the faux provider is registered, so `prompt()`'s auth gate admits it.
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    session
        .agent
        .set_stream_function(Arc::new(move |model, context, options| {
            streams.stream_simple(model, context, options.map(|options| options.simple))
        }));
    session.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global, &serde_json::from_value(
        serde_json::json!({"retry":{"enabled":true,"maxRetries":max_retries,"baseDelayMs":0,"modelFallback":false}})
    ).expect("settings")).expect("save settings"));
    session
}

#[tokio::test]
async fn generated_branch_summary_is_attached_at_target_with_source_identity() {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider};
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(vec![
        maho_ai::providers::faux::faux_assistant_message("branch digest", Default::default())
            .into(),
    ]);
    let session = test_session_with_stream_function(false);
    route_agent_stream_to_model_runtime(&session);
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("faux auth");
    let root = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"root","timestamp":0}))
    });
    let old = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"branch","timestamp":1}))
    });
    session.rebuild_session_context().expect("context");
    let result = session
        .navigate_tree(
            root["id"].as_str().expect("root"),
            TreeNavigationOptions {
                summarize: Some(true),
                intent: Some(TreeNavigationIntent::Resume),
                ..Default::default()
            },
        )
        .await
        .expect("summary navigation");
    let summary = result.summary_entry.expect("summary entry");
    assert_eq!(summary["fromId"], old["id"]);
    assert!(
        summary["summary"]
            .as_str()
            .expect("summary")
            .ends_with("branch digest")
    );
    assert_eq!(summary["fromHook"], false);
    assert_eq!(
        session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)),
        root["id"].as_str().map(str::to_owned)
    );
    assert_eq!(session.messages().len(), 1);
}

#[tokio::test]
async fn stalled_summary_aborts_request_under_its_duration_override() {
    struct StalledStreams(std::sync::Mutex<Option<maho_ai::utils::abort::AbortSignal>>);
    impl maho_ai::types::ProviderStreams for StalledStreams {
        fn stream(
            &self,
            _: &Model,
            _: &maho_ai::types::Context,
            _: Option<maho_ai::types::StreamOptions>,
        ) -> maho_ai::types::AssistantMessageEventStream {
            panic!("summary runs through the agent stream function")
        }
        fn stream_simple(
            &self,
            _: &Model,
            _: &maho_ai::types::Context,
            options: Option<maho_ai::types::SimpleStreamOptions>,
        ) -> maho_ai::types::AssistantMessageEventStream {
            *self.0.lock().expect("signal") =
                options.and_then(|options| options.stream.request.signal);
            maho_ai::types::AssistantMessageEventStream::assistant()
        }
    }
    let streams = Arc::new(StalledStreams(std::sync::Mutex::new(None)));
    let session = test_session_with_stream_function(false);
    route_agent_stream_to_model_runtime(&session);
    let mut model = test_model();
    model.provider = "stalled-summary".to_owned();
    session.agent.set_model(model.clone());
    let provider = maho_ai::models::create_provider(maho_ai::models::CreateProviderOptions {
        id: model.provider.clone(),
        name: None,
        base_url: None,
        headers: None,
        auth: None,
        models: vec![model],
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: maho_ai::models::ProviderApi::Single(streams.clone()),
        images: Default::default(),
    })
    .expect("valid stalled provider");
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider);
    runtime
        .register_provider(
            "stalled-summary",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("stalled-fixture".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("fixture auth");
    let controller = maho_ai::utils::abort::AbortController::new();
    // senpi `completeSummarization(..., summarizationMaxDurationMsOverride)`: compaction passes
    // the `compaction.summarizationMaxDurationMs` setting; branch summaries pass none.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        session.complete_summary_stream(
            &session.model(),
            &maho_ai::types::Context::default(),
            Some(maho_ai::types::StreamOptions {
                request: maho_ai::types::ProviderRequestOptions {
                    signal: Some(controller.signal()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            None,
            Some(1.0),
        ),
    )
    .await
    .expect("summary deadline");
    let error = result.expect_err("stalled provider").to_string();
    assert!(error.contains("wall-clock budget"), "{error}");
    assert!(
        streams
            .0
            .lock()
            .expect("signal")
            .as_ref()
            .expect("request signal")
            .aborted()
    );
    assert!(!controller.signal().aborted());
    assert!(session.with_session_manager(|manager| manager.entries().is_empty()));
    session.dispose().await;
}

#[tokio::test]
async fn aborted_branch_summary_does_not_request_or_change_context() {
    let session = test_session_with_stream_function(false);
    let controller = maho_ai::utils::abort::AbortController::new();
    let signal = controller.signal();
    session.state().branch_summary_abort_controller = Some(controller);
    session.abort_branch_summary();
    let result = session.generate_branch_summary_with_signal(&[serde_json::json!({
        "type":"message","id":"branch","parentId":null,"message":{"role":"user","content":"branch task","timestamp":0}
    })], None, false, signal).await.unwrap();
    assert_eq!(result["aborted"], true);
    assert!(session.messages().is_empty());
    assert!(
        session
            .with_session_manager(|manager| manager.entries())
            .is_empty()
    );
}

#[tokio::test]
async fn aborted_branch_provider_response_preserves_navigation_context() {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider};
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(vec![
        maho_ai::providers::faux::faux_assistant_message(
            "",
            maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Aborted),
                ..Default::default()
            },
        )
        .into(),
    ]);
    let session = test_session_with_stream_function(false);
    route_agent_stream_to_model_runtime(&session);
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("faux auth");
    let root = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"root","timestamp":0}))
    });
    let leaf = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"branch","timestamp":1}))
    });
    session.rebuild_session_context().expect("context");
    let result = session
        .navigate_tree(
            root["id"].as_str().expect("root"),
            TreeNavigationOptions {
                summarize: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("cancelled navigation");
    assert!(result.cancelled);
    assert_eq!(result.aborted, Some(true));
    assert_eq!(
        session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)),
        leaf["id"].as_str().map(str::to_owned)
    );
    assert_eq!(
        session.with_session_manager(|manager| manager.entries().len()),
        2
    );
    assert!(!session.is_compacting());
}

#[tokio::test]
async fn manual_compaction_runs_faux_summary_and_records_file_details() {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider};
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        models: Some(vec![
            maho_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_owned(),
                ..Default::default()
            },
            maho_ai::providers::faux::FauxModelDefinition {
                id: "summary".to_owned(),
                ..Default::default()
            },
        ]),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(vec![
        maho_ai::providers::faux::faux_assistant_message("digest", Default::default()).into(),
        maho_ai::providers::faux::faux_assistant_message("prefix digest", Default::default())
            .into(),
    ]);
    let session = test_session_with_stream_function(false);
    route_agent_stream_to_model_runtime(&session);
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("auth");
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"keepRecentTokens":1,"model":"faux/summary"}),
        )]))
    });
    session.with_session_manager_mut(|manager| {
        manager
            .append_message(serde_json::json!({"role":"user","content":"old task","timestamp":0}))
    });
    session.with_session_manager_mut(|manager| {
        manager.append_message(
            serde_json::json!({"role":"user","content":"recent task","timestamp":1}),
        )
    });
    session.with_session_manager_mut(|manager| {
        manager.append_message(
            serde_json::to_value(maho_ai::providers::faux::faux_assistant_message(
                "recent answer",
                Default::default(),
            ))
            .expect("assistant"),
        )
    });
    session.rebuild_session_context().expect("context");
    let result = session.compact(None).await.expect("compaction");
    assert_eq!(
        result.summary,
        "digest\n\n---\n\n**Turn Context (split turn):**\n\nprefix digest"
    );
    assert!(result.details.expect("file details")["readFiles"].is_array());
    assert!(result.usage.is_some());
    assert_eq!(
        result.estimated_tokens_after,
        Some(
            session
                .with_session_manager(|manager| manager.current_context())
                .messages
                .iter()
                .map(crate::compaction::compaction::estimate_tokens)
                .sum::<u64>() as i64
        )
    );
    assert!(!session.is_compacting());
    assert_eq!(session.compaction_state().status(), "completed");
    assert_eq!(session.compaction_state().generation(), 1);
    assert_eq!(session.model().id, "faux-1");
    assert_eq!(
        provider
            .get_call_log()
            .iter()
            .map(|call| call.model_id.as_str())
            .collect::<Vec<_>>(),
        ["summary", "summary"]
    );
    assert_eq!(
        session.with_session_manager(|manager| {
            manager.entries().last().expect("entry")["type"].clone()
        }),
        "compaction"
    );
}

// A relative reset keeps the account cooldown off the wall clock the test session runs on.
const SPENT_ACCOUNT_LIMIT: &str = r#"429 {"error":{"type":"rate_limit_error","message":"You have hit your session limit; resets in 5 hours"}}"#;

/// senpi test/suite/regressions/3131-spent-account.test.ts fixture: a keyed faux session whose primary
/// account was spent by a real account-limited turn, plus authenticated alternates on other providers.
async fn spent_account_session(
    alternates: &[(&str, u64)],
    compaction: Value,
) -> (AgentSession, maho_ai::providers::faux::FauxProviderHandle) {
    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, FauxModelDefinition, RegisterFauxProviderOptions,
        faux_assistant_message, faux_provider,
    };
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        models: Some(vec![FauxModelDefinition {
            id: "primary".to_owned(),
            context_window: Some(1_000_000),
            ..Default::default()
        }]),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(vec![
        faux_assistant_message(
            "",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some(SPENT_ACCOUNT_LIMIT.to_owned()),
                ..Default::default()
            },
        )
        .into(),
    ]);
    let session = test_session_with_stream_function(false);
    route_agent_stream_to_model_runtime(&session);
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    for (name, context_window) in alternates {
        let alternate = faux_provider(RegisterFauxProviderOptions {
            provider: Some((*name).to_owned()),
            tokens_per_second: Some(0.0),
            models: Some(vec![FauxModelDefinition {
                id: (*name).to_owned(),
                context_window: Some(*context_window),
                max_tokens: Some(*context_window / 10),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .expect("valid alternate provider");
        alternate.set_responses(vec![
            faux_assistant_message(format!("{name} summary"), Default::default()).into(),
            faux_assistant_message(format!("{name} prefix"), Default::default()).into(),
        ]);
        runtime.register_native_provider(alternate.provider.clone());
    }
    for name in std::iter::once("faux").chain(alternates.iter().map(|(name, _)| *name)) {
        runtime
            .register_provider(
                name,
                crate::provider_composer::ProviderConfigInput {
                    config: crate::model_config_schema::ModelsJsonProvider {
                        api_key: Some("faux-test".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .expect("auth");
    }
    session
        .agent
        .set_model(provider.get_model(Some("primary")).expect("model"));
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), compaction),
            ("retry".to_owned(), serde_json::json!({"enabled":false})),
        ]))
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("hit the account limit", Default::default()),
    )
    .await
    .expect("bounded account-limited turn")
    .ok();
    for text in ["older turn", "recent turn"] {
        session.with_session_manager_mut(|manager| {
            manager.append_message(
                serde_json::json!({"role":"user","content":text.repeat(100),"timestamp":1}),
            )
        });
        session.with_session_manager_mut(|manager| {
            manager.append_message(
                serde_json::to_value(maho_ai::providers::faux::faux_assistant_message(
                    "prior answer".repeat(100),
                    Default::default(),
                ))
                .expect("assistant"),
            )
        });
    }
    session.rebuild_session_context().expect("context");
    (session, provider)
}

fn spent_primary_calls(provider: &maho_ai::providers::faux::FauxProviderHandle) -> usize {
    provider.get_call_log().len()
}

/// senpi 3131-spent-account.test.ts "reports the account limit without a summary request when no usable
/// model exists": the spent account fails compaction with the account advice and makes no request.
#[tokio::test]
async fn spent_account_without_usable_model_fails_compaction_without_a_summary_request() {
    let (session, provider) =
        spent_account_session(&[], serde_json::json!({"keepRecentTokens":1})).await;
    assert!(
        session
            .retry_fallback
            .lock()
            .await
            .as_mut()
            .expect("controller")
            .is_provider_spent("faux")
    );
    let ends = Arc::new(Mutex::new(Vec::new()));
    let observed = ends.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::CompactionEnd { error_message, .. } = event {
            lock(&observed).push(error_message.clone());
        }
    }));
    let calls_before = spent_primary_calls(&provider);
    let error = session.compact(None).await.expect_err("spent account");
    assert_eq!(
        error,
        "The faux account usage limit has been reached (resets in 5 hours). Switch to another provider with /model, or configure another provider in retry.fallbackChains."
    );
    assert_eq!(spent_primary_calls(&provider), calls_before);
    let last_end = lock(&ends)
        .last()
        .cloned()
        .flatten()
        .expect("compaction_end error");
    assert!(
        last_end.contains(
            "The faux account usage limit has been reached (resets in 5 hours). Switch to another provider with /model"
        ),
        "compaction_end errorMessage: {last_end}"
    );
}

/// The resolver reads the spent accounts the controller publishes, so a task holding the controller lock
/// cannot make a spent provider look usable.
#[tokio::test]
async fn spent_account_stays_spent_while_the_controller_lock_is_held() {
    let (session, _provider) =
        spent_account_session(&[], serde_json::json!({"keepRecentTokens":1})).await;
    let held = session.retry_fallback.lock().await;
    let error = session
        .resolve_compaction_model(&session.model())
        .expect_err("spent account under contention");
    drop(held);
    assert_eq!(
        error,
        "The faux account usage limit has been reached (resets in 5 hours). Switch to another provider with /model, or configure another provider in retry.fallbackChains."
    );
}

/// senpi 3131-spent-account.test.ts "skips an authenticated summary alternate whose context window cannot
/// fit the input": a spent account summarizes on the first authenticated model that fits.
#[tokio::test]
async fn spent_account_compacts_on_an_authenticated_alternate_that_fits() {
    let (session, provider) = spent_account_session(
        &[("tiny", 1_000), ("roomy", 128_000)],
        serde_json::json!({"keepRecentTokens":1}),
    )
    .await;
    let calls_before = spent_primary_calls(&provider);
    let resolved = session
        .resolve_compaction_model(&session.model())
        .expect("usable alternate");
    assert_eq!(
        (resolved.provider.as_str(), resolved.id.as_str()),
        ("roomy", "roomy")
    );
    let result = session.compact(None).await.expect("compaction");
    assert!(result.summary.starts_with("roomy summary"));
    assert_eq!(spent_primary_calls(&provider), calls_before);
    assert_eq!(session.model().id, "primary");
}

/// senpi `_resolveCompactionModel`: a spent account uses a usable configured override before any other
/// authenticated model, and a deliberate manual selection clears the spent account (3131 "allows a
/// deliberate manual model selection to probe an account again").
#[tokio::test]
async fn spent_account_prefers_the_override_and_a_manual_selection_clears_it() {
    let (session, _provider) = spent_account_session(
        &[("roomy", 128_000), ("other", 128_000)],
        serde_json::json!({"keepRecentTokens":1,"model":"other/other"}),
    )
    .await;
    let resolved = session
        .resolve_compaction_model(&session.model())
        .expect("override");
    assert_eq!(resolved.provider, "other");
    let primary = session.model();
    session
        .set_session_model(primary.clone())
        .await
        .expect("manual selection");
    assert!(
        !session
            .retry_fallback
            .lock()
            .await
            .as_mut()
            .expect("controller")
            .is_provider_spent("faux")
    );
    assert_eq!(
        session
            .resolve_compaction_model(&primary)
            .expect("healthy override")
            .provider,
        "other"
    );
}

/// senpi 3131-spent-account.test.ts "uses the held target for %s compaction without calling the spent
/// provider" (manual and pre-prompt): a spent account whose transcript holds a downswitch summarizes on
/// the held target, and the spent primary is never called.
async fn held_target_compacts_without_calling_the_spent_provider(pre_prompt: bool) {
    use maho_ai::providers::faux::{
        FauxModelDefinition, FauxResponseStep, RegisterFauxProviderOptions, faux_assistant_message,
        faux_provider,
    };
    let (session, primary_provider) =
        spent_account_session(&[], serde_json::json!({"keepRecentTokens":1})).await;
    let target_provider = faux_provider(RegisterFauxProviderOptions {
        provider: Some("usable".to_owned()),
        tokens_per_second: Some(0.0),
        models: Some(vec![FauxModelDefinition {
            id: "target".to_owned(),
            context_window: Some(372_000),
            max_tokens: Some(32_000),
            ..Default::default()
        }]),
        ..Default::default()
    })
    .expect("valid target provider");
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(target_provider.provider.clone());
    runtime
        .register_provider(
            "usable",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-key".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("auth");
    let target = session
        .model_runtime()
        .get_model("usable", "target")
        .expect("missing target fixture");
    let primary = session.model();
    for text in ["large live context ".repeat(30_000), "continue".to_owned()] {
        session.with_session_manager_mut(|manager| {
            manager.append_message(
                serde_json::json!({"role":"user","content":[{"type":"text","text":text}],"timestamp":1}),
            )
        });
        let mut response = faux_assistant_message("earlier response", Default::default());
        response.provider = primary.provider.clone();
        response.model = primary.id.clone();
        response.usage.input = 320_000;
        response.usage.total_tokens = 321_000;
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::to_value(response).expect("assistant"))
        });
    }
    session.rebuild_session_context().expect("context");
    session
        .set_model(target.clone())
        .await
        .expect("held switch");
    assert_eq!(
        session.pending_model_switch().expect("pending").model.id,
        "target"
    );
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let step = || {
        let requests = requests.clone();
        FauxResponseStep::Factory(Arc::new(move |_context, _options, _state, model| {
            lock(&requests).push(format!("{}/{}", model.provider, model.id));
            Box::pin(std::future::ready(faux_assistant_message(
                "a usable model summary",
                Default::default(),
            )))
        }))
    };
    target_provider.set_responses(vec![step(), step(), step()]);
    let reduced = Arc::new(Mutex::new(0usize));
    let observed = reduced.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(event, AgentSessionEvent::ResumeContextReduced { .. }) {
            *lock(&observed) += 1;
        }
    }));
    let primary_calls_before = spent_primary_calls(&primary_provider);

    if pre_prompt {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            session.prompt("continue after switching", Default::default()),
        )
        .await
        .expect("bounded pre-prompt turn")
        .expect("pre-prompt turn");
    } else {
        tokio::time::timeout(std::time::Duration::from_secs(10), session.compact(None))
            .await
            .expect("bounded manual compaction")
            .expect("manual compaction");
    }

    let requests = lock(&requests).clone();
    assert_eq!(requests.first().map(String::as_str), Some("usable/target"));
    assert!(!requests.iter().any(|request| request == "faux/primary"));
    assert_eq!(spent_primary_calls(&primary_provider), primary_calls_before);
    let compaction = session
        .with_session_manager(|manager| manager.entries())
        .into_iter()
        .rev()
        .find(|entry| entry["type"] == "compaction")
        .expect("compaction entry");
    assert!(
        compaction["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("a usable model summary"))
    );
    assert_eq!(*lock(&reduced), 0);
    if pre_prompt {
        assert_eq!(session.model().provider, "usable");
        assert!(session.pending_model_switch().is_none());
    }
}

#[tokio::test]
async fn held_target_serves_manual_compaction_without_calling_the_spent_provider() {
    held_target_compacts_without_calling_the_spent_provider(false).await;
}

#[tokio::test]
async fn held_target_serves_pre_prompt_compaction_without_calling_the_spent_provider() {
    held_target_compacts_without_calling_the_spent_provider(true).await;
}

/// senpi `_resolveCompactionModel` (healthy session): an empty `compaction.model` is unset (`!override`),
/// so the session model serves and no `compaction_model_fallback` notice is recorded.
#[test]
fn empty_compaction_model_override_is_unset() {
    let session = keyed_test_session();
    session.agent.set_model(test_model());
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"model":""}),
        )]))
    });
    let resolved = session
        .resolve_compaction_model(&session.model())
        .expect("session model");
    assert_eq!(resolved.id, test_model().id);
    assert_eq!(session.state().compaction_model_fallback_notice, None);
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"model":"no-slash"}),
        )]))
    });
    session
        .resolve_compaction_model(&session.model())
        .expect("session model");
    assert_eq!(
        session.state().compaction_model_fallback_notice.as_deref(),
        Some("no-slash\u{0}malformed")
    );
}

/// senpi `_resolveCompactionModel` (healthy session): an override on a spent provider keeps the session
/// model.
#[tokio::test]
async fn healthy_session_keeps_its_model_when_the_override_provider_is_spent() {
    let (session, _provider) = spent_account_session(
        &[("other", 128_000)],
        serde_json::json!({"keepRecentTokens":1,"model":"faux/primary"}),
    )
    .await;
    let other = session
        .model_runtime()
        .get_model("other", "other")
        .expect("other");
    session.agent.set_model(other.clone());
    let resolved = session
        .resolve_compaction_model(&other)
        .expect("session model");
    assert_eq!(resolved.provider, "other");
}

#[tokio::test]
async fn cancelled_compaction_hook_makes_no_summary_request() {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider};
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    let session = test_session_with_stream_function(false);
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("auth");
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"keepRecentTokens":1}),
        )]))
    });
    for (index, text) in ["old task", "recent task"].into_iter().enumerate() {
        session.with_session_manager_mut(|manager| {
            manager
                .append_message(serde_json::json!({"role":"user","content":text,"timestamp":index}))
        });
    }
    session.rebuild_session_context().expect("context");
    let messages = session.messages();
    let entries = session.with_session_manager(|manager| conversation_entries(manager.entries()));
    let (started, entered) = tokio::sync::oneshot::channel();
    let started = Arc::new(Mutex::new(Some(started)));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:compaction-abort>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(move |event, _| {
            let started = started.clone();
            Box::pin(async move {
                let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                    panic!("compaction event");
                };
                lock(&started)
                    .take()
                    .expect("single hook")
                    .send(())
                    .expect("observer");
                event.signal.cancelled().await;
                Ok(maho_ext_api::EventResult::None)
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let (result, prompted) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(session.compact(None), async {
            use std::{future::Future, task::Poll};
            entered.await.expect("hook started");
            let mut prompt = Box::pin(session.prompt(
                "retained after running compaction",
                PromptOptions {
                    source: Some(InputSource::Extension),
                    streaming_behavior: Some(StreamingBehavior::FollowUp),
                    ..Default::default()
                },
            ));
            std::future::poll_fn(|cx| {
                assert!(prompt.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            session.abort_compaction();
            prompt.await
        })
    })
    .await
    .expect("bounded compaction cancellation");
    assert!(result.is_err());
    assert_eq!(
        prompted.expect("retained extension prompt"),
        PromptDisposition::Queued
    );
    assert_eq!(
        session.get_follow_up_messages(),
        ["retained after running compaction"]
    );
    session.clear_queue(false);
    assert!(provider.get_call_log().is_empty());
    assert_eq!(session.messages(), messages);
    assert_eq!(
        session.with_session_manager(|manager| conversation_entries(manager.entries())),
        entries
    );
    assert_eq!(session.compaction_state().status(), "aborted");
    assert!(!session.is_compacting());
}

#[tokio::test]
async fn summary_retry_preserves_affinity_and_isolates_request_identity() {
    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, FauxResponseStep, RegisterFauxProviderOptions,
        faux_assistant_message, faux_provider,
    };
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    let options_seen = Arc::new(Mutex::new(Vec::new()));
    let make_response = |failure: bool| {
        let options_seen = options_seen.clone();
        FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
            lock(&options_seen).push(options.expect("summary options").stream.clone());
            Box::pin(async move {
                faux_assistant_message(
                    if failure { "" } else { "summary" },
                    FauxAssistantMessageOptions {
                        stop_reason: failure.then_some(maho_ai::types::StopReason::Error),
                        error_message: failure.then(|| "socket hang up".to_owned()),
                        ..Default::default()
                    },
                )
            })
        }))
    };
    provider.set_responses(vec![make_response(true), make_response(false)]);
    let session = test_session();
    route_agent_stream_to_model_runtime(&session);
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("auth");
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "retry".into(),
            serde_json::json!({"enabled":true,"maxRetries":1,"baseDelayMs":0}),
        )]))
    });
    let model = provider.get_model(Some("faux-1")).expect("model");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.complete_summary_stream(
            &model,
            &maho_ai::types::Context::default(),
            Some(maho_ai::types::StreamOptions {
                session_id: Some("caller-affinity".into()),
                ..Default::default()
            }),
            None,
            None,
        ),
    )
    .await;
    session.dispose().await;
    let response = result.expect("bounded retries").expect("retried summary");
    assert_eq!(
        maho_ai::utils::text::content_text(&response.content, ""),
        "summary"
    );
    let seen = lock(&options_seen);
    assert_eq!(seen.len(), 2);
    assert_ne!(seen[0].session_id.as_deref(), Some("caller-affinity"));
    assert_eq!(seen[0].session_id, seen[1].session_id);
    assert_eq!(
        seen[0].request.affinity_session_id.as_deref(),
        Some("caller-affinity")
    );
    assert_eq!(
        seen[1].request.affinity_session_id.as_deref(),
        Some("caller-affinity")
    );
    assert_eq!(
        seen[0].cache_retention,
        Some(maho_ai::types::CacheRetention::None)
    );
    assert_eq!(
        seen[1].cache_retention,
        Some(maho_ai::types::CacheRetention::None)
    );
}

#[tokio::test]
async fn deterministic_summary_failure_preserves_explicit_retention_without_retry() {
    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, FauxResponseStep, RegisterFauxProviderOptions,
        faux_assistant_message, faux_provider,
    };
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    provider.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |_, options, _, _| {
            lock(&captured).push(options.expect("summary options").stream.clone());
            Box::pin(async {
                faux_assistant_message(
                    "",
                    FauxAssistantMessageOptions {
                        stop_reason: Some(maho_ai::types::StopReason::Error),
                        error_message: Some("invalid request fixture".into()),
                        ..Default::default()
                    },
                )
            })
        },
    ))]);
    let session = test_session();
    route_agent_stream_to_model_runtime(&session);
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("auth");
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "retry".into(),
            serde_json::json!({"enabled":true,"maxRetries":3,"baseDelayMs":0}),
        )]))
    });
    let model = provider.get_model(Some("faux-1")).expect("model");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.complete_summary_stream(
            &model,
            &maho_ai::types::Context::default(),
            Some(maho_ai::types::StreamOptions {
                cache_retention: Some(maho_ai::types::CacheRetention::Short),
                session_id: Some("caller".into()),
                request: maho_ai::types::ProviderRequestOptions {
                    affinity_session_id: Some("explicit-affinity".into()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            None,
            None,
        ),
    )
    .await;
    session.dispose().await;
    let response = result
        .expect("bounded deterministic error")
        .expect("error response returned");
    assert_eq!(response.stop_reason, maho_ai::types::StopReason::Error);
    assert_eq!(
        crate::compaction::compaction::get_summarization_failure(&response, "Branch summarization")
            .as_deref(),
        Some("Branch summarization failed: invalid request fixture")
    );
    let seen = lock(&seen);
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].cache_retention,
        Some(maho_ai::types::CacheRetention::Short)
    );
    assert_eq!(
        seen[0].request.affinity_session_id.as_deref(),
        Some("explicit-affinity")
    );
    assert_ne!(seen[0].session_id.as_deref(), Some("caller"));
}

#[tokio::test]
async fn session_abort_cancels_started_summary_without_late_apply() {
    use maho_ai::providers::faux::{FauxResponseStep, RegisterFauxProviderOptions, faux_provider};
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    let (started, entered) = tokio::sync::oneshot::channel();
    let started = Arc::new(Mutex::new(Some(started)));
    let (cancelled, observed) = tokio::sync::oneshot::channel();
    let cancelled = Arc::new(Mutex::new(Some(cancelled)));
    provider.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |_, options, _, _| {
            let signal = options
                .expect("summary options")
                .stream
                .request
                .signal
                .clone()
                .expect("request signal");
            let started = started.clone();
            let cancelled = cancelled.clone();
            Box::pin(async move {
                lock(&started)
                    .take()
                    .expect("single summary request")
                    .send(())
                    .expect("request observer");
                signal.cancelled().await;
                lock(&cancelled)
                    .take()
                    .expect("single cancellation")
                    .send(())
                    .expect("cancellation observer");
                maho_ai::providers::faux::faux_assistant_message("late summary", Default::default())
            })
        },
    ))]);
    let session = test_session_with_stream_function(false);
    route_agent_stream_to_model_runtime(&session);
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("auth");
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"keepRecentTokens":1}),
        )]))
    });
    for (index, text) in ["old task", "recent task"].into_iter().enumerate() {
        session.with_session_manager_mut(|manager| {
            manager
                .append_message(serde_json::json!({"role":"user","content":text,"timestamp":index}))
        });
    }
    session.rebuild_session_context().expect("context");
    let messages = session.messages();
    let entries = session.with_session_manager(|manager| conversation_entries(manager.entries()));
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let (result, ()) = tokio::join!(session.compact(None), async {
            entered.await.expect("actual summary started");
            session.abort().await;
            observed.await.expect("request cancelled");
        });
        result
    })
    .await;
    let after_messages = session.messages();
    let after_entries =
        session.with_session_manager(|manager| conversation_entries(manager.entries()));
    let status = session.compaction_state().status().to_owned();
    let compacting = session.is_compacting();
    let calls = provider.get_call_log().len();
    let cleanup = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        if result.is_err() {
            session.abort().await;
        }
        session.dispose().await;
    })
    .await;
    cleanup.expect("bounded session cleanup");
    assert!(result.expect("bounded request cancellation").is_err());
    assert_eq!(calls, 1);
    assert_eq!(after_messages, messages);
    assert_eq!(after_entries, entries);
    assert_eq!(status, "aborted");
    assert!(!compacting);
}

#[tokio::test]
async fn disposal_cancels_title_and_prevents_late_background_launch() {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider};
    let (started, entered) = tokio::sync::oneshot::channel();
    let started = Arc::new(Mutex::new(Some(started)));
    let (release, released) = tokio::sync::watch::channel(false);
    let provider = faux_provider(RegisterFauxProviderOptions {
        scheduler_hook: Some(Arc::new(move || {
            let started = started.clone();
            let mut released = released.clone();
            Box::pin(async move {
                if let Some(started) = lock(&started).take() {
                    started.send(()).expect("observer");
                }
                released
                    .wait_for(|released| *released)
                    .await
                    .expect("release");
            })
        })),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(vec![
        maho_ai::providers::faux::faux_assistant_message(
            "<title>Stale Title</title>",
            Default::default(),
        )
        .into(),
    ]);
    let session = test_session_with_stream_function(false);
    session.agent.set_timeout_ms(Some(12_345));
    session.agent.set_max_retry_delay_ms(Some(678));
    session
        .agent
        .set_transport(Some(maho_ai::types::Transport::Sse));
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("auth");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            session.generate_session_title_if_needed("Implement a native session"),
            async {
                entered.await.expect("title request started");
                assert!(session.state().session_title_abort_controller.is_some());
                session.dispose().await;
                release.send_replace(true);
            }
        );
    })
    .await
    .expect("bounded title cancellation");
    assert!(session.session_name().is_none());
    let calls = provider.get_call_log();
    let options = calls[0].options.as_ref().expect("title options");
    assert_eq!(options.request.timeout_ms, Some(12_345));
    assert_eq!(options.request.max_retry_delay_ms, Some(678));
    assert_eq!(options.transport, Some(maho_ai::types::Transport::Sse));
    assert!(
        provider.get_call_log()[0]
            .options
            .as_ref()
            .expect("options")
            .request
            .signal
            .as_ref()
            .expect("signal")
            .aborted()
    );
    session
        .generate_session_title_if_needed("Implement another feature")
        .await;
    assert_eq!(provider.get_call_log().len(), 1);
    assert!(session.state().session_title_abort_controller.is_none());
}

#[tokio::test]
async fn title_generation_retries_one_transient_error() {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider};
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(vec![
        maho_ai::providers::faux::faux_assistant_message(
            "",
            maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("503 Service Unavailable".to_owned()),
                ..Default::default()
            },
        )
        .into(),
        maho_ai::providers::faux::faux_assistant_message(
            "<title>Native Session Recovery</title>",
            Default::default(),
        )
        .into(),
    ]);
    let session = test_session_with_stream_function(false);
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("auth");
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "retry".to_owned(),
            serde_json::json!({"enabled":true,"maxRetries":5,"baseDelayMs":0}),
        )]))
    });
    session
        .generate_session_title_if_needed("Recover native session calls")
        .await;
    assert_eq!(
        session.session_name().as_deref(),
        Some("Native Session Recovery")
    );
    assert_eq!(provider.get_call_log().len(), 2);
    assert!(session.state().session_title_abort_controller.is_none());
}

#[tokio::test]
async fn scheduled_continuation_does_not_clear_a_later_user_abort() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "unexpected",
            Default::default(),
        )],
        0,
    );
    session.agent.follow_up(
        session_message_from_value(
            serde_json::json!({"role":"user","content":"queued","timestamp":0}),
        )
        .expect("queued message"),
    );
    let generation = session.user_abort_generation.load(Ordering::SeqCst);
    let admission = session.prompt_admission.lock().await;
    let continuation = session.continue_session_internal(Some(generation));
    tokio::pin!(continuation);
    std::future::poll_fn(|context| {
        assert!(std::future::Future::poll(continuation.as_mut(), context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    session.abort().await;
    drop(admission);
    continuation.await.expect("suppressed continuation");
    assert!(session.state().user_aborted);
    assert!(session.agent.has_queued_messages());
    assert!(session.messages().is_empty());
    assert!(!session.work_barrier.has_active_work());
}

#[tokio::test]
async fn custom_turn_before_agent_start_preserves_validated_delivery_source() {
    let delivery = serde_json::json!({"delivery_id":"delivery-1","source":"session_control","deliverAs":"steer"});
    for (role, custom_type, details, expected) in [
        ("custom", "notice", None, "extension"),
        ("custom", "notice", Some(delivery.clone()), "extension"),
        (
            "custom",
            "session_control_delivery",
            Some(delivery.clone()),
            "delivery",
        ),
        (
            "custom",
            "session_control_delivery",
            Some(
                serde_json::json!({"delivery_id":"","source":"session_control","deliverAs":"followUp"}),
            ),
            "delivery",
        ),
        (
            "other",
            "session_control_delivery",
            Some(delivery),
            "extension",
        ),
        ("custom", "session_control_delivery", None, "extension"),
        (
            "custom",
            "session_control_delivery",
            Some(
                serde_json::json!({"delivery_id":1,"source":"session_control","deliverAs":"steer"}),
            ),
            "extension",
        ),
        (
            "custom",
            "session_control_delivery",
            Some(
                serde_json::json!({"delivery_id":"delivery-1","source":"extension","deliverAs":"steer"}),
            ),
            "extension",
        ),
        (
            "custom",
            "session_control_delivery",
            Some(
                serde_json::json!({"delivery_id":"delivery-1","source":"session_control","deliverAs":"nextTurn"}),
            ),
            "extension",
        ),
    ] {
        let session = retry_session(
            vec![maho_ai::providers::faux::faux_assistant_message(
                "done",
                Default::default(),
            )],
            0,
        );
        let observed = Arc::new(Mutex::new(Vec::new()));
        let captured = observed.clone();
        let mut extension = maho_ext_api::LoadedExtension::new(
            "<inline:turn-source>",
            session.cwd().into(),
            Default::default(),
        );
        extension.handlers.insert(
            maho_ext_api::EventKind::BeforeAgentStart,
            vec![Arc::new(move |event, _| {
                let maho_ext_api::ExtensionEvent::BeforeAgentStart(event) = event else {
                    panic!("before agent start");
                };
                lock(&captured).push((event.trigger.clone(), event.preview, event.prompt.clone()));
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            })],
        );
        session
            .set_extension_runner(maho_ext_host::ExtensionRunner::new(
                vec![extension],
                Default::default(),
                Default::default(),
                test_extension_context(&session),
            ))
            .await;
        let message = maho_agent::harness::messages::CustomMessage {
            role: role.into(),
            custom_type: custom_type.into(),
            content: maho_agent::harness::messages::CustomMessageContent::Text("same text".into()),
            display: true,
            details,
            timestamp: 0,
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session.trigger_custom_message_turn(
                message,
                None,
                session.user_abort_generation.load(Ordering::SeqCst),
            ),
        )
        .await
        .expect("bounded custom turn")
        .expect("custom turn");
        assert_eq!(
            *lock(&observed),
            [(expected.to_owned(), false, "same text".to_owned())]
        );
        session.dispose().await;
    }
}

#[tokio::test]
async fn user_prompt_before_agent_start_reports_non_preview_prompt_source() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "done",
            Default::default(),
        )],
        0,
    );
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:prompt-source>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::BeforeAgentStart,
        vec![Arc::new(move |event, _| {
            let maho_ext_api::ExtensionEvent::BeforeAgentStart(event) = event else {
                panic!("before agent start");
            };
            lock(&captured).push((event.trigger.clone(), event.preview));
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("same text", PromptOptions::default()),
    )
    .await
    .expect("bounded prompt")
    .expect("prompt");
    assert_eq!(*lock(&observed), [("prompt".to_owned(), false)]);
    session.dispose().await;
}

#[tokio::test]
async fn idle_custom_trigger_respects_abort_before_scheduled_admission() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "unexpected",
            Default::default(),
        )],
        0,
    );
    let admission = session.prompt_admission.lock().await;
    maho_ext_api::ExtensionActions::send_message(
        &SessionExtensionActions(Arc::downgrade(&session.inner)),
        maho_ext_api::CustomMessage {
            custom_type: "notice".to_owned(),
            content: vec![maho_tools::definition::ToolContent::text("saved notice")],
            display: true,
            details: None,
        },
        maho_ext_api::SendMessageOptions {
            trigger_turn: true,
            deliver_as: None,
        },
    )
    .expect("custom trigger");
    session.abort().await;
    drop(admission);
    tokio::time::timeout(std::time::Duration::from_secs(5), session.wait_for_idle())
        .await
        .expect("settled scheduled work");
    assert!(session.state().user_aborted);
    assert!(session.messages().is_empty());
    assert!(
        session
            .with_session_manager(|manager| manager.entries())
            .is_empty()
    );
    assert!(!session.agent.has_queued_messages());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_custom_trigger_from_a_short_lived_runtime_still_runs_the_turn() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "woken",
            Default::default(),
        )],
        0,
    );
    let (finished, received) = tokio::sync::oneshot::channel();
    let finished = Arc::new(Mutex::new(Some(finished)));
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(event, AgentSessionEvent::AgentIdle)
            && let Some(finished) = lock(&finished).take()
        {
            finished.send(()).expect("observer");
        }
    }));
    // A completion notifier drains on a foreign thread through a throwaway runtime that is
    // dropped as soon as the synchronous send returns Ok.
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("throwaway runtime");
        runtime.block_on(async {
            maho_ext_api::ExtensionActions::send_message(
                &actions,
                maho_ext_api::CustomMessage {
                    custom_type: "wake".to_owned(),
                    content: vec![maho_tools::definition::ToolContent::text("task done")],
                    display: false,
                    details: None,
                },
                maho_ext_api::SendMessageOptions {
                    trigger_turn: true,
                    deliver_as: Some(maho_ext_api::DeliverAs::Steer),
                },
            )
            .expect("accepted wake");
        });
    })
    .join()
    .expect("sender thread");
    tokio::time::timeout(std::time::Duration::from_secs(5), received)
        .await
        .expect("accepted wake must run its turn after the sender runtime is gone")
        .expect("settled");
    let entries = session.with_session_manager(|manager| manager.entries());
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry["customType"] == "wake")
            .count(),
        1
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["message"]["role"] == "assistant")
    );
    session.dispose().await;
}

#[tokio::test]
async fn custom_trigger_delivery_receipt_settles_only_after_transcript_append_or_loss() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "woken",
            Default::default(),
        )],
        0,
    );
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));

    // 1. An idle wake canceled before admission must resolve its receipt as Failed (never Delivered).
    let admission = session.prompt_admission.lock().await;
    let aborted_receipt = maho_ext_api::ExtensionActions::send_message_with_receipt(
        &actions,
        maho_ext_api::CustomMessage {
            custom_type: "omo-senpi:wake".to_owned(),
            content: vec![maho_tools::definition::ToolContent::text("aborted wake")],
            display: false,
            details: None,
        },
        maho_ext_api::SendMessageOptions {
            trigger_turn: true,
            deliver_as: Some(maho_ext_api::DeliverAs::Steer),
        },
    )
    .expect("wake accepted");
    assert!(
        aborted_receipt.outcome().is_none(),
        "acceptance is not delivery: receipt must stay pending before append"
    );
    let (abort_tx, abort_rx) = tokio::sync::oneshot::channel();
    aborted_receipt.on_settled(move |outcome| {
        let _ = abort_tx.send(outcome);
    });
    session.abort().await;
    drop(admission);
    let abort_outcome = tokio::time::timeout(std::time::Duration::from_secs(5), abort_rx)
        .await
        .expect("aborted wake receipt must settle")
        .expect("abort outcome");
    assert!(abort_outcome.is_err());

    // 2. A wake deferred into an open settled window that is then canceled must resolve Failed.
    lock(&session.settled_delivery).begin(session.user_abort_generation.load(Ordering::SeqCst));
    let deferred_receipt = maho_ext_api::ExtensionActions::send_message_with_receipt(
        &actions,
        maho_ext_api::CustomMessage {
            custom_type: "omo-senpi:wake".to_owned(),
            content: vec![maho_tools::definition::ToolContent::text(
                "dropped settled wake",
            )],
            display: false,
            details: None,
        },
        maho_ext_api::SendMessageOptions {
            trigger_turn: true,
            deliver_as: Some(maho_ext_api::DeliverAs::Steer),
        },
    )
    .expect("deferred wake accepted");
    assert!(deferred_receipt.outcome().is_none());
    lock(&session.settled_delivery).cancel();
    assert!(matches!(deferred_receipt.outcome(), Some(Err(_))));

    // 3. A wake steered into a queue that is cleared before a run consumes it must resolve Failed.
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:abort-during-admission>",
        session.cwd().into(),
        Default::default(),
    );
    let abort_once = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let abort_flag = Arc::clone(&abort_once);
    let abort_session = session.clone();
    extension.handlers.insert(
        maho_ext_api::EventKind::BeforeAgentStart,
        vec![Arc::new(move |_, _| {
            if abort_flag.swap(false, Ordering::SeqCst) {
                abort_session
                    .user_abort_generation
                    .fetch_add(1, Ordering::SeqCst);
            }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let queued_receipt = maho_ext_api::ExtensionActions::send_message_with_receipt(
        &actions,
        maho_ext_api::CustomMessage {
            custom_type: "omo-senpi:wake".to_owned(),
            content: vec![maho_tools::definition::ToolContent::text(
                "steered for later",
            )],
            display: false,
            details: None,
        },
        maho_ext_api::SendMessageOptions {
            trigger_turn: true,
            deliver_as: Some(maho_ext_api::DeliverAs::Steer),
        },
    )
    .expect("wake accepted");
    tokio::time::timeout(std::time::Duration::from_secs(5), session.wait_for_idle())
        .await
        .expect("rejected admission turn settles");
    assert!(session.agent.has_queued_messages());
    assert!(
        queued_receipt.outcome().is_none(),
        "wake sitting in steering queue is not yet in transcript"
    );
    session.clear_queue(false);
    assert!(matches!(queued_receipt.outcome(), Some(Err(_))));

    // 4. A normal idle wake resolves Delivered only once the entry is in the session transcript.
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let delivered_receipt = maho_ext_api::ExtensionActions::send_message_with_receipt(
        &actions,
        maho_ext_api::CustomMessage {
            custom_type: "omo-senpi:wake".to_owned(),
            content: vec![maho_tools::definition::ToolContent::text("delivered wake")],
            display: false,
            details: None,
        },
        maho_ext_api::SendMessageOptions {
            trigger_turn: true,
            deliver_as: Some(maho_ext_api::DeliverAs::Steer),
        },
    )
    .expect("wake accepted");
    delivered_receipt.on_settled(move |outcome| {
        let _ = delivered_tx.send(outcome);
    });
    let delivered_outcome = tokio::time::timeout(std::time::Duration::from_secs(5), delivered_rx)
        .await
        .expect("delivered wake receipt settles")
        .expect("delivered outcome");
    assert_eq!(delivered_outcome, Ok(()));
    let entries = session.with_session_manager(|manager| manager.entries());
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry["customType"] == "omo-senpi:wake")
            .count(),
        1
    );
    session.dispose().await;
}

#[tokio::test]
async fn dropped_pending_compaction_releases_its_admission_owner() {
    let session = test_session();
    let admission = session.prompt_admission.lock().await;
    {
        let compact = session.compact(None);
        tokio::pin!(compact);
        std::future::poll_fn(|context| {
            assert!(std::future::Future::poll(compact.as_mut(), context).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert!(session.is_compacting());
    }
    assert!(!session.is_compacting());
    assert!(!session.work_barrier.has_active_work());
    drop(admission);
}

#[tokio::test]
async fn pending_manual_compaction_is_visible_and_abortable_before_admission() {
    let session = test_session();
    let admission = session.prompt_admission.lock().await;
    let compact = session.compact(None);
    tokio::pin!(compact);
    std::future::poll_fn(|context| {
        assert!(std::future::Future::poll(compact.as_mut(), context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert!(session.is_compacting());
    session.abort_compaction();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), compact)
            .await
            .expect("cancellation without lock release")
            .expect_err("cancelled admission"),
        "Compaction cancelled"
    );
    drop(admission);
    assert!(!session.is_compacting());
    assert_eq!(session.compaction_state().generation(), 0);
    assert!(!session.work_barrier.has_active_work());
}

#[tokio::test]
async fn precomputed_compaction_reuses_feedback_id_and_reports_rejection() {
    use maho_ext_api::ExtensionContextActions;
    for reject in [false, true] {
        let session = test_session();
        session.agent.set_model(test_model());
        let retained = session.with_session_manager_mut(|manager| {
            manager
                .append_message(serde_json::json!({"role":"user","content":"recent","timestamp":0}))
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if matches!(
                event,
                AgentSessionEvent::CompactionStart { .. } | AgentSessionEvent::CompactionEnd { .. }
            ) {
                lock(&captured).push(event.clone());
            }
        }));
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        let signal = actions
            .begin_compaction(maho_ext_api::BeginCompactionOptions {
                reason: maho_ext_api::CompactionReason::Extension,
            })
            .expect("feedback");
        let applied = actions
            .apply_compaction(
                maho_ext_api::CompactionResult {
                    summary: if reject {
                        "x".repeat(600_000)
                    } else {
                        "digest".to_owned()
                    },
                    first_kept_entry_id: retained["id"].as_str().expect("retained").to_owned(),
                    tokens_before: 100,
                    details: None,
                },
                maho_ext_api::ApplyCompactionOptions {
                    reason: maho_ext_api::CompactionReason::Extension,
                    expected_revision: Some(session.message_revision()),
                    expected_warm_anchor: None,
                    signal: Some(signal),
                },
            )
            .await
            .expect("application result");
        assert_eq!(
            applied,
            if reject {
                maho_ext_api::ApplyCompactionResult::Rejected
            } else {
                maho_ext_api::ApplyCompactionResult::Applied
            }
        );
        assert!(!session.is_compacting());
        assert_eq!(session.compaction_state().generation(), 1);
        assert_eq!(
            session.compaction_state().status(),
            if reject { "failed" } else { "completed" }
        );
        let events = lock(&events);
        assert_eq!(events.len(), 2);
        let AgentSessionEvent::CompactionStart { request_id, .. } = &events[0] else {
            panic!("start");
        };
        assert!(
            matches!(&events[1], AgentSessionEvent::CompactionEnd { request_id: end, accepted: Some(accepted), error_message, .. }
            if end == request_id && *accepted != reject && error_message.is_some() == reject)
        );
        assert_eq!(
            session
                .with_session_manager(|manager| conversation_entries(manager.entries()))
                .len(),
            if reject { 1 } else { 2 }
        );
    }
}

#[tokio::test]
async fn precomputed_extension_compaction_preserves_provenance_and_rejects_stale_revision() {
    use maho_ext_api::ExtensionContextActions;
    let session = test_session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(
            event,
            AgentSessionEvent::CompactionStart { .. } | AgentSessionEvent::CompactionEnd { .. }
        ) {
            lock(&captured).push(event.clone());
        }
    }));
    session.agent.set_model(test_model());
    let retained = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"recent","timestamp":0}))
    });
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let result = maho_ext_api::CompactionResult {
        summary: "digest".to_owned(),
        first_kept_entry_id: retained["id"].as_str().expect("retained").to_owned(),
        tokens_before: 100,
        details: None,
    };
    let options = |revision| maho_ext_api::ApplyCompactionOptions {
        reason: maho_ext_api::CompactionReason::Extension,
        expected_revision: Some(revision),
        expected_warm_anchor: None,
        signal: None,
    };
    assert_eq!(
        actions
            .apply_compaction(result.clone(), options(session.message_revision() + 1))
            .await
            .expect("stale"),
        maho_ext_api::ApplyCompactionResult::Stale
    );
    assert_eq!(
        session
            .with_session_manager(|manager| manager.entries())
            .len(),
        1
    );
    assert!(lock(&events).is_empty());
    assert_eq!(
        actions
            .apply_compaction(result, options(session.message_revision()))
            .await
            .expect("applied"),
        maho_ext_api::ApplyCompactionResult::Applied
    );
    assert_eq!(
        session.with_session_manager(|manager| {
            manager.entries().last().expect("compaction")["fromHook"].clone()
        }),
        true
    );
    assert_eq!(session.compaction_state().status(), "completed");
    let events = lock(&events);
    let AgentSessionEvent::CompactionStart { request_id, .. } = &events[0] else {
        panic!("start");
    };
    assert!(
        matches!(&events[1], AgentSessionEvent::CompactionEnd { request_id: completed, accepted: Some(true), .. } if completed == request_id)
    );
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn extension_applied_compaction_dispatches_session_compact() {
    let session = test_session();
    session.agent.set_model(test_model());
    let retained = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"recent","timestamp":0}))
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:apply-compact>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionCompact,
        vec![Arc::new(move |event, _| {
            if let maho_ext_api::ExtensionEvent::SessionCompact(
                maho_ext_api::SessionCompactEvent::Accepted { from_extension, .. },
            ) = event
            {
                lock(&captured).push(*from_extension);
            }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
    let result = maho_ext_api::CompactionResult {
        summary: "digest".to_owned(),
        first_kept_entry_id: retained["id"].as_str().expect("retained").to_owned(),
        tokens_before: 100,
        details: None,
    };
    assert_eq!(
        maho_ext_api::ExtensionContextActions::apply_compaction(
            &actions,
            result,
            maho_ext_api::ApplyCompactionOptions {
                reason: maho_ext_api::CompactionReason::Extension,
                expected_revision: Some(session.message_revision()),
                expected_warm_anchor: None,
                signal: None,
            },
        )
        .await
        .expect("applied"),
        maho_ext_api::ApplyCompactionResult::Applied
    );
    assert_eq!(*lock(&seen), [true]);
}

#[tokio::test]
async fn superseded_pending_compaction_cannot_release_the_new_admission() {
    let session = test_session();
    let admission = session.prompt_admission.lock().await;
    let first = session.compact(None);
    tokio::pin!(first);
    std::future::poll_fn(|context| {
        assert!(std::future::Future::poll(first.as_mut(), context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    let second = session.compact(None);
    tokio::pin!(second);
    std::future::poll_fn(|context| {
        assert!(std::future::Future::poll(second.as_mut(), context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), first)
            .await
            .expect("superseded claimant")
            .expect_err("superseded"),
        "Compaction cancelled"
    );
    assert!(session.is_compacting());
    assert!(session.work_barrier.has_active_work());
    session.abort_compaction();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), second)
            .await
            .expect("current claimant")
            .expect_err("aborted"),
        "Compaction cancelled"
    );
    drop(admission);
    assert!(!session.is_compacting());
    assert!(!session.work_barrier.has_active_work());
}

#[tokio::test]
async fn before_switch_handler_can_read_inventory_and_cancel_replacement() {
    let session = test_session();
    let captured = Arc::downgrade(&session.inner);
    let observed = Arc::new(AtomicU64::new(0));
    let calls = observed.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:before-switch-inventory>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeSwitch,
        vec![Arc::new(move |_, _| {
            let session = AgentSession {
                inner: captured.upgrade().expect("live session"),
            };
            let calls = calls.clone();
            Box::pin(async move {
                assert!(
                    session
                        .has_extension_handlers(maho_ext_api::EventKind::SessionBeforeSwitch)
                        .await
                );
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        cancel: Some(true),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let original_id = session.session_id();
    let result =
        tokio::time::timeout(std::time::Duration::from_secs(1), session.new_session(None)).await;
    let current_id = session.session_id();
    session.dispose().await;
    assert!(
        !result
            .expect("bounded reentrant before-switch handler")
            .expect("replacement disposition")
    );
    assert_eq!(current_id, original_id);
    assert_eq!(observed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn shutdown_handler_can_read_live_extension_inventory_without_reentrant_lock() {
    let session = test_session();
    let captured = Arc::downgrade(&session.inner);
    let observed = Arc::new(AtomicU64::new(0));
    let calls = observed.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:shutdown-inventory>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionShutdown,
        vec![Arc::new(move |_, _| {
            let session = AgentSession {
                inner: captured.upgrade().expect("live session"),
            };
            let calls = calls.clone();
            Box::pin(async move {
                assert!(
                    session
                        .has_extension_handlers(maho_ext_api::EventKind::SessionShutdown)
                        .await
                );
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(maho_ext_api::EventResult::None)
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        session.emit_session_shutdown(maho_ext_api::SessionReason::Resume),
    )
    .await;
    session.dispose().await;
    result.expect("bounded reentrant shutdown handler");
    assert_eq!(observed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn runtime_shutdown_carries_replacement_target_and_live_signal() {
    let session = test_session();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:replacement-shutdown>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionShutdown,
        vec![Arc::new(move |event, _| {
            let maho_ext_api::ExtensionEvent::SessionShutdown(event) = event else {
                panic!("shutdown");
            };
            assert_eq!(event.reason, maho_ext_api::SessionReason::Resume);
            assert!(!event.signal.as_ref().expect("shutdown signal").is_aborted());
            lock(&captured).push(event.target_session_file.clone());
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    session
        .runtime_shutdown_to(
            maho_ext_api::SessionReason::Resume,
            Some("target.jsonl".to_owned()),
        )
        .await;
    session
        .emit_session_shutdown(maho_ext_api::SessionReason::Resume)
        .await;
    assert_eq!(*lock(&observed), [Some("target.jsonl".to_owned()), None]);
}

#[tokio::test]
async fn title_failure_is_reported_as_runtime_extension_error() {
    let session = test_session_with_stream_function(false);
    session.agent.set_model(test_model());
    let errors = Arc::new(Mutex::new(Vec::new()));
    let observed = errors.clone();
    session.state().extension_error_listener =
        Some(Arc::new(move |error| lock(&observed).push(error.clone())));
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let _subscription =
        session.subscribe(Arc::new(move |event| lock(&observed).push(event.clone())));
    session
        .generate_session_title_if_needed("Inspect the workspace")
        .await;
    let errors = lock(&errors);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].extension_path, "<runtime>");
    assert_eq!(errors[0].event, "session_title_generation");
    assert!(!errors[0].error.is_empty());
    assert!(
        !lock(&events)
            .iter()
            .any(|event| matches!(event, AgentSessionEvent::ContinuationError { .. }))
    );
    assert!(session.state().session_title_abort_controller.is_none());
}

#[tokio::test]
async fn runner_errors_reach_the_bound_error_listener_once() {
    let session = test_session();
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            Vec::new(),
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let errors = Arc::new(Mutex::new(Vec::new()));
    let observed = errors.clone();
    session
        .bind_extensions(ExtensionBindings {
            on_error: Some(Arc::new(move |error: &ExtensionError| {
                lock(&observed).push(error.clone())
            })),
            ..Default::default()
        })
        .await;
    let error = ExtensionError {
        extension_path: "ext.rs".to_owned(),
        event: "tool_call".to_owned(),
        error: "boom".to_owned(),
        stack: None,
    };
    if let Some(runner) = session.extension_runner.lock().await.as_mut() {
        runner.emit_error(error.clone());
    }
    assert_eq!(
        lock(&errors)
            .iter()
            .map(|error| error.error.clone())
            .collect::<Vec<_>>(),
        ["boom".to_owned()]
    );
}

#[tokio::test]
async fn extension_compaction_rejection_has_no_execution_failure_event() {
    let session = test_session();
    let terminal = Arc::new(Mutex::new(Vec::new()));
    let observed = terminal.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(event, AgentSessionEvent::CompactionEnd { .. }) {
            lock(&observed).push(event.clone());
        }
    }));
    session.agent.set_model(test_model());
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"keepRecentTokens":1}),
        )]))
    });
    for text in ["old task", "recent task"] {
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0}))
        });
    }
    session.rebuild_session_context().expect("context");
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:compaction-rejection>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(|_, _| {
            Box::pin(async {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        cancel: Some(true),
                        rejection_cause: Some(
                            maho_ext_api::CompactionRejectionCause::CircuitBreaker,
                        ),
                        reason: Some("cooldown active".to_owned()),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    for kind in [
        maho_ext_api::EventKind::SessionCompact,
        maho_ext_api::EventKind::SessionCompactFailed,
    ] {
        let captured = events.clone();
        extension.handlers.insert(
            kind,
            vec![Arc::new(move |event, _| {
                lock(&captured).push(event.clone());
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            })],
        );
    }
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    assert!(session.compact(None).await.is_err());
    assert_eq!(session.compaction_state().status(), "failed");
    assert!(
        matches!(&lock(&terminal)[0], AgentSessionEvent::CompactionEnd {
        aborted: true, error_message: Some(message), accepted: Some(false), ..
    } if message == "Compaction rejected: cooldown active")
    );
    assert!(
        matches!(session.compaction_state(), crate::compaction::lifecycle::CompactionLifecycleState::Failed(_, _, Some(cause), _)
        if cause == "circuit-breaker")
    );
    let events = lock(&events);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Rejected {
            rejection_cause: maho_ext_api::CompactionRejectionCause::CircuitBreaker,
            ..
        })
    ));
}

#[tokio::test]
async fn provider_owned_compaction_bypasses_native_final_admission() {
    let session = test_session();
    let mut model = test_model();
    model.context_window = 128;
    session.agent.set_model(model.clone());
    session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
        ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1,"reserveTokens":0,"reserveScalingEnabled":false})),
    ])));
    for text in ["old task", "recent task"] {
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0}))
        });
    }
    session.rebuild_session_context().expect("context");
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:provider-owner>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(|_, _| {
            Box::pin(async {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        cancel: Some(true),
                        rejection_cause: Some(
                            maho_ext_api::CompactionRejectionCause::ExternalOwner,
                        ),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    assert!(session.compact(None).await.is_err());
    assert!(session.is_compaction_delegated());
    let additions = [session_message_from_value(serde_json::json!({
        "role":"custom", "customType":"admission-aside", "content":"large input ".repeat(200), "display":true, "timestamp":0,
    })).expect("custom addition")];
    session
        .enforce_final_provider_admission(&additions)
        .await
        .expect("provider owns oversized admission");
    model.id = "other-model".to_owned();
    session.agent.set_model(model);
    assert!(!session.is_compaction_delegated());
    assert!(
        session
            .enforce_final_provider_admission(&additions)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn late_queued_input_requires_final_admission_without_consuming_queue() {
    let session = test_session();
    let mut model = test_model();
    model.context_window = 128;
    session.agent.set_model(model);
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false}),
        )]))
    });
    let additions = [make_user_message("small prompt", None)];
    session
        .enforce_final_provider_admission(&additions)
        .await
        .expect("user-only admission");
    session
        .steer(&"late steering ".repeat(200), None, Default::default())
        .await
        .expect("steer");
    session
        .follow_up("late followup", None, Default::default())
        .await
        .expect("followup");
    assert!(
        session
            .enforce_final_provider_admission(&additions)
            .await
            .is_err()
    );
    assert!(session.messages().is_empty());
    assert!(
        session
            .with_session_manager(|manager| conversation_entries(manager.entries()))
            .is_empty()
    );
    let cleared = session.clear_queue(false);
    assert_eq!(cleared.steering, ["late steering ".repeat(200)]);
    assert_eq!(cleared.follow_up, ["late followup"]);
}

#[tokio::test]
async fn footer_reads_on_a_compacted_session_do_not_materialize_the_history() {
    let session = test_session();
    session.agent.set_model(test_model());
    session.with_session_manager_mut(|manager| {
        let root = manager
            .append_message(serde_json::json!({"role":"user","content":"first","timestamp":0}));
        manager.append_message(
            serde_json::json!({"role":"user","content":"x".repeat(64 * 1024),"timestamp":1}),
        );
        manager.append_compaction(
            "digest",
            root["id"].as_str().expect("root"),
            1000,
            None,
            None,
            None,
        );
        manager.append_message(serde_json::json!({"role":"user","content":"after","timestamp":2}));
    });
    session.with_session_manager(|manager| manager.drop_materialized_cache());

    let stats = session.get_session_stats();

    assert_eq!(stats.user_messages, 3);
    assert!(stats.context_usage.is_some());
    assert!(!session.with_session_manager(|manager| manager.has_materialized_view()));
}

#[tokio::test]
async fn context_usage_ignores_compaction_on_an_abandoned_branch() {
    let session = test_session();
    session.agent.set_model(test_model());
    let mut assistant =
        maho_ai::providers::faux::faux_assistant_message("answer", Default::default());
    assistant.usage.input = 1000;
    let selected = session.with_session_manager_mut(|manager| {
        let root = manager
            .append_message(serde_json::json!({"role":"user","content":"small","timestamp":0}));
        let selected = manager.append_message(serde_json::to_value(&assistant).expect("assistant"));
        manager.append_compaction(
            "abandoned digest",
            root["id"].as_str().expect("root"),
            1000,
            None,
            None,
            None,
        );
        selected
    });
    session
        .with_session_manager_mut(|manager| manager.set_leaf(selected["id"].as_str()))
        .expect("leaf");
    session.rebuild_session_context().expect("selected context");
    assert_eq!(
        session.get_context_usage().expect("usage").tokens,
        Some(1000)
    );
}

#[tokio::test]
async fn continuation_uses_post_compaction_usage_by_persistence_order() {
    for post_boundary in [false, true] {
        let session = test_session();
        let mut model = test_model();
        model.context_window = 128;
        session.agent.set_model(model);
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false,"keepRecentTokens":1})),
        ])));
        let mut assistant =
            maho_ai::providers::faux::faux_assistant_message("answer", Default::default());
        assistant.usage.input = 1000;
        assistant.timestamp = 0;
        session.with_session_manager_mut(|manager| {
            let kept = manager
                .append_message(serde_json::json!({"role":"user","content":"small","timestamp":0}));
            manager.append_message(serde_json::to_value(&assistant).expect("assistant"));
            manager.append_compaction(
                "digest",
                kept["id"].as_str().expect("kept"),
                1000,
                None,
                None,
                None,
            );
            if post_boundary {
                manager.append_message(serde_json::to_value(&assistant).expect("assistant"));
            }
        });
        session.rebuild_session_context().expect("context");
        let mut extension = maho_ext_api::LoadedExtension::new(
            "<inline:usage-admission>",
            session.cwd().into(),
            Default::default(),
        );
        extension.handlers.insert(
            maho_ext_api::EventKind::SessionBeforeCompact,
            vec![Arc::new(|_, _| {
                Box::pin(async {
                    Ok(maho_ext_api::EventResult::SessionBefore(
                        maho_ext_api::SessionBeforeEventResult {
                            cancel: Some(true),
                            ..Default::default()
                        },
                    ))
                })
            })],
        );
        session
            .set_extension_runner(maho_ext_host::ExtensionRunner::new(
                vec![extension],
                Default::default(),
                Default::default(),
                test_extension_context(&session),
            ))
            .await;
        assert_eq!(
            session
                .revalidate_scheduled_continuation_admission()
                .await
                .is_err(),
            post_boundary
        );
    }
}

#[tokio::test]
async fn recompact_continuation_excludes_failed_assistant_from_provider_context() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "resumed",
            Default::default(),
        )],
        0,
    );
    let mut model = session.model();
    model.context_window = 128_000;
    session.agent.set_model(model);
    session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
        ("compaction".to_owned(), serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false,"keepRecentTokens":1})),
    ])));
    for text in ["old task".repeat(18_000), "recent task".repeat(18_000)] {
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0}))
        });
    }
    let mut failed =
        maho_ai::providers::faux::faux_assistant_message("failed tail", Default::default());
    failed.stop_reason = StopReason::Error;
    failed.error_message = Some("failed request".to_owned());
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::to_value(failed).expect("failed assistant"))
    });
    session.rebuild_session_context().expect("context");
    session.agent.follow_up(
        session_message_from_value(
            serde_json::json!({"role":"user","content":"queued","timestamp":0}),
        )
        .expect("queued"),
    );
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:retire-failed-tail>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("preparation");
            };
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(maho_ext_api::CompactionResult {
                            summary: "digest".to_owned(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                            tokens_before: event.preparation.tokens_before,
                            details: None,
                        }),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    session
        .continue_session()
        .await
        .expect("recompacted continuation");
    assert_eq!(
        session
            .messages()
            .iter()
            .filter(|message| message.role() == "assistant")
            .count(),
        1
    );
    assert!(
        session
            .with_session_manager(|manager| manager.entries())
            .iter()
            .any(|entry| entry["message"]["errorMessage"] == "failed request")
    );
}

#[tokio::test]
async fn continuation_admission_only_hard_failure_blocks() {
    for (hard_limit, reject, cooldown) in [
        (false, false, false),
        (false, true, false),
        (true, true, false),
        (true, true, true),
    ] {
        let session = test_session();
        let mut model = test_model();
        model.context_window = 128;
        session.agent.set_model(model);
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false,"keepRecentTokens":1})),
        ])));
        for text in ["old task", "recent task"] {
            let text = text.repeat(if hard_limit { 800 } else { 13 });
            session.with_session_manager_mut(|manager| {
                manager
                    .append_message(serde_json::json!({"role":"user","content":text,"timestamp":0}))
            });
        }
        session.rebuild_session_context().expect("context");
        let mut extension = maho_ext_api::LoadedExtension::new(
            "<inline:continuation-admission>",
            session.cwd().into(),
            Default::default(),
        );
        extension.handlers.insert(
            maho_ext_api::EventKind::SessionBeforeCompact,
            vec![Arc::new(move |event, _| {
                let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                    panic!("preparation");
                };
                assert_eq!(event.reason, maho_ext_api::CompactionReason::PrePrompt);
                let result = maho_ext_api::CompactionResult {
                    summary: "digest".to_owned(),
                    first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                    tokens_before: event.preparation.tokens_before,
                    details: None,
                };
                Box::pin(async move {
                    Ok(maho_ext_api::EventResult::SessionBefore(
                        maho_ext_api::SessionBeforeEventResult {
                            cancel: reject.then_some(true),
                            rejection_cause: cooldown
                                .then_some(maho_ext_api::CompactionRejectionCause::CircuitBreaker),
                            compaction: (!reject).then_some(result),
                            ..Default::default()
                        },
                    ))
                })
            })],
        );
        session
            .set_extension_runner(maho_ext_host::ExtensionRunner::new(
                vec![extension],
                Default::default(),
                Default::default(),
                test_extension_context(&session),
            ))
            .await;
        let result = session.revalidate_scheduled_continuation_admission().await;
        assert_eq!(result.is_err(), hard_limit && reject && !cooldown);
        assert_eq!(session.compaction_state().generation(), 1);
    }
}

#[tokio::test]
async fn non_triggering_custom_message_waits_for_streaming_turn_end() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "finished",
            Default::default(),
        )],
        0,
    );
    let captured = session.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:quiet-custom>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::MessageStart,
        vec![Arc::new(move |event, _| {
            if let maho_ext_api::ExtensionEvent::MessageStart { message } = event
                && message.role() == "user"
            {
                assert!(captured.is_streaming());
                maho_ext_api::ExtensionActions::send_message(
                    &SessionExtensionActions(Arc::downgrade(&captured.inner)),
                    maho_ext_api::CustomMessage {
                        custom_type: "quiet-notice".to_owned(),
                        content: vec![maho_tools::definition::ToolContent::text("notice")],
                        display: true,
                        details: None,
                    },
                    maho_ext_api::SendMessageOptions {
                        trigger_turn: false,
                        deliver_as: None,
                    },
                )
                .expect("non-triggering message");
                assert!(!captured.agent.has_queued_messages());
                assert!(
                    !captured
                        .messages()
                        .iter()
                        .any(|message| message.role() == "custom")
                );
            }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("work", Default::default()),
    )
    .await
    .expect("bounded prompt")
    .expect("prompt");
    assert_eq!(
        session
            .messages()
            .iter()
            .filter(|message| message.role() == "assistant")
            .count(),
        1
    );
    assert_eq!(session.messages().last().expect("notice").role(), "custom");
    assert_eq!(
        session
            .with_session_manager(|manager| manager.entries())
            .iter()
            .filter(|entry| entry["customType"] == "quiet-notice")
            .count(),
        1
    );
    assert!(session.state().pending_custom_messages.is_empty());
}

#[tokio::test]
async fn compaction_preserves_only_its_matching_hook_diagnostic() {
    for (matching_request, replace_prefix) in [(true, false), (false, false), (true, true)] {
        let session = test_session();
        session.agent.set_model(test_model());
        session.with_settings_manager_mut(|manager| {
            manager.apply_overrides(&Map::from_iter([(
                "compaction".to_owned(),
                serde_json::json!({"keepRecentTokens":1}),
            )]))
        });
        for text in ["old task", "recent task"] {
            session.with_session_manager_mut(|manager| {
                manager
                    .append_message(serde_json::json!({"role":"user","content":text,"timestamp":0}))
            });
        }
        session.rebuild_session_context().expect("context");
        let captured = session.clone();
        let mut extension = maho_ext_api::LoadedExtension::new(
            "<inline:compaction-diagnostic>",
            session.cwd().into(),
            Default::default(),
        );
        extension.handlers.insert(
            maho_ext_api::EventKind::SessionBeforeCompact,
            vec![Arc::new(move |event, _| {
                let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                    panic!("preparation");
                };
                maho_ext_api::ExtensionActions::send_message(
                    &SessionExtensionActions(Arc::downgrade(&captured.inner)),
                    maho_ext_api::CustomMessage {
                        custom_type: "senpi.hook".to_owned(),
                        content: vec![maho_tools::definition::ToolContent::text("diagnostic")],
                        display: true,
                        details: Some(
                            serde_json::json!({"event":"PreCompact", "compactionRequestId":
                    if matching_request { event.request_id.as_str() } else { "stale-request" }}),
                        ),
                    },
                    maho_ext_api::SendMessageOptions {
                        trigger_turn: false,
                        deliver_as: None,
                    },
                )
                .expect("diagnostic");
                if replace_prefix {
                    let mut messages = captured.messages();
                    messages[0] = make_user_message("revision-neutral replacement", None);
                    captured.agent.set_messages(messages);
                }
                let result = maho_ext_api::CompactionResult {
                    summary: "digest".to_owned(),
                    first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                    tokens_before: event.preparation.tokens_before,
                    details: None,
                };
                Box::pin(async move {
                    Ok(maho_ext_api::EventResult::SessionBefore(
                        maho_ext_api::SessionBeforeEventResult {
                            compaction: Some(result),
                            ..Default::default()
                        },
                    ))
                })
            })],
        );
        session
            .set_extension_runner(maho_ext_host::ExtensionRunner::new(
                vec![extension],
                Default::default(),
                Default::default(),
                test_extension_context(&session),
            ))
            .await;
        let result = session.compact(None).await;
        assert_eq!(result.is_ok(), matching_request && !replace_prefix);
        assert_eq!(
            session
                .messages()
                .iter()
                .filter(|message| message.role() == "custom")
                .count(),
            1
        );
        let entries = session.with_session_manager(|manager| manager.entries());
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry["customType"] == "senpi.hook")
                .count(),
            1
        );
        assert_eq!(
            entries.iter().any(|entry| entry["type"] == "compaction"),
            matching_request && !replace_prefix
        );
    }
}

#[tokio::test]
async fn custom_trigger_during_compaction_is_queued_then_delivered_once() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "resumed",
            Default::default(),
        )],
        0,
    );
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"keepRecentTokens":1}),
        )]))
    });
    for text in ["old task", "recent task"] {
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0}))
        });
    }
    session.rebuild_session_context().expect("context");
    let captured = session.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:compaction-trigger>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(move |event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("preparation");
            };
            let revision = captured.message_revision();
            maho_ext_api::ExtensionActions::send_message(
                &SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::CustomMessage {
                    custom_type: "after-compaction".to_owned(),
                    content: vec![maho_tools::definition::ToolContent::text("continue")],
                    display: true,
                    details: None,
                },
                maho_ext_api::SendMessageOptions {
                    trigger_turn: true,
                    deliver_as: None,
                },
            )
            .expect("queue trigger");
            assert_eq!(captured.message_revision(), revision);
            assert!(captured.agent.has_queued_messages());
            let result = maho_ext_api::CompactionResult {
                summary: "digest".to_owned(),
                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                tokens_before: event.preparation.tokens_before,
                details: None,
            };
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(result),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let (finished, received) = tokio::sync::oneshot::channel();
    let finished = Arc::new(Mutex::new(Some(finished)));
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(event, AgentSessionEvent::AgentIdle)
            && let Some(finished) = lock(&finished).take()
        {
            finished.send(()).expect("observer");
        }
    }));
    session.compact(None).await.expect("compact");
    tokio::time::timeout(std::time::Duration::from_secs(5), received)
        .await
        .expect("bounded continuation")
        .expect("settled");
    let entries = session.with_session_manager(|manager| manager.entries());
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry["customType"] == "after-compaction")
            .count(),
        1
    );
    assert_eq!(
        session
            .messages()
            .iter()
            .filter(|message| message.role() == "assistant")
            .count(),
        1
    );
    assert!(!session.agent.has_queued_messages());
}

#[test]
fn idle_custom_message_persists_before_paired_message_events() {
    let session = test_session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = session.clone();
    let observed = events.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::Agent(event) = event {
            match event {
                maho_agent::types::AgentEvent::MessageStart { message, .. }
                | maho_agent::types::AgentEvent::MessageEnd { message } => {
                    assert_eq!(message.role(), "custom");
                    assert_eq!(
                        captured.messages().as_slice(),
                        std::slice::from_ref(message)
                    );
                    assert_eq!(
                        captured.with_session_manager(|manager| manager.entries())[0]["customType"],
                        "idle-notice"
                    );
                    lock(&observed).push(matches!(
                        event,
                        maho_agent::types::AgentEvent::MessageEnd { .. }
                    ));
                }
                _ => {}
            }
        }
    }));
    maho_ext_api::ExtensionActions::send_message(
        &SessionExtensionActions(Arc::downgrade(&session.inner)),
        maho_ext_api::CustomMessage {
            custom_type: "idle-notice".to_owned(),
            content: vec![maho_tools::definition::ToolContent::text("notice")],
            display: true,
            details: None,
        },
        maho_ext_api::SendMessageOptions {
            trigger_turn: false,
            deliver_as: None,
        },
    )
    .expect("append");
    assert_eq!(*lock(&events), [false, true]);
    assert_eq!(
        session
            .with_session_manager(|manager| manager.entries())
            .len(),
        1
    );
}

#[tokio::test]
async fn compaction_success_event_precedes_completed_extension_hook() {
    let session = test_session();
    session.agent.set_model(test_model());
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"keepRecentTokens":1}),
        )]))
    });
    for text in ["old task", "recent task"] {
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0}))
        });
    }
    session.rebuild_session_context().expect("context");
    let order = Arc::new(Mutex::new(Vec::new()));
    let events = order.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(
            event,
            AgentSessionEvent::CompactionEnd {
                accepted: Some(true),
                ..
            }
        ) {
            lock(&events).push("end");
        }
    }));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:compaction-result>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("preparation");
            };
            let result = maho_ext_api::CompactionResult {
                summary: "digest".to_owned(),
                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                tokens_before: event.preparation.tokens_before,
                details: None,
            };
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(result),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    let hooks = order.clone();
    let captured = session.clone();
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionCompact,
        vec![Arc::new(move |event, _| {
            assert!(matches!(
                event,
                maho_ext_api::ExtensionEvent::SessionCompact(
                    maho_ext_api::SessionCompactEvent::Accepted {
                        from_extension: true,
                        ..
                    }
                )
            ));
            assert_eq!(captured.compaction_state().status(), "completed");
            assert!(!captured.is_compacting());
            lock(&hooks).push("hook");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "retry".to_owned(),
            serde_json::json!({"revertPolicy":"never"}),
        )]))
    });
    session
        .retry_fallback
        .lock()
        .await
        .as_mut()
        .expect("controller")
        .state = Some(crate::retry_fallback::controller::ActiveFallbackState {
        chain_key: "fixture".to_owned(),
        original_selector: "faux/primary".to_owned(),
        original_thinking_level: None,
        last_applied_thinking_level: None,
        pinned_by_refusal: true,
        pinned_by_billing: false,
        pinned: true,
    });
    session.compact(None).await.expect("compaction");
    assert_eq!(*lock(&order), ["end", "hook"]);
    assert_eq!(
        session.with_session_manager(|manager| {
            manager.entries().last().expect("compaction")["fromHook"].clone()
        }),
        true
    );
    let guard = session.retry_fallback.lock().await;
    let fallback = guard
        .as_ref()
        .expect("controller")
        .state
        .as_ref()
        .expect("fallback remains for never policy");
    assert!(!fallback.pinned_by_refusal);
    assert!(!fallback.pinned);
}

#[tokio::test]
async fn auto_retry_creates_fresh_invoke_recovery_wrapper() {
    use maho_ai::types::{AssistantMessageEvent as Event, DoneReason, ErrorReason};
    let session = retry_session(Vec::new(), 2);
    let executed = Arc::new(Mutex::new(Vec::new()));
    let captured = executed.clone();
    let mut tool = test_tool("Echo");
    tool.tool.parameters = serde_json::json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"]});
    tool.execute = Arc::new(move |_, arguments, _, _| {
        lock(&captured).push(arguments["value"].as_str().expect("argument").to_owned());
        Box::pin(async { AgentToolResult::text("echoed") })
    });
    session.agent.set_tools(vec![tool.clone()]);
    let calls = Arc::new(AtomicU64::new(0));
    let stream_calls = calls.clone();
    session.agent.set_stream_function(Arc::new(move |_, _, _| {
        let call = stream_calls.fetch_add(1, Ordering::SeqCst);
        let text = match call {
            0 => "<invoke na", 1 => "<invoke name=\"Ec",
            2 => "<invoke name=\"Echo\"><parameter name=\"value\">second-attempt</parameter></invoke>",
            _ => "Final",
        };
        let failed = call < 2;
        let mut message = maho_ai::providers::faux::faux_assistant_message(text,
            maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(if failed { StopReason::Error } else { StopReason::Stop }),
                error_message: failed.then(|| "overloaded_error".to_owned()), ..Default::default()
            });
        let inner = maho_ai::utils::event_stream::create_assistant_message_event_stream();
        let stream = maho_ai::tool_call_middleware::recovery_stream_wrapper::wrap_stream_with_invoke_recovery(
            inner.clone(), vec![tool.tool.clone()], Default::default());
        let mut partial = message.clone();
        partial.content.clear();
        inner.push(Event::Start { partial });
        inner.push(Event::TextStart { content_index: 0, partial: message.clone() });
        inner.push(Event::TextDelta { content_index: 0, delta: text.to_owned(), partial: message.clone() });
        if failed {
            inner.push(Event::Error { reason: ErrorReason::Error, error: message });
        } else {
            inner.push(Event::TextEnd { content_index: 0, content: text.to_owned(), partial: message.clone() });
            message.stop_reason = StopReason::Stop;
            inner.push(Event::Done { reason: DoneReason::Stop, message });
        }
        stream
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("Test", Default::default()),
    )
    .await
    .expect("bounded prompt")
    .expect("prompt");
    assert_eq!(*lock(&executed), vec!["second-attempt"]);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn prompt_retries_transient_failure_and_preserves_durable_history() {
    let session = retry_session(
        vec![
            maho_ai::providers::faux::faux_assistant_message(
                "",
                maho_ai::providers::faux::FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Error),
                    error_message: Some("overloaded_error".to_owned()),
                    ..Default::default()
                },
            ),
            maho_ai::providers::faux::faux_assistant_message("recovered", Default::default()),
        ],
        2,
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription =
        session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("input", Default::default()),
    )
    .await
    .expect("bounded prompt")
    .expect("prompt");
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some("recovered")
    );
    assert!(!session.is_retrying());
    assert_eq!(session.messages().len(), 2);
    assert_eq!(
        session.with_session_manager(|manager| conversation_entries(manager.entries()).len()),
        3
    );
    assert!(lock(&events).iter().any(|event| matches!(
        event,
        AgentSessionEvent::AutoRetryEnd {
            success: true,
            attempt: 1,
            ..
        }
    )));
}

#[tokio::test]
async fn rate_limit_body_hint_drives_tier_delay_without_marker() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "",
            maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("rate_limit_error: retry in 1 s".to_owned()),
                ..Default::default()
            },
        )],
        2,
    );
    let observed = Arc::new(Mutex::new(None));
    let captured = observed.clone();
    let cancelling = session.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::AutoRetryStart { delay_ms, .. } = event {
            *lock(&captured) = Some(*delay_ms);
            cancelling.abort_retry();
        }
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("input", Default::default()),
    )
    .await
    .expect("bounded prompt")
    .expect("prompt");
    assert_eq!(*lock(&observed), Some(500));
}

#[tokio::test]
async fn settled_handler_followup_owns_work_until_second_turn_is_idle() {
    let session = retry_session(
        vec![
            maho_ai::providers::faux::faux_assistant_message("first", Default::default()),
            maho_ai::providers::faux::faux_assistant_message("second", Default::default()),
        ],
        0,
    );
    let once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let followup = session.clone();
    let (idle_tx, idle_rx) = tokio::sync::oneshot::channel();
    let idle_tx = Arc::new(Mutex::new(Some(idle_tx)));
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(event, AgentSessionEvent::AgentSettled) && !once.swap(true, Ordering::SeqCst) {
            maho_ext_api::ExtensionActions::send_user_message(
                &SessionExtensionActions(Arc::downgrade(&followup.inner)),
                maho_ext_api::UserMessageContent::Text("followup".to_owned()),
                Default::default(),
            )
            .expect("deferred followup");
        }
        if matches!(event, AgentSessionEvent::AgentIdle)
            && let Some(sender) = lock(&idle_tx).take()
        {
            let _ = sender.send(());
        }
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("start", Default::default()),
    )
    .await
    .expect("bounded prompt")
    .expect("prompt");
    tokio::time::timeout(std::time::Duration::from_secs(5), idle_rx)
        .await
        .expect("bounded idle")
        .expect("idle");
    assert_eq!(session.messages().len(), 4);
    assert_eq!(session.get_last_assistant_text().as_deref(), Some("second"));
}

#[tokio::test]
async fn model_switch_engages_server_side_fallback_abort_only_with_configured_chain() {
    let session = retry_session(Vec::new(), 1);
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(
        maho_ai::providers::faux::faux_provider(Default::default())
            .expect("valid faux provider")
            .provider
            .clone(),
    );
    runtime.register_native_provider(
        maho_ai::providers::faux::faux_provider(
            maho_ai::providers::faux::RegisterFauxProviderOptions {
                provider: Some("anthropic-subscription".to_owned()),
                ..Default::default()
            },
        )
        .expect("valid faux provider")
        .provider
        .clone(),
    );
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("fallback provider");
    let mut model = test_model();
    model.provider = "anthropic-subscription".to_owned();
    session
        .set_session_model(test_model())
        .await
        .expect("unchained switch");
    assert_eq!(session.agent.abort_server_side_fallback(), Some(false));
    session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
        ("retry".to_owned(), serde_json::json!({"enabled":true,"fallbackChains":{"anthropic-subscription/faux-1":["faux/faux-1"]}})),
    ])));
    session
        .set_session_model(model.clone())
        .await
        .expect("chained switch");
    assert_eq!(session.agent.abort_server_side_fallback(), Some(true));
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "retry".to_owned(),
            serde_json::json!({"enabled":true,"abortServerSideFallback":false,
            "fallbackChains":{"anthropic-subscription/faux-1":["faux/faux-1"]}}),
        )]))
    });
    session
        .set_session_model(model)
        .await
        .expect("disabled switch");
    assert_eq!(session.agent.abort_server_side_fallback(), Some(false));
}

#[tokio::test]
async fn subscription_auth_miss_does_not_admit_configured_provider_fallback() {
    let session = retry_session(Vec::new(), 1);
    let provider =
        maho_ai::providers::faux::faux_provider(Default::default()).expect("valid faux provider");
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    let subscription = maho_ai::providers::faux::faux_provider(
        maho_ai::providers::faux::RegisterFauxProviderOptions {
            provider: Some("anthropic-subscription".to_owned()),
            ..Default::default()
        },
    )
    .expect("valid faux provider");
    runtime.register_native_provider(subscription.provider.clone());
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("fallback provider");
    let mut model = test_model();
    model.provider = "anthropic-subscription".to_owned();
    session.agent.set_model(model);
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "retry".to_owned(),
            serde_json::json!({"enabled":true,"modelFallback":true,"fallbackChains":{
                "anthropic-subscription/faux-1":["faux/faux-1"]
            }}),
        )]))
    });
    assert!(
        session
            .retry_fallback
            .lock()
            .await
            .as_mut()
            .expect("controller")
            .can_try_fallback()
    );
    let failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some(maho_ai::auth::resolve::provider_not_configured_message(
                "anthropic-subscription",
            )),
            ..Default::default()
        },
    );
    assert!(!session.will_retry(Some(&failed)).await);
}

#[tokio::test]
async fn quota_resource_exhaustion_admits_recovery_with_orphaned_tool_call() {
    let session = retry_session(Vec::new(), 1);
    session.agent.set_model(test_model());
    let mut failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("RESOURCE_EXHAUSTED".to_owned()),
            ..Default::default()
        },
    );
    failed.provider = "cursor".to_owned();
    failed.usage.input = 1000;
    failed.content.push(maho_ai::types::ContentBlock::ToolCall(
        maho_ai::types::ToolCall {
            id: "orphan".to_owned(),
            name: "read".to_owned(),
            arguments: Map::new(),
            thought_signature: None,
            incomplete: None,
            error_message: None,
            namespace: None,
        },
    ));
    // senpi `_willRetryAfterAgentEnd`: quota exhaustion only announces a retry when a fallback exists.
    assert!(!session.will_retry(Some(&failed)).await);
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"task"}],"timestamp":0}));
        manager.append_message(serde_json::to_value(&failed).expect("quota failure"));
    });
    session.rebuild_session_context().expect("context");
    session
        .follow_up("retained quota input", None, Default::default())
        .await
        .expect("queue");
    let retries = Arc::new(Mutex::new(0));
    let captured = retries.clone();
    let final_errors = Arc::new(Mutex::new(Vec::new()));
    let captured_errors = final_errors.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(event, AgentSessionEvent::AutoRetryStart { .. }) {
            *lock(&captured) += 1;
        }
        if let AgentSessionEvent::AutoRetryEnd {
            final_error: Some(error),
            ..
        } = event
        {
            lock(&captured_errors).push(error.clone());
        }
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.finish_provider_turn(),
    )
    .await
    .expect("bounded quota recovery")
    .expect("quota recovery");
    assert_eq!(*lock(&retries), 0);
    assert_eq!(
        *lock(&final_errors),
        vec![
            "RESOURCE_EXHAUSTED (likely provider usage/quota exhaustion: conversation is well below the model context window)"
        ]
    );
    assert_eq!(
        session.get_follow_up_messages(),
        vec!["retained quota input"]
    );
    assert!(session.agent.has_queued_messages());
    assert_eq!(session.messages().len(), 1);
    assert_eq!(
        session
            .with_session_manager(|manager| manager.entries())
            .iter()
            .filter(|entry| entry["type"] == "message")
            .count(),
        2
    );
}

#[tokio::test]
async fn subscription_remint_replays_and_exhausts_without_fallback() {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
    for recovered in [true, false] {
        let provider = faux_provider(RegisterFauxProviderOptions {
            provider: Some("anthropic-subscription".to_owned()),
            tokens_per_second: Some(0.0),
            ..Default::default()
        })
        .expect("valid faux provider");
        let failure = || {
            maho_ai::providers::faux::faux_assistant_message(
                "",
                maho_ai::providers::faux::FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Error),
                    error_message: Some("invalid_request".to_owned()),
                    ..Default::default()
                },
            )
        };
        provider.set_responses(vec![
            failure().into(),
            if recovered {
                maho_ai::providers::faux::faux_assistant_message("recovered", Default::default())
                    .into()
            } else {
                failure().into()
            },
        ]);
        let streams = faux_streams(provider.core.clone());
        let session = retry_session(Vec::new(), 1);
        // senpi test/suite/harness.ts: the faux provider is registered, so `prompt()`'s auth gate admits it.
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider.provider.clone());
        session
            .agent
            .set_model(provider.get_model(Some("faux-1")).expect("model"));
        session
            .agent
            .set_stream_function(Arc::new(move |model, context, options| {
                streams.stream_simple(model, context, options.map(|options| options.simple))
            }));
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session.prompt("task", Default::default()),
        )
        .await
        .expect("bounded prompt")
        .expect("prompt");
        let calls = provider.get_call_log();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|call| call.model_id == "faux-1"));
        assert_eq!(session.model().provider, "anthropic-subscription");
        assert!(!session.is_retrying());
        if recovered {
            assert_eq!(
                session.get_last_assistant_text().as_deref(),
                Some("recovered")
            );
        } else {
            assert_eq!(
                session
                    .messages()
                    .last()
                    .and_then(AgentMessage::as_assistant)
                    .and_then(|message| message.error_message.as_deref()),
                Some("invalid_request")
            );
        }
    }
}

#[tokio::test]
async fn subscription_session_errors_admit_same_model_recovery_only_on_subscription() {
    let session = retry_session(Vec::new(), 1);
    for error in ["Lock file is already being held", "invalid_request"] {
        let failed = maho_ai::providers::faux::faux_assistant_message(
            "",
            maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some(error.to_owned()),
                ..Default::default()
            },
        );
        let mut model = test_model();
        model.provider = "anthropic-subscription".to_owned();
        session.agent.set_model(model);
        assert!(
            session.will_retry(Some(&failed)).await,
            "subscription recovery: {error}"
        );
        assert!(session.is_subscription_same_model_remint_error(&failed));
        session.agent.set_model(test_model());
        assert!(
            !session.is_subscription_same_model_remint_error(&failed),
            "provider-scoped recovery: {error}"
        );
        assert_eq!(
            session.will_retry(Some(&failed)).await,
            maho_ai::utils::retry::is_retryable_assistant_error(&failed)
        );
    }
}

#[tokio::test]
async fn zero_token_resource_exhaustion_recovers_on_same_model() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "reminted",
            Default::default(),
        )],
        1,
    );
    let mut failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("RESOURCE_EXHAUSTED".to_owned()),
            ..Default::default()
        },
    );
    failed.provider = "cursor".to_owned();
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"task"}],"timestamp":0}));
        manager.append_message(serde_json::to_value(maho_ai::providers::faux::faux_assistant_message("prior", Default::default())).expect("prior"));
        manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"next task"}],"timestamp":1}));
        manager.append_message(serde_json::to_value(failed).expect("failed"));
    });
    session.rebuild_session_context().expect("context");
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"keepRecentTokens":1}),
        )]))
    });
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:payload-overflow>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("preparation");
            };
            assert_eq!(event.reason, maho_ext_api::CompactionReason::Overflow);
            let result = maho_ext_api::CompactionResult {
                summary: "digest".to_owned(),
                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                tokens_before: event.preparation.tokens_before,
                details: None,
            };
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(result),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let model = session.model();
    let overflow_starts = Arc::new(Mutex::new(0));
    let captured = overflow_starts.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(
            event,
            AgentSessionEvent::CompactionStart {
                reason: maho_ext_api::CompactionReason::Overflow,
                ..
            }
        ) {
            *lock(&captured) += 1;
        }
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.finish_provider_turn(),
    )
    .await
    .expect("bounded recovery")
    .expect("recovery");
    assert_eq!(
        *lock(&overflow_starts),
        1,
        "a zero-token payload resource_exhausted is overflow"
    );
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some("reminted")
    );
    assert_eq!(session.model().id, model.id);
    assert_eq!(session.model().provider, model.provider);
}

#[tokio::test]
async fn repeated_recoverable_length_stops_after_two_compact_and_retries() {
    let truncated = |text: &str| {
        maho_ai::providers::faux::faux_assistant_message(
            text,
            maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                ..Default::default()
            },
        )
    };
    let session = retry_session(
        vec![
            truncated(&"x".repeat(64)),
            truncated(&"y".repeat(64)),
            truncated(&"z".repeat(64)),
        ],
        0,
    );
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"keepRecentTokens":1,"reserveTokens":0}),
        )]))
    });
    session.with_session_manager_mut(|manager| {
        manager.append_message(
            serde_json::json!({"role":"user","content":"previous task","timestamp":0}),
        )
    });
    session.rebuild_session_context().expect("context");
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:length-recovery>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("preparation");
            };
            let result = maho_ext_api::CompactionResult {
                summary: "digest".to_owned(),
                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                tokens_before: event.preparation.tokens_before,
                details: None,
            };
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(result),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let ends = Arc::new(Mutex::new(Vec::new()));
    let captured = ends.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::CompactionEnd {
            reason: maho_ext_api::CompactionReason::Overflow,
            error_message,
            ..
        } = event
        {
            lock(&captured).push(error_message.clone());
        }
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt(&"x".repeat(5000), Default::default()),
    )
    .await
    .expect("bounded recovery")
    .expect("prompt");
    let ends = lock(&ends).clone();
    assert_eq!(ends.len(), 3);
    assert_eq!(ends[0], None);
    assert_eq!(ends[1], None);
    assert_eq!(ends[2].as_deref(), Some(OVERFLOW_RECOVERY_FAILED_MESSAGE));
}

#[tokio::test]
async fn truncate_agent_messages_keeps_only_the_last_user_turn() {
    let session = test_session();
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"first","timestamp":0}));
        manager.append_message(
            serde_json::to_value(maho_ai::providers::faux::faux_assistant_message(
                "one",
                Default::default(),
            ))
            .expect("assistant"),
        );
        manager.append_message(serde_json::json!({"role":"user","content":"second","timestamp":1}));
    });
    session.rebuild_session_context().expect("context");
    assert!(session.truncate_agent_messages_to_last_user_turn());
    assert_eq!(session.messages().len(), 1);
    assert_eq!(user_message_text(&session.messages()[0]), "second");
    assert!(
        !session.truncate_agent_messages_to_last_user_turn(),
        "a lone user turn is already minimal"
    );
}

#[tokio::test]
async fn required_threshold_compaction_precedes_retry_start() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "recovered",
            Default::default(),
        )],
        1,
    );
    session.agent.set_model(test_model());
    let mut prior = maho_ai::providers::faux::faux_assistant_message("prior", Default::default());
    prior.usage.input = 120_000;
    let failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("overloaded_error".to_owned()),
            ..Default::default()
        },
    );
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"task"}],"timestamp":0}));
        manager.append_message(serde_json::to_value(prior).expect("prior"));
        manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"next task"}],"timestamp":1}));
        manager.append_message(serde_json::to_value(failed).expect("failed"));
    });
    session.rebuild_session_context().expect("context");
    session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
        ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1,"reserveTokens":20000,"reserveScalingEnabled":false})),
    ])));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:retry-order>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("preparation");
            };
            let result = maho_ext_api::CompactionResult {
                summary: "digest".to_owned(),
                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                tokens_before: event.preparation.tokens_before,
                details: None,
            };
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(result),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let order = Arc::new(Mutex::new(Vec::new()));
    let captured = order.clone();
    let _subscription = session.subscribe(Arc::new(move |event| match event {
        AgentSessionEvent::CompactionEnd {
            accepted: Some(true),
            ..
        } => lock(&captured).push("compacted"),
        AgentSessionEvent::AutoRetryStart { .. } => lock(&captured).push("retry"),
        _ => {}
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.finish_provider_turn(),
    )
    .await
    .expect("bounded recovery")
    .expect("recovery");
    assert_eq!(*lock(&order), vec!["compacted", "retry"]);
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some("recovered")
    );
}

#[tokio::test]
async fn will_retry_reports_false_once_the_budget_or_delay_ceiling_is_spent_without_fallback() {
    let failed = |error: &str| {
        maho_ai::providers::faux::faux_assistant_message(
            "",
            maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some(error.to_owned()),
                ..Default::default()
            },
        )
    };
    assert!(
        !retry_session(Vec::new(), 0)
            .will_retry(Some(&failed("overloaded_error")))
            .await
    );
    let session = retry_session(Vec::new(), 1);
    assert!(session.will_retry(Some(&failed("overloaded_error"))).await);
    assert!(
        !session
            .will_retry(Some(&failed("overloaded_error (retry-after-ms: 120000)")))
            .await
    );
}

async fn terminal_retry_error(session: &AgentSession, error: &str) -> (u32, Option<String>) {
    let failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some(error.to_owned()),
            ..Default::default()
        },
    );
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"task"}],"timestamp":0}));
        manager.append_message(serde_json::to_value(failed).expect("failed"));
    });
    session.rebuild_session_context().expect("context");
    let ends = Arc::new(Mutex::new(Vec::new()));
    let captured = ends.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::AutoRetryEnd {
            success: false,
            attempt,
            final_error,
        } = event
        {
            lock(&captured).push((*attempt, final_error.clone()));
        }
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.finish_provider_turn(),
    )
    .await
    .expect("bounded retry")
    .expect("retry");
    let ends = lock(&ends);
    assert_eq!(ends.len(), 1);
    ends[0].clone()
}

#[tokio::test]
async fn exhausted_transport_drop_ends_with_user_facing_failure_text() {
    let (attempt, final_error) = terminal_retry_error(
        &retry_session(Vec::new(), 0),
        "WebSocket closed 1006 Connection ended",
    )
    .await;
    assert_eq!(attempt, 0);
    let final_error = final_error.expect("final error");
    assert!(
        final_error.contains("dropped before the reply finished"),
        "{final_error}"
    );
    assert!(
        final_error.contains("No fallback model is configured"),
        "{final_error}"
    );
}

#[tokio::test]
async fn provider_delay_above_max_retry_delay_fails_with_the_configured_maximum() {
    let (attempt, final_error) = terminal_retry_error(
        &retry_session(Vec::new(), 2),
        "overloaded_error (retry-after-ms: 120000)",
    )
    .await;
    assert_eq!(
        (attempt, final_error.as_deref()),
        (
            1,
            Some("Provider requested retry delay 120000ms, exceeding configured maximum 60000ms")
        )
    );
}

#[tokio::test]
async fn rejected_native_tool_search_retries_the_same_model_once() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "recovered",
            Default::default(),
        )],
        1,
    );
    let provider =
        maho_ai::providers::faux::faux_provider(Default::default()).expect("valid faux provider");
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    let subscription = maho_ai::providers::faux::faux_provider(
        maho_ai::providers::faux::RegisterFauxProviderOptions {
            provider: Some("anthropic-subscription".to_owned()),
            ..Default::default()
        },
    )
    .expect("valid faux provider");
    runtime.register_native_provider(subscription.provider.clone());
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("fallback provider");
    let mut model = test_model();
    model.provider = "anthropic-subscription".to_owned();
    session.agent.set_model(model);
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "retry".to_owned(),
            serde_json::json!({"enabled":true,"maxRetries":1,"baseDelayMs":0,"modelFallback":true,
            "fallbackChains":{"anthropic-subscription/faux-1":["faux/faux-1"]}}),
        )]))
    });
    let pending = Arc::new(Mutex::new(Some("native tool search rejected".to_owned())));
    let source = pending.clone();
    session.state().native_tool_search_injection_failure_source =
        Some(Arc::new(move || lock(&source).take()));
    let (attempt, delay_ms) = {
        let starts = Arc::new(Mutex::new(Vec::new()));
        let captured = starts.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::AutoRetryStart {
                attempt, delay_ms, ..
            } = event
            {
                lock(&captured).push((*attempt, *delay_ms));
            }
        }));
        let failed = maho_ai::providers::faux::faux_assistant_message(
            "",
            maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("400 bad request".to_owned()),
                ..Default::default()
            },
        );
        // senpi `buildSessionContext` restores the model of the last assistant entry, so the
        // rejected request must be recorded against the subscription model that produced it.
        let mut failed = failed;
        failed.provider = "anthropic-subscription".to_owned();
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"task"}],"timestamp":0}));
            manager.append_message(serde_json::to_value(failed).expect("failed"));
        });
        session.rebuild_session_context().expect("context");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session.finish_provider_turn(),
        )
        .await
        .expect("bounded recovery")
        .expect("recovery");
        let starts = lock(&starts);
        assert_eq!(starts.len(), 1);
        starts[0]
    };
    assert_eq!((attempt, delay_ms), (1, 0));
    assert!(lock(&pending).is_none());
    assert_eq!(session.model().provider, "anthropic-subscription");
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some("recovered")
    );
}

#[tokio::test]
async fn rejected_required_retry_compaction_retains_follow_up_queue() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "unexpected retry",
            Default::default(),
        )],
        1,
    );
    session.agent.set_model(test_model());
    let mut prior = maho_ai::providers::faux::faux_assistant_message("prior", Default::default());
    prior.usage.input = 120_000;
    let failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("overloaded_error".to_owned()),
            ..Default::default()
        },
    );
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"task"}],"timestamp":0}));
        manager.append_message(serde_json::to_value(prior).expect("prior"));
        manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"next task"}],"timestamp":1}));
        manager.append_message(serde_json::to_value(failed).expect("failed"));
    });
    session.rebuild_session_context().expect("context");
    session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
        ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1,"reserveTokens":20000,"reserveScalingEnabled":false})),
    ])));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:retry-rejection>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(|_, _| {
            Box::pin(async {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        cancel: Some(true),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    session
        .follow_up("retained input", None, Default::default())
        .await
        .expect("queue");
    let retries = Arc::new(Mutex::new(0));
    let captured = retries.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(event, AgentSessionEvent::AutoRetryStart { .. }) {
            *lock(&captured) += 1;
        }
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.finish_provider_turn(),
    )
    .await
    .expect("bounded recovery")
    .expect("recovery");
    assert_eq!(*lock(&retries), 0);
    assert_eq!(session.get_follow_up_messages(), vec!["retained input"]);
    assert!(session.agent.has_queued_messages());
    assert_eq!(session.get_last_assistant_text().as_deref(), Some("prior"));
    assert_eq!(session.compaction_state().status(), "failed");
}

/// senpi test/suite/regressions/4272-epoch-producer.test.ts and 4272-terminal-inprocess.test.ts: under compact
/// updates each message's events carry one recovery carrier to session listeners, its transcript append records
/// that owner, and a new host epoch re-mints while a same-epoch rebind keeps the open message.
#[tokio::test]
async fn emitted_messages_carry_their_recovery_owner_into_the_transcript() {
    let session = retry_session(
        vec![maho_ai::providers::faux::faux_assistant_message(
            "reply",
            Default::default(),
        )],
        0,
    );
    *lock(&session.recovery_identity) =
        Some(crate::session_recovery_identity::SessionRecoveryIdentity::new("epoch-1".into()));
    session.set_session_recovery_epoch("epoch-1");
    session.with_session_manager_mut(|manager| manager.begin_transcript_journal());
    let carriers = Arc::new(Mutex::new(Vec::new()));
    let seen = carriers.clone();
    let weak = session.weak_accessor();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::Agent(event) = event
            && let Some(kind) = match event {
                maho_agent::types::AgentEvent::MessageStart { message, .. } => {
                    Some(("start", message.role()))
                }
                maho_agent::types::AgentEvent::MessageEnd { message } => {
                    Some(("end", message.role()))
                }
                _ => None,
            }
            && let Some(session) = weak()
        {
            lock(&seen).push((kind, session.recovery_carrier(&Value::Null)));
        }
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("hello", Default::default()),
    )
    .await
    .expect("bounded turn")
    .expect("prompt");
    let carriers = lock(&carriers).clone();
    let of = |kind, role| {
        carriers
            .iter()
            .find(|(seen, _)| *seen == (kind, role))
            .and_then(|(_, carrier)| carrier.clone())
            .unwrap_or_else(|| panic!("{kind} {role} carries its recovery identity"))
    };
    let (user, assistant) = (of("start", "user"), of("start", "assistant"));
    assert_eq!(of("end", "user"), user);
    assert_eq!(of("end", "assistant"), assistant);
    assert_ne!(user.message_id, assistant.message_id);
    assert_eq!(user.epoch, "epoch-1");
    let owners: Vec<_> = session
        .with_session_manager_mut(|manager| manager.drain_transcript())
        .into_iter()
        .filter_map(|transition| match transition {
            crate::session_transcript_journal::TranscriptTransition::Append {
                owning_message_id: Some(id),
                recovery_epoch,
                ..
            } => Some((id, recovery_epoch)),
            _ => None,
        })
        .collect();
    for owner in [&user, &assistant] {
        assert!(
            owners.contains(&(owner.message_id.clone(), Some("epoch-1".to_owned()))),
            "{owner:?} owns its transcript append"
        );
    }
    assert!(
        owners
            .iter()
            .all(|(_, epoch)| epoch.as_deref() == Some("epoch-1"))
    );
    assert!(
        session.recovery_carrier(&Value::Null).is_none(),
        "a carrier is readable only while its record is emitted"
    );
}

#[test]
fn a_session_without_compact_updates_mints_no_recovery_carrier() {
    let session = test_session();
    *lock(&session.recovery_identity) = None;
    let message = AgentMessage::Llm(maho_ai::types::Message::User(maho_ai::types::UserMessage {
        content: maho_ai::types::UserContent::Text("hi".into()),
        timestamp: 1,
    }));
    assert!(
        session
            .observe_recovery(&maho_agent::types::AgentEvent::MessageEnd { message })
            .is_none()
    );
}

/// senpi test/suite/regressions/4272-structured-lane-bridge.test.ts: the structured lane delivers the session's
/// events with their provider capture and each block control between the events around it (`subscribe` has no
/// control variant to see), and a session built without an internal channel refuses a structured listener.
#[tokio::test]
async fn the_structured_lane_orders_block_controls_among_session_events() {
    use crate::structured_session_lane::AgentSessionStructuredRecord as Record;
    use maho_ai::utils::assistant_capture::{
        AssistantProgress, BlockPhase, get_assistant_capture, push_assistant_progress,
    };
    assert!(
        test_session()
            .subscribe_structured(Arc::new(|_: &Record| {}))
            .is_err()
    );
    let channel = maho_agent::create_internal_agent_channel();
    let stream_fn: maho_agent::types::StreamFn = Arc::new(|_, _, _| {
        let stream =
            maho_ai::utils::event_stream::AssistantMessageEventStream::with_capture_blocks(true);
        let mut partial = maho_ai::providers::faux::faux_assistant_message("", Default::default());
        partial.content.clear();
        stream.push(maho_ai::types::AssistantMessageEvent::Start {
            partial: partial.clone(),
        });
        let native =
            maho_ai::types::ContentBlock::ProviderNative(maho_ai::types::ProviderNativeContent {
                subtype: "fixture".into(),
                raw: json!({"value": 1}),
            });
        partial.content.push(native.clone());
        push_assistant_progress(
            &stream,
            AssistantProgress::block_state(0, BlockPhase::Insert, &native),
        );
        stream.push(maho_ai::types::AssistantMessageEvent::Done {
            reason: maho_ai::types::DoneReason::Stop,
            message: partial.clone(),
        });
        stream.end(Some(partial));
        stream
    });
    let agent = Agent::new(maho_agent::AgentOptions {
        internal_channel: Some(channel.clone()),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });
    let session = AgentSession::new_with_internal_channel(
        AgentSessionConfig {
            agent,
            session_manager: SessionManager::in_memory("/tmp", None, None),
            settings_manager: SettingsManager::from_storage(
                Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
                false,
            ),
            cwd: "/tmp".to_owned(),
            agent_dir: Some("/tmp/maho-agent".to_owned()),
            fallback_now: None,
            retry_random: None,
            scoped_models: Vec::new(),
            favorite_models: Vec::new(),
            flag_values: BTreeMap::new(),
            custom_tools: Vec::new(),
            model_runtime: Some(
                ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
                    providers: Some(Vec::new()),
                    ..Default::default()
                })
                .expect("valid model runtime"),
            ),
            model_registry: None,
            uses_default_stream_function: Some(false),
            initial_active_tool_names: None,
            default_tool_names: None,
            eval_only_tool_names: None,
            allowed_tool_names: None,
            excluded_tool_names: None,
            base_tools_override: None,
            session_start_event: None,
            auto_title_sessions: None,
        },
        None,
        Some(channel),
    )
    .expect("session");
    let structured = Arc::new(Mutex::new(Vec::new()));
    let seen = structured.clone();
    let subscription = session
        .subscribe_structured(Arc::new(move |record: &Record| {
            let label = match record {
                Record::Progress(_) => ("progress".to_owned(), false),
                Record::Event(event) => match &event.value {
                    AgentSessionEvent::Agent(agent) => (
                        serde_json::to_value(agent).expect("event json")["type"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                        get_assistant_capture(event).is_some(),
                    ),
                    _ => ("session".to_owned(), false),
                },
            };
            lock(&seen).push(label);
        }))
        .expect("the session layer owns the channel");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session
            .agent
            .prompt(maho_agent::AgentPromptInput::text("go")),
    )
    .await
    .expect("bounded turn");
    drop(subscription);
    let structured = lock(&structured).clone();
    let labels: Vec<_> = structured.iter().map(|(label, _)| label.as_str()).collect();
    let progress = labels
        .iter()
        .position(|label| *label == "progress")
        .expect("the block control reached the structured lane");
    let assistant_start = labels
        .iter()
        .rposition(|label| *label == "message_start")
        .expect("assistant message_start");
    let assistant_end = labels
        .iter()
        .rposition(|label| *label == "message_end")
        .expect("assistant message_end");
    assert!(
        assistant_start < progress && progress < assistant_end,
        "{labels:?}"
    );
    assert!(
        structured[assistant_start].1,
        "the assistant start carries its provider capture"
    );
}

struct FailingControlHost {
    disposed: Arc<AtomicU64>,
}
impl crate::session_control_actions::ControlEndpointHost for FailingControlHost {
    fn register(
        &self,
        _: crate::extensions::session_control_types::RegisterControlEndpointOptions,
    ) -> futures::future::BoxFuture<
        '_,
        crate::extensions::session_control_types::SessionControlRegistration,
    > {
        let disposed = self.disposed.clone();
        Box::pin(async move {
            crate::extensions::session_control_types::SessionControlRegistration::Registered {
                socket: "/tmp/control.sock".into(),
                dispose: Arc::new(
                    move || -> futures::future::BoxFuture<'static, Result<(), String>> {
                        disposed.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async { Err("socket busy".to_owned()) })
                    },
                ),
            }
        })
    }
}

/// senpi test/suite/regressions/3246-control-endpoint-reload-generation.test.ts: a reload retires the control
/// endpoints the previous extension generation registered, reports a failed retirement as a `<session-control>`
/// reload error, and still installs the next generation.
#[tokio::test]
async fn reload_retires_the_previous_generations_control_endpoints() {
    use crate::extensions::session_control_types::SessionControlActions;
    let session = test_session();
    let disposed = Arc::new(AtomicU64::new(0));
    session.set_control_endpoint_host(Some(Arc::new(FailingControlHost {
        disposed: disposed.clone(),
    })));
    let extension = maho_ext_api::LoadedExtension::new(
        "<inline:control>",
        session.cwd().into(),
        Default::default(),
    );
    let retained = extension.clone();
    let mut runner = maho_ext_host::ExtensionRunner::new(
        vec![extension],
        Default::default(),
        Default::default(),
        test_extension_context(&session),
    );
    runner.set_runtime_factory(Arc::new(move |context| {
        let extension = retained.clone();
        Box::pin(async move {
            Ok(maho_ext_host::ExtensionRunner::new(
                vec![extension],
                Default::default(),
                Default::default(),
                context,
            ))
        })
    }));
    session.set_extension_runner(runner).await;
    let errors = Arc::new(Mutex::new(Vec::new()));
    let seen = errors.clone();
    session.state().extension_error_listener = Some(Arc::new(move |error: &ExtensionError| {
        lock(&seen).push((
            error.extension_path.clone(),
            error.event.clone(),
            error.error.clone(),
        ));
    }));
    let first = session.session_control().expect("first generation");
    let registration = first
        .register_control_endpoint(
            crate::extensions::session_control_types::RegisterControlEndpointOptions {
                inbox_dir: "/tmp/inbox".into(),
                drain: Arc::new(|_: Value| {
                    Box::pin(async { Ok(None) })
                        as futures::future::BoxFuture<'static, Result<Option<Value>, String>>
                }),
                is_session_referenced: None,
            },
        )
        .await;
    assert!(matches!(
        registration,
        crate::extensions::session_control_types::SessionControlRegistration::Registered { .. }
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), session.reload())
        .await
        .expect("bounded reload")
        .expect("reload");
    assert_eq!(disposed.load(Ordering::SeqCst), 1);
    assert_eq!(
        *lock(&errors),
        vec![(
            "<session-control>".to_owned(),
            "reload".to_owned(),
            "socket busy".to_owned()
        )]
    );
    let next = session.session_control().expect("next generation");
    assert!(!Arc::ptr_eq(&first, &next));
}

/// senpi test/suite/regressions/4295-session-recovered-notice.test.ts: a torn session tail found on open is
/// emitted once as `session_recovered`, replayed to later subscribers, and cleared by the next prompt.
#[tokio::test]
async fn a_torn_session_tail_is_replayed_until_the_next_prompt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("torn.jsonl");
    let header = serde_json::to_string(&json!({
        "type": "session", "version": crate::session_manager::CURRENT_SESSION_VERSION, "id": "s",
        "timestamp": "2026-01-01T00:00:00.000Z", "cwd": "/tmp",
    }))
    .expect("header");
    std::fs::write(&path, format!("{header}\n{{\"type\":\"mess")).expect("torn");
    let manager = SessionManager::open(&path.to_string_lossy(), None, None, None).expect("open");
    let session = test_session_with_manager(manager);
    let recovered = |events: &Arc<Mutex<Vec<AgentSessionEvent>>>| {
        lock(events)
            .iter()
            .filter_map(|event| match event {
                AgentSessionEvent::SessionRecovered {
                    cause,
                    backup_path,
                    dropped_bytes,
                    ..
                } => Some((cause.clone(), backup_path.clone(), *dropped_bytes)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let subscribe = || {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let subscription = session.subscribe(Arc::new(move |event| {
            lock(&sink).push(event.clone());
        }));
        (events, subscription)
    };
    let (first, _first) = subscribe();
    let seen = recovered(&first);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "unknown");
    assert_eq!(
        std::fs::read_to_string(&seen[0].1).expect("backup"),
        "{\"type\":\"mess"
    );
    assert_eq!(seen[0].2, "{\"type\":\"mess".len() as u64);
    let (second, _second) = subscribe();
    assert_eq!(recovered(&second), seen);
    // The next prompt clears it, even one that is refused for missing auth.
    let _ = session.prompt("next", Default::default()).await;
    let (third, _third) = subscribe();
    assert!(recovered(&third).is_empty());
}

/// senpi#4295 on the append path: the fragment of a failed append is set aside by the next append, inside the
/// session manager's lock. `session_recovered` is emitted after that lock is released, so a subscriber that
/// reads the session manager does not deadlock.
#[test]
fn a_recovery_found_by_an_append_is_emitted_after_the_session_manager_lock_is_released() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manager = SessionManager::create(
        &dir.path().to_string_lossy(),
        Some(&dir.path().to_string_lossy()),
        None,
    );
    let session = test_session_with_manager(manager);
    let path = PathBuf::from(
        session
            .with_session_manager(|manager| manager.session_file().map(str::to_owned))
            .expect("session file"),
    );
    session.with_session_manager_mut(|manager| {
        manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        manager.append_message(json!({ "role": "assistant", "content": [], "timestamp": 2 }));
    });
    // A real failed write: the session file is a directory while the append runs.
    let aside = dir.path().join("aside.jsonl");
    std::fs::rename(&path, &aside).expect("move file aside");
    std::fs::create_dir(&path).expect("directory in its place");
    let failure = session.with_session_manager_mut(|manager| {
        manager.append_message(json!({ "role": "user", "content": "b", "timestamp": 3 }));
        manager.take_write_error()
    });
    assert!(failure.is_some(), "the append must fail");
    std::fs::remove_dir(&path).expect("remove directory");
    let mut torn = std::fs::read(&aside).expect("read file");
    torn.extend_from_slice(b"{\"type\":\"mess");
    std::fs::write(&path, torn).expect("file with a torn tail");

    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let weak = Arc::downgrade(&session.inner);
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::SessionRecovered { dropped_bytes, .. } = event
            && let Some(inner) = weak.upgrade()
        {
            let entries =
                AgentSession { inner }.with_session_manager(|manager| manager.entries().len());
            lock(&sink).push((*dropped_bytes, entries));
        }
    }));
    let (done, finished) = std::sync::mpsc::channel();
    let appender = session.clone();
    std::thread::spawn(move || {
        appender.with_session_manager_mut(|manager| {
            manager.append_message(json!({ "role": "user", "content": "c", "timestamp": 4 }));
        });
        let _ = done.send(());
    });
    finished
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the append completes without deadlocking");
    let seen = lock(&seen).clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "{\"type\":\"mess".len() as u64);
    assert!(lock(&session.pending_session_recoveries).is_empty());
}

/// senpi test/suite/regressions/4282-model-not-found-config-error.test.ts: a model configuration error is never
/// retried, and emits `model_config_error` once per session and model.
#[tokio::test]
async fn a_model_config_error_is_emitted_once_and_never_retried() {
    let mut failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("model_not_found: the model does not exist".to_owned()),
            ..Default::default()
        },
    );
    failed.error_cause = Some(maho_ai::types::AssistantErrorCause::ModelConfig {
        model: failed.model.clone(),
    });
    let session = retry_session(Vec::new(), 3);
    session.with_session_manager_mut(|manager| {
        manager.append_message(json!({"role":"user","content":"task","timestamp":0}));
        manager.append_message(serde_json::to_value(&failed).expect("failed"));
    });
    session.rebuild_session_context().expect("context");
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let _subscription = session.subscribe(Arc::new(move |event| match event {
        AgentSessionEvent::ModelConfigError {
            model,
            provider,
            error_excerpt,
        } => lock(&seen).push((model.clone(), provider.clone(), error_excerpt.clone())),
        AgentSessionEvent::AutoRetryStart { .. } => {
            lock(&seen).push(("retry".into(), String::new(), String::new()))
        }
        _ => {}
    }));
    for _ in 0..2 {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session.finish_provider_turn(),
        )
        .await
        .expect("bounded turn")
        .expect("turn");
    }
    assert_eq!(
        *lock(&events),
        vec![(
            format!("{}/{}", failed.provider, failed.model),
            failed.provider.clone(),
            "model_not_found: the model does not exist".to_owned()
        )]
    );
}

#[tokio::test]
async fn successful_overflow_compacts_without_retrying_completed_assistant() {
    let mut completed =
        maho_ai::providers::faux::faux_assistant_message("completed answer", Default::default());
    completed.usage.input = 200_000;
    let session = retry_session(Vec::new(), 0);
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"task","timestamp":0}));
        manager.append_message(serde_json::to_value(completed).expect("completed assistant"));
    });
    session.rebuild_session_context().expect("context");
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"keepRecentTokens":1,"reserveTokens":0}),
        )]))
    });
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:completed-overflow>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("preparation");
            };
            let result = maho_ext_api::CompactionResult {
                summary: "digest".to_owned(),
                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                tokens_before: event.preparation.tokens_before,
                details: None,
            };
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(result),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let retries = Arc::new(Mutex::new(Vec::new()));
    let captured = retries.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::CompactionEnd { will_retry, .. } = event {
            lock(&captured).push(*will_retry);
        }
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.finish_provider_turn(),
    )
    .await
    .expect("bounded completion")
    .expect("completion");
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some("completed answer")
    );
    assert_eq!(*lock(&retries), vec![false]);
    assert_eq!(session.compaction_state().status(), "completed");
    session
        .finish_provider_turn()
        .await
        .expect("old assistant resampled");
    assert_eq!(
        *lock(&retries),
        vec![false],
        "pre-compaction usage cannot retrigger compaction"
    );
    let prior_messages = session.messages();
    let original_model = session.model();
    let mut small_model = original_model.clone();
    small_model.context_window = 128;
    session.agent.set_model(small_model);
    let mut oversized_messages = prior_messages.clone();
    oversized_messages.push(
        session_message_from_value(serde_json::json!({
            "role":"custom", "customType":"fresh-content", "content":"large input ".repeat(1000),
            "display":true, "timestamp":0,
        }))
        .expect("custom content"),
    );
    session.agent.set_messages(oversized_messages);
    session
        .finish_provider_turn()
        .await
        .expect_err("fresh oversized content still requires compaction");
    session.agent.set_model(original_model);
    session.agent.set_messages(prior_messages.clone());
    let mut pending_assistant =
        maho_ai::providers::faux::faux_assistant_message("pending new answer", Default::default());
    pending_assistant.usage.input = 200_000;
    pending_assistant.usage.total_tokens = 200_000;
    pending_assistant.stop_reason = StopReason::Error;
    pending_assistant.error_message = Some("prompt is too long".into());
    let pending_assistant = AgentMessage::from(maho_ai::types::Message::Assistant(
        std::sync::Arc::new(pending_assistant),
    ));
    session
        .process_agent_event(
            maho_agent::types::AgentEvent::MessageStart {
                message: pending_assistant.clone(),
                streaming: None,
            },
            maho_ai::utils::abort::AbortController::new().signal(),
        )
        .await;
    let mut pending_messages = prior_messages.clone();
    pending_messages.push(pending_assistant);
    session.agent.set_messages(pending_messages);
    let pending_result = session.finish_provider_turn().await;
    assert!(
        pending_result.is_err() || lock(&retries).len() > 1,
        "unpersisted assistant is not the old branch assistant"
    );
}

#[tokio::test]
async fn overflow_from_previous_model_does_not_compact_small_current_context() {
    let session = test_session();
    session.agent.set_model(test_model());
    let mut failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("prompt is too long".to_owned()),
            ..Default::default()
        },
    );
    failed.model = "previous-model".to_owned();
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"task","timestamp":0}))
    });
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::to_value(failed).expect("failure"))
    });
    session.rebuild_session_context().expect("context");
    session
        .finish_provider_turn()
        .await
        .expect("stale overflow ignored");
    assert_eq!(session.compaction_state().generation(), 0);
    assert_eq!(
        session
            .with_session_manager(|manager| manager.entries())
            .len(),
        2
    );
}

#[tokio::test]
async fn compaction_preserves_assistant_waiting_for_message_end_persistence() {
    let mut assistant = maho_ai::providers::faux::faux_assistant_message(
        "delayed assistant payload",
        Default::default(),
    );
    assistant.timestamp = 7;
    let session = retry_session(vec![assistant], 0);
    let first = session.with_session_manager_mut(|manager| {
        manager.append_message(
            serde_json::json!({"role":"user","content":"seed history","timestamp":0}),
        )
    });
    session.rebuild_session_context().expect("seed history");
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered_tx)));
    let release = Arc::new(Mutex::new(Some(release_rx)));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:delay-persistence>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(maho_ext_api::EventKind::MessageEnd, vec![Arc::new(move |event, _| {
        let is_assistant = matches!(event, maho_ext_api::ExtensionEvent::MessageEnd { message } if message.as_assistant().is_some());
        let signals = if is_assistant { lock(&entered).take().zip(lock(&release).take()) } else { None };
        Box::pin(async move {
            if let Some((entered, release)) = signals {
                entered.send(()).expect("hook observer");
                release.await.expect("release hook");
            }
            Ok(maho_ext_api::EventResult::None)
        })
    })]);
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let (prompted, (applied, reapplied, retained)) =
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(
                session.prompt("produce delayed assistant", Default::default()),
                async {
                    entered_rx.await.expect("message end entered");
                    let applied = session.apply_compaction(
                        &crate::compaction::compaction::CompactionResult {
                            summary: "digest".into(),
                            first_kept_entry_id: first["id"].as_str().expect("entry").into(),
                            tokens_before: 42,
                            estimated_tokens_after: None,
                            usage: None,
                            details: None,
                        },
                    );
                    let reapplied = session.apply_compaction(
                        &crate::compaction::compaction::CompactionResult {
                            summary: "second digest".into(),
                            first_kept_entry_id: first["id"].as_str().expect("entry").into(),
                            tokens_before: 42,
                            estimated_tokens_after: None,
                            usage: None,
                            details: None,
                        },
                    );
                    let retained = session
                        .messages()
                        .iter()
                        .filter(|message| message.as_assistant().is_some())
                        .count();
                    release_tx.send(()).expect("release pending persistence");
                    (applied, reapplied, retained)
                }
            )
        })
        .await
        .expect("bounded persistence");
    applied.expect("compaction");
    reapplied.expect("repeated compaction");
    prompted.expect("prompt completion");
    assert_eq!(retained, 1, "pending assistant remains in runtime context");
    let branch = session.with_session_manager(|manager| manager.current_branch());
    let compacted = branch
        .iter()
        .rposition(|entry| entry["type"] == "compaction")
        .expect("compaction entry");
    let assistants: Vec<_> = branch
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry["message"]["role"] == "assistant")
        .collect();
    assert_eq!(assistants.len(), 1);
    assert!(assistants[0].0 > compacted);
    assert_eq!(assistants[0].1["message"]["timestamp"], 7);
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some("delayed assistant payload")
    );
    assert!(session.state().messages_awaiting_persistence.is_empty());
}

#[tokio::test]
async fn dropped_message_end_releases_pending_persistence_owner() {
    use std::{future::Future, task::Poll};
    let session = retry_session(Vec::new(), 0);
    let first = session.with_session_manager_mut(|manager| {
        manager.append_message(
            serde_json::json!({"role":"user","content":"seed history","timestamp":0}),
        )
    });
    session.rebuild_session_context().expect("history");
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:cancel-persistence>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::MessageEnd,
        vec![Arc::new(|_, _| Box::pin(std::future::pending()))],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let assistant =
        maho_ai::providers::faux::faux_assistant_message("abandoned payload", Default::default());
    let mut event = Box::pin(session.process_agent_event(
        maho_agent::types::AgentEvent::MessageEnd {
            message: AgentMessage::from(maho_ai::types::Message::Assistant(std::sync::Arc::new(
                assistant,
            ))),
        },
        maho_ai::utils::abort::AbortController::new().signal(),
    ));
    std::future::poll_fn(|cx| {
        assert!(event.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(session.state().messages_awaiting_persistence.len(), 1);
    drop(event);
    assert!(session.state().messages_awaiting_persistence.is_empty());
    session
        .apply_compaction(&crate::compaction::compaction::CompactionResult {
            summary: "digest".into(),
            first_kept_entry_id: first["id"].as_str().expect("entry").into(),
            tokens_before: 42,
            estimated_tokens_after: None,
            usage: None,
            details: None,
        })
        .expect("later compaction");
    assert!(
        session
            .messages()
            .iter()
            .all(|message| message.as_assistant().is_none())
    );
    assert!(
        session
            .with_session_manager(|manager| manager.entries())
            .iter()
            .all(|entry| entry["message"]["role"] != "assistant")
    );
}

#[tokio::test]
async fn first_successful_assistant_after_compaction_is_usage_exempt() {
    let mut completed = maho_ai::providers::faux::faux_assistant_message(
        "completed after digest",
        Default::default(),
    );
    completed.usage.input = 200_000;
    completed.usage.total_tokens = 200_000;
    let session = retry_session(Vec::new(), 0);
    session.agent.set_stream_function(Arc::new(move |_, _, _| {
        let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
        stream.push(maho_ai::types::AssistantMessageEvent::Start {
            partial: completed.clone(),
        });
        stream.push(maho_ai::types::AssistantMessageEvent::Done {
            reason: maho_ai::types::DoneReason::Stop,
            message: completed.clone(),
        });
        stream
    }));
    session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
        ("compaction".into(), serde_json::json!({"keepRecentTokens":1,"reserveTokens":0,"reserveScalingEnabled":false})),
    ])));
    for text in ["old task".repeat(200), "recent task".repeat(200)] {
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0}))
        });
    }
    session.rebuild_session_context().expect("history");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let captured = calls.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:usage-exemption>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(move |event, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("preparation");
            };
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(maho_ext_api::CompactionResult {
                            summary: "digest".into(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                            tokens_before: event.preparation.tokens_before,
                            details: None,
                        }),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    session.compact(None).await.expect("manual compaction");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("next task", Default::default()),
    )
    .await
    .expect("bounded provider turn")
    .expect("successful turn");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "first new successful assistant is exempt"
    );
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some("completed after digest")
    );
    assert_eq!(
        session
            .messages()
            .iter()
            .rev()
            .find_map(AgentMessage::as_assistant)
            .expect("delivered assistant")
            .usage
            .input,
        200_000
    );
    assert!(!session.agent.has_queued_messages());
    session
        .finish_provider_turn()
        .await
        .expect("same assistant resampled");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    session
        .revalidate_scheduled_continuation_admission()
        .await
        .expect("exempt continuation admission");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "admission preserves the consumed exemption"
    );
    let identical = session
        .messages()
        .iter()
        .rev()
        .find_map(AgentMessage::as_assistant)
        .cloned()
        .expect("assistant");
    session.agent.set_stream_function(Arc::new(|_, _, _| {
        let response = maho_ai::providers::faux::faux_assistant_message(
            "zero usage followup",
            Default::default(),
        );
        let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
        stream.push(maho_ai::types::AssistantMessageEvent::Start {
            partial: response.clone(),
        });
        stream.push(maho_ai::types::AssistantMessageEvent::Done {
            reason: maho_ai::types::DoneReason::Stop,
            message: response,
        });
        stream
    }));
    session
        .follow_up("zero usage task", None, Default::default())
        .await
        .expect("followup");
    session
        .agent
        .continue_with_queued_messages(Default::default())
        .await
        .expect("continuation admitted");
    session
        .finish_provider_turn()
        .await
        .expect("zero usage continuation");
    assert_eq!(
        session.get_last_assistant_text().as_deref(),
        Some("zero usage followup")
    );
    assert_eq!(
        session
            .messages()
            .iter()
            .rev()
            .find_map(AgentMessage::as_assistant)
            .expect("zero usage assistant")
            .usage
            .total_tokens,
        0
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "actual pinned stream recompacts after zero usage"
    );
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::to_value(identical).expect("identical contents"))
    });
    session
        .rebuild_session_context()
        .expect("distinct persisted assistant");
    session
        .finish_provider_turn()
        .await
        .expect("distinct assistant checked");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "equal contents do not share the exemption"
    );
}

#[tokio::test]
async fn pending_post_compaction_prompt_defers_agent_queue_ownership() {
    use std::{future::Future, task::Poll};
    let session = retry_session(Vec::new(), 0);
    let first = session.with_session_manager_mut(|manager| {
        manager
            .append_message(serde_json::json!({"role":"user","content":"old task","timestamp":0}))
    });
    session.with_session_manager_mut(|manager| {
        manager.append_message(
            serde_json::json!({"role":"user","content":"recent task","timestamp":0}),
        )
    });
    session.rebuild_session_context().expect("history");
    session
        .apply_compaction(&crate::compaction::compaction::CompactionResult {
            summary: "digest".into(),
            first_kept_entry_id: first["id"].as_str().expect("entry").into(),
            tokens_before: 10,
            estimated_tokens_after: None,
            usage: None,
            details: None,
        })
        .expect("compaction");
    let admission = session.prompt_admission.lock().await;
    let mut prompt = Box::pin(session.prompt("pending turn", Default::default()));
    std::future::poll_fn(|cx| {
        assert!(prompt.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    session
        .steer("later steering", None, Default::default())
        .await
        .expect("steering");
    session
        .follow_up("later followup", None, Default::default())
        .await
        .expect("followup");
    assert_eq!(session.get_steering_messages(), ["later steering"]);
    assert_eq!(session.get_follow_up_messages(), ["later followup"]);
    let mut queued_prompt = Box::pin(session.prompt(
        "explicit followup",
        PromptOptions {
            streaming_behavior: Some(StreamingBehavior::FollowUp),
            ..Default::default()
        },
    ));
    let queued = std::future::poll_fn(|cx| Poll::Ready(queued_prompt.as_mut().poll(cx))).await;
    drop(queued_prompt);
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:reject-pending-input>",
        session.cwd().into(),
        Default::default(),
    );
    let input_calls = Arc::new(AtomicU64::new(0));
    let observed_calls = input_calls.clone();
    extension.handlers.insert(
        maho_ext_api::EventKind::Input,
        vec![Arc::new(move |event, _| {
            observed_calls.fetch_add(1, Ordering::SeqCst);
            *event = maho_ext_api::ExtensionEvent::AgentStart;
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let (sent, received) = tokio::sync::oneshot::channel();
    let sent = Arc::new(Mutex::new(Some(sent)));
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(event, AgentSessionEvent::ContinuationError { .. })
            && let Some(sent) = lock(&sent).take()
        {
            sent.send(()).expect("error observer");
        }
    }));
    maho_ext_api::ExtensionActions::send_user_message(
        &SessionExtensionActions(Arc::downgrade(&session.inner)),
        maho_ext_api::UserMessageContent::Text("raw rejected input".into()),
        maho_ext_api::SendUserMessageOptions {
            deliver_as: Some(StreamingBehavior::FollowUp),
            ..Default::default()
        },
    )
    .expect("extension input");
    tokio::time::timeout(std::time::Duration::from_secs(5), received)
        .await
        .expect("bounded rejection")
        .expect("rejection event");
    assert_eq!(
        session.get_follow_up_messages(),
        ["later followup", "explicit followup", "raw rejected input"]
    );
    assert_eq!(
        input_calls.load(Ordering::SeqCst),
        1,
        "fallback retains raw input without re-running hooks"
    );
    let agent_owned = session.agent.has_queued_messages();
    drop(prompt);
    drop(admission);
    session.clear_queue(false);
    assert!(
        matches!(queued, Poll::Ready(Ok(PromptDisposition::Queued))),
        "explicit queue mode must not wait for pending prompt"
    );
    assert!(
        !agent_owned,
        "pending post-compaction inputs belong to the session until checked"
    );
    assert!(!session.state().prompt_start_pending);
    assert!(!session.work_barrier.has_active_work());
    assert!(!session.agent.has_queued_messages());
}

#[tokio::test]
async fn deferred_post_compaction_inputs_deliver_once_after_first_response() {
    use std::{future::Future, task::Poll};
    for (clear_before_release, failed_first) in [(false, false), (true, false), (false, true)] {
        let mut responses: Vec<_> = ["initial response", "steering response", "followup response"]
            .into_iter()
            .map(|text| maho_ai::providers::faux::faux_assistant_message(text, Default::default()))
            .collect();
        if failed_first {
            responses[0].stop_reason = StopReason::Error;
            responses[0].error_message = Some("invalid fixture request".into());
        }
        let session = retry_session(responses, 0);
        let first = session.with_session_manager_mut(|manager| {
            manager.append_message(
                serde_json::json!({"role":"user","content":"old task","timestamp":0}),
            )
        });
        session.with_session_manager_mut(|manager| {
            manager.append_message(
                serde_json::json!({"role":"user","content":"recent task","timestamp":0}),
            )
        });
        session.rebuild_session_context().expect("history");
        session
            .apply_compaction(&crate::compaction::compaction::CompactionResult {
                summary: "digest".into(),
                first_kept_entry_id: first["id"].as_str().expect("entry").into(),
                tokens_before: 10,
                estimated_tokens_after: None,
                usage: None,
                details: None,
            })
            .expect("compaction");
        let admission = session.prompt_admission.lock().await;
        let mut prompt = Box::pin(session.prompt("pending turn", Default::default()));
        std::future::poll_fn(|cx| {
            assert!(prompt.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        session
            .steer("later steering", None, Default::default())
            .await
            .expect("steering");
        session
            .follow_up("later followup", None, Default::default())
            .await
            .expect("followup");
        assert!(!session.agent.has_queued_messages());
        if clear_before_release {
            let cleared = session.clear_queue(false);
            assert_eq!(cleared.steering, ["later steering"]);
            assert_eq!(cleared.follow_up, ["later followup"]);
        }
        drop(admission);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), prompt)
                .await
                .expect("bounded delivery")
                .expect("prompt"),
            PromptDisposition::Started
        );
        assert_eq!(
            session.get_last_assistant_text().as_deref(),
            Some(if clear_before_release {
                "initial response"
            } else {
                "followup response"
            })
        );
        let persisted = session.with_session_manager(|manager| manager.entries());
        for text in ["pending turn", "later steering", "later followup"] {
            assert_eq!(
                persisted
                    .iter()
                    .filter(|entry| entry["message"]["role"] == "user"
                        && entry["message"]["content"][0]["text"] == text)
                    .count(),
                usize::from(!clear_before_release || text == "pending turn"),
                "{text}"
            );
        }
        assert_eq!(session.pending_message_count(), 0);
        assert!(!session.agent.has_queued_messages());
        assert!(!session.state().prompt_start_pending);
        assert!(!session.work_barrier.has_active_work());
    }
}

#[tokio::test]
async fn provider_overflow_recovers_when_proactive_compaction_is_disabled() {
    let failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("prompt is too long".to_owned()),
            ..Default::default()
        },
    );
    let session = retry_session(
        vec![
            failed,
            maho_ai::providers::faux::faux_assistant_message("recovered", Default::default()),
        ],
        0,
    );
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"enabled":false,"keepRecentTokens":1}),
        )]))
    });
    session.with_session_manager_mut(|manager| {
        manager.append_message(
            serde_json::json!({"role":"user","content":"previous task","timestamp":0}),
        )
    });
    session.rebuild_session_context().expect("context");
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:overflow-recovery>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("preparation");
            };
            assert_eq!(event.reason, maho_ext_api::CompactionReason::Overflow);
            let result = maho_ext_api::CompactionResult {
                summary: "digest".to_owned(),
                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                tokens_before: event.preparation.tokens_before,
                details: None,
            };
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(result),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("current task", Default::default()),
    )
    .await
    .expect("bounded recovery")
    .expect("prompt");
    assert_eq!(
        session
            .messages()
            .last()
            .and_then(AgentMessage::as_assistant)
            .expect("assistant")
            .stop_reason,
        StopReason::Stop
    );
    assert_eq!(session.compaction_state().status(), "completed");
}

#[tokio::test]
async fn rejected_overflow_recovery_retains_deferred_followup_in_agent_queue() {
    use std::{future::Future, task::Poll};
    let failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("prompt is too long".into()),
            ..Default::default()
        },
    );
    let session = retry_session(vec![failed], 0);
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([(
            "compaction".into(),
            serde_json::json!({"keepRecentTokens":1,"reserveTokens":0}),
        )]))
    });
    let first = session.with_session_manager_mut(|manager| {
        manager
            .append_message(serde_json::json!({"role":"user","content":"old task","timestamp":0}))
    });
    session.with_session_manager_mut(|manager| {
        manager.append_message(
            serde_json::json!({"role":"user","content":"recent task","timestamp":0}),
        )
    });
    session.rebuild_session_context().expect("history");
    session
        .apply_compaction(&crate::compaction::compaction::CompactionResult {
            summary: "digest".into(),
            first_kept_entry_id: first["id"].as_str().expect("entry").into(),
            tokens_before: 10,
            estimated_tokens_after: None,
            usage: None,
            details: None,
        })
        .expect("compaction");
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let weak = Arc::downgrade(&session.inner);
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:reject-deferred-overflow>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(move |_, _| {
            let session = AgentSession {
                inner: weak.upgrade().expect("session"),
            };
            lock(&captured).push(session.agent.has_queued_messages());
            Box::pin(async {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        cancel: Some(true),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let admission = session.prompt_admission.lock().await;
    let mut prompt = Box::pin(session.prompt("current task", Default::default()));
    std::future::poll_fn(|cx| {
        assert!(prompt.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    session
        .follow_up("retained input", None, Default::default())
        .await
        .expect("followup");
    assert!(!session.agent.has_queued_messages());
    drop(admission);
    tokio::time::timeout(std::time::Duration::from_secs(5), prompt)
        .await
        .expect("bounded rejection")
        .expect_err("rejected compaction");
    let queued = session.get_follow_up_messages();
    let agent_owned = session.agent.has_queued_messages();
    session.clear_queue(false);
    assert_eq!(
        *lock(&observed),
        [true],
        "overflow transfers queue before recovery hook"
    );
    assert_eq!(queued, ["retained input"]);
    assert!(agent_owned);
    assert!(!session.state().prompt_start_pending);
}

#[tokio::test]
async fn transient_failures_exhaust_bounded_retry_budget() {
    let failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("overloaded_error".to_owned()),
            ..Default::default()
        },
    );
    let session = retry_session(vec![failed.clone(), failed.clone(), failed], 2);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("input", Default::default()),
    )
    .await
    .expect("bounded prompt")
    .expect("prompt");
    assert!(!session.is_retrying());
    assert_eq!(
        session.with_session_manager(|manager| conversation_entries(manager.entries()).len()),
        4
    );
    let branch =
        session.with_session_manager(|manager| conversation_entries(manager.current_branch()));
    assert_eq!(branch.len(), 2);
    assert_eq!(branch[0]["message"]["role"], "user");
    assert_eq!(branch[1]["message"]["stopReason"], "error");
    assert_eq!(session.messages().len(), 2);
}

#[tokio::test]
async fn non_retryable_failure_after_retry_ends_the_retry() {
    let options = |error: &str| maho_ai::providers::faux::FauxAssistantMessageOptions {
        stop_reason: Some(StopReason::Error),
        error_message: Some(error.to_owned()),
        ..Default::default()
    };
    let session = retry_session(
        vec![
            maho_ai::providers::faux::faux_assistant_message("", options("overloaded_error")),
            maho_ai::providers::faux::faux_assistant_message("", options("Permission denied")),
        ],
        3,
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(event, AgentSessionEvent::AutoRetryEnd { .. }) {
            lock(&observed).push(event.clone());
        }
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("input", Default::default()),
    )
    .await
    .expect("bounded prompt")
    .expect("prompt");
    assert!(matches!(
        lock(&events).as_slice(),
        [AgentSessionEvent::AutoRetryEnd { success: false, attempt: 1, final_error: Some(error) }]
            if error == "Permission denied"
    ));
    assert!(!session.is_retrying());
}

#[tokio::test]
async fn retry_admission_rejection_stops_before_second_provider_request() {
    let failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("overloaded_error".to_owned()),
            ..Default::default()
        },
    );
    let session = retry_session(vec![failed], 2);
    let captured = session.clone();
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(event, AgentSessionEvent::AutoRetryStart { .. }) {
            let mut model = captured.model();
            model.context_window = 128;
            captured.agent.set_model(model);
        }
        if matches!(event, AgentSessionEvent::AutoRetryEnd { .. }) {
            lock(&observed).push(event.clone());
        }
    }));
    session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
        ("compaction".to_owned(), serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false,"keepRecentTokens":1})),
    ])));
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:retry-admission>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("preparation");
            };
            assert_eq!(event.reason, maho_ext_api::CompactionReason::Threshold);
            Box::pin(async {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        cancel: Some(true),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    session.with_session_manager_mut(|manager| {
        manager.append_message(
            serde_json::json!({"role":"user","content":"old context ".repeat(100),"timestamp":0}),
        )
    });
    session.rebuild_session_context().expect("context");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("next", Default::default()),
    )
    .await
    .expect("bounded prompt")
    .expect("blocked retry settles");
    assert_eq!(
        session
            .with_session_manager(|manager| manager.entries())
            .iter()
            .filter(|entry| entry["message"]["role"] == "assistant")
            .count(),
        1
    );
    assert!(
        matches!(&lock(&events)[0], AgentSessionEvent::AutoRetryEnd { success: false, attempt: 1, final_error: Some(error) }
        if error == "Context remains above the compaction threshold because compaction did not complete")
    );
    assert!(!session.is_retrying());
}

#[tokio::test]
async fn abort_retry_cancels_backoff_before_another_request() {
    let failed = maho_ai::providers::faux::faux_assistant_message(
        "",
        maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("overloaded_error".to_owned()),
            ..Default::default()
        },
    );
    let session = retry_session(vec![failed], 2);
    let cancelling = session.clone();
    let terminals = Arc::new(Mutex::new(Vec::new()));
    let captured = terminals.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(event, AgentSessionEvent::AutoRetryStart { .. }) {
            cancelling.abort_retry();
        }
        if matches!(
            event,
            AgentSessionEvent::AgentSettled | AgentSessionEvent::AutoRetryEnd { .. }
        ) {
            lock(&captured).push(event.clone());
        }
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt("input", Default::default()),
    )
    .await
    .expect("bounded prompt")
    .expect("prompt");
    assert!(!session.is_retrying());
    assert_eq!(
        session.with_session_manager(|manager| conversation_entries(manager.entries()).len()),
        2
    );
    let terminals = lock(&terminals);
    assert_eq!(terminals.len(), 2);
    assert!(matches!(terminals[0], AgentSessionEvent::AgentSettled));
    assert!(matches!(
        terminals[1],
        AgentSessionEvent::AutoRetryEnd { success: false, .. }
    ));
}

#[tokio::test]
async fn model_switch_persists_only_after_admission() {
    let session = keyed_test_session();
    let mut model = test_model();
    model.id = "second".to_owned();
    session.set_session_model(model).await.expect("switch");
    assert_eq!(session.model().id, "second");
    assert_eq!(
        session.with_session_manager(|manager| manager.entries()[0]["type"].clone()),
        "model_change"
    );
    assert!(
        session
            .with_settings_manager(|manager| manager.get_string("defaultModel"))
            .is_none()
    );
}

#[tokio::test]
async fn fallback_reduction_retains_selected_branch_not_abandoned_tail() {
    let session = test_session_with_stream_function(false);
    let mut primary = test_model();
    primary.context_window = 1_000_000;
    session.agent.set_model(primary);
    let selected = session.with_session_manager_mut(|manager| {
        let root = manager.append_message(serde_json::json!({"role":"user","content":"root","timestamp":0}));
        for _ in 0..20 {
            manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"selected".repeat(5000)}],"timestamp":0}));
        }
        let selected = manager.leaf_id().expect("selected leaf").to_owned();
        manager.set_leaf(root["id"].as_str()).expect("leaf");
        manager.append_message(serde_json::json!({"role":"user","content":"abandoned","timestamp":1}));
        selected
    });
    session
        .with_session_manager_mut(|manager| manager.set_leaf(Some(&selected)))
        .expect("leaf");
    session.rebuild_session_context().expect("selected context");
    session.with_session_manager_mut(|manager| {
        let root = manager
            .entries()
            .into_iter()
            .find(|entry| entry["type"] == "message")
            .expect("root");
        let selected = manager.leaf_id().expect("selected context leaf").to_owned();
        manager.set_leaf(root["id"].as_str()).expect("leaf");
        manager.append_custom("abandoned-metadata", None);
        manager.set_leaf(Some(&selected)).expect("leaf");
    });
    let mut target = test_model();
    target.id = "fallback-small".to_owned();
    let measured = session
        .with_session_manager(|manager| manager.current_context())
        .messages
        .iter()
        .map(crate::compaction::compaction::estimate_tokens)
        .sum();
    let reduced = session
        .reduce_for_switch_target(&target, measured)
        .expect("fallback reduction");
    session
        .assert_model_usable(&target, reduced)
        .expect("reduced budget");
    let actual: u64 = session
        .with_session_manager(|manager| manager.current_context())
        .messages
        .iter()
        .map(crate::compaction::compaction::estimate_tokens)
        .sum();
    assert_eq!(actual, reduced);
    assert!(
        session
            .with_session_manager(|manager| manager.entries())
            .iter()
            .any(|entry| entry["details"]["schema"] == "senpi.compaction.resume-slice.v1")
    );
    assert!(
        !session
            .messages()
            .iter()
            .any(|message| user_message_text(message) == "abandoned")
    );
    assert!(
        session
            .messages()
            .iter()
            .any(|message| user_message_text(message).starts_with("selected"))
    );
    assert_eq!(
        session
            .with_session_manager(|manager| manager.entries())
            .iter()
            .filter(|entry| entry["type"] == "message")
            .count(),
        22
    );
}

#[tokio::test]
async fn fallback_switch_reduces_context_without_losing_recorded_transcript() {
    let session = keyed_test_session();
    let mut primary = test_model();
    primary.context_window = 1_000_000;
    session.agent.set_model(primary);
    for _ in 0..20 {
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({
                "role":"user","content":[{"type":"text","text":"x".repeat(40_000)}],"timestamp":0
            }))
        });
    }
    session.rebuild_session_context().unwrap();
    let mut target = test_model();
    target.id = "fallback-small".to_owned();
    target.context_window = 128_000;
    let mut deps = SessionFallbackDeps(
        Arc::downgrade(&session.inner),
        crate::retry_fallback::log::FallbackLogger::new(
            std::path::Path::new(&session.agent_dir()),
            None,
        ),
    );
    crate::retry_fallback::controller::RetryFallbackDeps::switch_model(
        &mut deps,
        target.clone(),
        ModelThinkingLevel::Off,
        false,
    )
    .await
    .unwrap();
    assert_eq!(session.model().id, target.id);
    let entries = session.with_session_manager(|manager| manager.entries());
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry["type"] == "message")
            .count(),
        20
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["details"]["schema"] == "senpi.compaction.resume-slice.v1")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["type"] == "model_change" && entry["reason"] == "fallback")
    );
    let live = session
        .with_session_manager(|manager| manager.current_context())
        .messages
        .iter()
        .map(crate::compaction::compaction::estimate_tokens)
        .sum();
    session.assert_model_usable(&target, live).unwrap();
}

#[tokio::test]
async fn empty_manual_compaction_emits_matched_terminal_events() {
    let session = test_session_with_stream_function(false);
    session.agent.set_model(test_model());
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription =
        session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
    assert!(session.compact(None).await.is_err());
    let events = lock(&events);
    let start = events
        .iter()
        .find_map(|event| match event {
            AgentSessionEvent::CompactionStart {
                reason: maho_ext_api::CompactionReason::Manual,
                request_id,
            } => request_id.clone(),
            _ => None,
        })
        .expect("start");
    assert!(events.iter().any(|event| matches!(event,
        AgentSessionEvent::CompactionEnd { reason: maho_ext_api::CompactionReason::Manual, request_id: Some(id),
            aborted: false, result: None, will_retry: false, accepted: Some(false), .. } if id == &start)));
    assert!(!session.is_compacting());
    assert!(
        session
            .with_session_manager(|manager| conversation_entries(manager.entries()))
            .is_empty()
    );
}

#[tokio::test]
async fn user_abort_joins_settling_boundary_and_is_delivered_once() {
    let session = test_session_with_stream_function(false);
    let boundary = session
        .state()
        .abort_provenance
        .begin_agent_end(Vec::new(), false, false);
    session.state().abort_provenance.end_agent_end(&boundary);
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription =
        session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
    tokio::time::timeout(std::time::Duration::from_secs(2), session.abort())
        .await
        .unwrap();
    assert!(lock(&events).is_empty());
    session.emit_late_user_abort().await;
    session.emit_late_user_abort().await;
    assert_eq!(
        lock(&events)
            .iter()
            .filter(|event| matches!(event, AgentSessionEvent::SessionAbort))
            .count(),
        1
    );
    session.state().abort_provenance.close_agent_end_boundary();
    assert!(
        !session
            .state()
            .abort_provenance
            .has_open_agent_end_boundary()
    );
}

#[tokio::test]
async fn settled_precedes_late_user_join_abort_and_inactive_settle_is_noop() {
    let session = test_session_with_stream_function(false);
    let boundary = session
        .state()
        .abort_provenance
        .begin_agent_end(Vec::new(), false, false);
    session.state().abort_provenance.end_agent_end(&boundary);
    assert!(
        session
            .state()
            .abort_provenance
            .join(maho_ext_api::AbortSource::User, false)
            .user_owned
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription = session.subscribe(Arc::new(move |event| {
        if matches!(
            event,
            AgentSessionEvent::AgentSettled | AgentSessionEvent::SessionAbort
        ) {
            lock(&captured).push(event.clone());
        }
    }));
    session.agent_run_active.store(true, Ordering::SeqCst);
    session.emit_agent_settled().await;
    session.emit_agent_settled().await;
    let events = lock(&events);
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], AgentSessionEvent::AgentSettled));
    assert!(matches!(events[1], AgentSessionEvent::SessionAbort));
}

#[tokio::test]
async fn impossible_model_switch_leaves_active_model_and_history_unchanged() {
    let session = test_session_with_stream_function(false);
    session.agent.set_model(test_model());
    let mut model = test_model();
    model.id = "tiny".to_owned();
    model.context_window = 4_096;
    let revision = session.message_revision();
    assert!(session.set_model(model).await.is_err());
    assert_eq!(session.message_revision(), revision);
    assert_eq!(session.model().id, "faux-1");
    // senpi `appendModelChangeRejected`: the rejection is the only durable record; no message is added.
    let kinds: Vec<String> = session
        .with_session_manager(|manager| manager.entries())
        .iter()
        .map(|entry| entry["type"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(kinds, ["model_change_rejected"]);
}

#[tokio::test]
async fn oversized_transcript_holds_repairable_switch_without_durable_change() {
    let session = keyed_test_session();
    session.agent.set_model(test_model());
    session
        .agent
        .set_messages(vec![make_user_message(&"x".repeat(200_000), None)]);
    let mut model = test_model();
    model.id = "smaller".to_owned();
    model.context_window = 64_000;
    let revision = session.message_revision();
    session.set_model(model).await.expect("held switch");
    assert_eq!(session.message_revision(), revision);
    assert_eq!(session.model().id, "faux-1");
    assert_eq!(
        session.pending_model_switch().expect("pending").model.id,
        "smaller"
    );
    assert!(
        session
            .with_session_manager(|manager| manager.entries())
            .is_empty()
    );
}

#[tokio::test]
async fn equal_window_switch_accounts_for_smaller_usable_context() {
    let session = keyed_test_session();
    session.agent.set_model(test_model());
    let (budget, _) = session
        .model_budget(&session.model(), 0, false)
        .expect("budget");
    let available = budget.context_window - budget.required_tokens;
    session.agent.set_messages(vec![make_user_message(
        &"x".repeat((available * 4 - 8_000) as usize),
        None,
    )]);
    let mut model = test_model();
    model.id = "same-window-larger-output".to_owned();
    model.max_tokens += 8_000;
    session.set_session_model(model).await.expect("held switch");
    assert_eq!(session.model().id, "faux-1");
    assert_eq!(
        session.pending_model_switch().expect("pending").model.id,
        "same-window-larger-output"
    );
    assert!(
        session
            .with_session_manager(|manager| manager.entries())
            .is_empty()
    );
}

#[test]
fn compaction_application_rebuilds_only_summary_and_retained_suffix() {
    let session = test_session();
    session.agent.set_model(test_model());
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"old","timestamp":0}))
    });
    let retained = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"recent","timestamp":1}))
    });
    session
        .apply_compaction(&crate::compaction::compaction::CompactionResult {
            summary: "summary".to_owned(),
            first_kept_entry_id: retained["id"].as_str().expect("id").to_owned(),
            tokens_before: 100,
            estimated_tokens_after: None,
            usage: None,
            details: None,
        })
        .expect("apply");
    assert_eq!(session.messages().len(), 2);
    assert_eq!(user_message_text(&session.messages()[1]), "recent");
    assert_eq!(
        session.with_session_manager(|manager| {
            manager.entries().last().expect("entry")["type"].clone()
        }),
        "compaction"
    );
}

#[test]
fn compaction_retention_uses_selected_leaf_instead_of_last_appended_branch() {
    let session = test_session();
    session.agent.set_model(test_model());
    let selected = session.with_session_manager_mut(|manager| {
        manager
            .append_message(serde_json::json!({"role":"user","content":"selected","timestamp":0}))
    });
    let abandoned = session.with_session_manager_mut(|manager| {
        manager
            .append_message(serde_json::json!({"role":"user","content":"abandoned","timestamp":1}))
    });
    session
        .with_session_manager_mut(|manager| manager.set_leaf(selected["id"].as_str()))
        .expect("leaf");
    let result = crate::compaction::compaction::CompactionResult {
        summary: "summary".to_owned(),
        first_kept_entry_id: abandoned["id"].as_str().expect("abandoned").to_owned(),
        tokens_before: 100,
        estimated_tokens_after: None,
        usage: None,
        details: None,
    };
    assert_eq!(
        session.apply_compaction(&result).expect_err("off branch"),
        "Compaction first kept entry is not on the current branch"
    );
    assert_eq!(
        session
            .with_session_manager(|manager| manager.entries())
            .len(),
        2
    );
    let context = SessionContextManager::new(&session);
    assert_eq!(maho_ext_api::SessionManager::get_branch(&context).len(), 1);
    session
        .apply_compaction(&crate::compaction::compaction::CompactionResult {
            first_kept_entry_id: selected["id"].as_str().expect("selected").to_owned(),
            ..result
        })
        .expect("selected branch compaction");
    assert_eq!(user_message_text(&session.messages()[1]), "selected");
}

#[test]
fn oversized_compaction_is_rejected_without_mutating_history() {
    let session = test_session();
    session.agent.set_model(test_model());
    let retained = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"recent","timestamp":1}))
    });
    let revision = session.message_revision();
    let error = session
        .apply_compaction(&crate::compaction::compaction::CompactionResult {
            summary: "x".repeat(600_000),
            first_kept_entry_id: retained["id"].as_str().expect("id").to_owned(),
            tokens_before: 100,
            estimated_tokens_after: None,
            usage: None,
            details: None,
        })
        .expect_err("overflow rejected");
    assert_eq!(error, "Compaction rejected: summary-would-overflow");
    assert_eq!(session.message_revision(), revision);
    assert_eq!(
        session.with_session_manager(|manager| manager.entries().len()),
        1
    );
}

#[test]
fn invalid_compaction_retention_does_not_mutate_history() {
    let session = test_session();
    assert!(
        session
            .apply_compaction(&crate::compaction::compaction::CompactionResult {
                summary: "summary".to_owned(),
                first_kept_entry_id: "missing".to_owned(),
                tokens_before: 100,
                estimated_tokens_after: None,
                usage: None,
                details: None,
            })
            .is_err()
    );
    assert!(
        session
            .with_session_manager(|manager| manager.entries())
            .is_empty()
    );
}

#[tokio::test]
async fn editing_user_message_preserves_old_branch_and_rejects_stale_leaf() {
    let session = test_session();
    let entry = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"old","timestamp":0}))
    });
    let id = entry["id"].as_str().expect("id");
    assert!(
        session
            .edit_user_message(
                id,
                "changed",
                TreeNavigationOptions {
                    expected_leaf_id: Some("stale".to_owned()),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    let result = session
        .edit_user_message(id, "changed", Default::default())
        .await
        .expect("edit");
    assert!(result.entry_id.is_some());
    assert_eq!(
        session.with_session_manager(|manager| manager.entries().len()),
        2
    );
    assert_eq!(user_message_text(&session.messages()[0]), "changed");
    session
        .navigate_tree(
            id,
            TreeNavigationOptions {
                intent: Some(TreeNavigationIntent::Resume),
                ..Default::default()
            },
        )
        .await
        .expect("resume");
    assert_eq!(user_message_text(&session.messages()[0]), "old");
}

#[tokio::test]
async fn unchanged_message_edits_reject_stale_leaf_and_append_nothing() {
    let session = test_session();
    let user = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({"role":"user","content":"same","timestamp":0}))
    });
    let assistant = maho_ai::providers::faux::faux_assistant_message("reply", Default::default());
    let reply = session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::to_value(&assistant).expect("message"))
    });
    let user_id = user["id"].as_str().expect("id");
    let reply_id = reply["id"].as_str().expect("id");
    let stale = TreeNavigationOptions {
        expected_leaf_id: Some("stale".to_owned()),
        ..Default::default()
    };
    assert!(
        session
            .edit_assistant_message(reply_id, "reply", stale.clone())
            .await
            .is_err()
    );
    assert!(
        session
            .edit_user_message(user_id, "same", stale)
            .await
            .is_err()
    );
    assert_eq!(
        session
            .edit_assistant_message(reply_id, "reply", Default::default())
            .await
            .expect("unchanged")
            .unchanged,
        Some(true)
    );
    assert_eq!(
        session
            .edit_user_message(user_id, "same", Default::default())
            .await
            .expect("unchanged")
            .unchanged,
        Some(true)
    );
    assert_eq!(
        session.with_session_manager(|manager| manager.entries().len()),
        2
    );
    assert_eq!(
        session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)),
        Some(reply_id.to_owned())
    );
}

/// senpi `onBeforeSessionReplace`: an in-place replacement runs the listener once, while the outgoing identity is
/// still current; a removed listener does not run.
#[tokio::test]
async fn before_session_replace_runs_once_per_replacement_on_the_outgoing_session() {
    let session = test_session();
    let id = session.session_id();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let weak = session.weak_accessor();
    let record = seen.clone();
    let listener = session.on_before_session_replace(Arc::new(move || {
        if let Some(session) = weak() {
            lock(&record).push(session.session_id());
        }
    }));
    assert!(session.new_session(None).await.expect("new"));
    assert_eq!(*lock(&seen), [id]);
    session.remove_before_session_replace(listener);
    assert!(session.new_session(None).await.expect("new"));
    assert_eq!(lock(&seen).len(), 1);
}

#[tokio::test]
async fn new_session_clears_context_and_changes_identity() {
    let session = test_session();
    let id = session.session_id();
    session
        .agent
        .set_messages(vec![make_user_message("old", None)]);
    assert!(session.new_session(None).await.expect("new"));
    assert_ne!(session.session_id(), id);
    assert!(session.messages().is_empty());
}

#[test]
fn enabled_hook_sources_preserve_scope_order_and_session_directory() {
    let session = test_session();
    let resource = |name: &str, scope, enabled| crate::package_manager::ResolvedResource {
        path: name.into(),
        enabled,
        metadata: crate::package_manager::PathMetadata {
            scope,
            base_dir: Some(session.cwd().to_owned()),
            ..Default::default()
        },
    };
    session.set_hook_source_paths(
        vec![
            resource("global.json", crate::source_info::SourceScope::User, true),
            resource(
                "project.json",
                crate::source_info::SourceScope::Project,
                true,
            ),
            resource("ignored.json", crate::source_info::SourceScope::User, false),
            resource("global.json", crate::source_info::SourceScope::User, true),
        ],
        vec!["extra.json".into(), "extra.json".into()],
    );
    let context = crate::sdk::extension_context::create(&session);
    let sources = context.get_loaded_hook_sources().expect("hook sources");
    assert_eq!(sources.global_hook_source_paths.len(), 1);
    assert_eq!(sources.project_hook_source_paths.len(), 1);
    assert_eq!(sources.pre_session_hook_source_paths.len(), 1);
    assert_eq!(
        context.session_manager.get_session_dir(),
        Some(session.with_session_manager(|manager| manager.session_dir().into()))
    );
}

#[test]
fn prompt_expansion_uses_template_arguments_and_can_be_disabled() {
    let session = test_session();
    session.set_prompt_resources(
        vec![crate::prompt_templates::PromptTemplate {
            name: "review".to_owned(),
            description: String::new(),
            argument_hint: None,
            requires_arguments: false,
            content: "review $1".to_owned(),
            source_info: crate::source_info::create_synthetic_source_info(
                "/tmp/review.md",
                crate::source_info::SyntheticSourceInfoOptions::default(),
            ),
            file_path: "/tmp/review.md".to_owned(),
        }],
        Vec::new(),
    );
    assert_eq!(
        session.expand_input("/review file", true).expect("expand"),
        "review file"
    );
    assert_eq!(
        session.expand_input("/review file", false).expect("raw"),
        "/review file"
    );
    session
        .state()
        .extension_commands
        .push(maho_ext_api::SlashCommandInfo {
            name: "extension".into(),
            description: Some("extension command".into()),
            argument_hint: Some("target".into()),
            source_info: None,
        });
    session.state().skills.push(crate::skills::Skill {
        name: "guide".into(),
        description: "guide skill".into(),
        file_path: "/tmp/guide/SKILL.md".into(),
        base_dir: "/tmp/guide".into(),
        source_info: crate::source_info::create_synthetic_source_info(
            "/tmp/guide/SKILL.md",
            crate::source_info::SyntheticSourceInfoOptions::default(),
        ),
        disable_model_invocation: true,
        provenance: None,
        allowed_set: None,
    });
    let commands = session.get_commands();
    assert_eq!(
        commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["extension", "review", "skill:guide"]
    );
    assert_eq!(commands[0].argument_hint.as_deref(), Some("target"));
    assert_eq!(
        commands[1].source_info.as_ref().unwrap().path,
        "/tmp/review.md"
    );
    assert_eq!(
        commands[2].source_info.as_ref().unwrap().path,
        "/tmp/guide/SKILL.md"
    );
    let live = Arc::new(std::sync::Mutex::new(Vec::new()));
    let catalog = live.clone();
    session.state().extension_command_catalog =
        Some(Arc::new(move || catalog.lock().unwrap().clone()));
    live.lock().unwrap().push(maho_ext_api::SlashCommandInfo {
        name: "late".into(),
        description: None,
        argument_hint: None,
        source_info: None,
    });
    assert_eq!(
        session
            .get_commands()
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["late", "review", "skill:guide"]
    );
}

#[test]
fn command_groups_keep_skill_names_raw_for_rpc_consumers() {
    let session = test_session();
    session.state().skills.push(crate::skills::Skill {
        name: "guide".into(),
        description: "guide skill".into(),
        file_path: "/tmp/guide/SKILL.md".into(),
        base_dir: "/tmp/guide".into(),
        source_info: crate::source_info::create_synthetic_source_info(
            "/tmp/guide/SKILL.md",
            Default::default(),
        ),
        disable_model_invocation: true,
        provenance: None,
        allowed_set: None,
    });
    let (_, _, skills) = session.command_groups();
    assert_eq!(
        skills
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["guide"]
    );
    assert_eq!(
        session
            .get_commands()
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["skill:guide"]
    );
}

#[tokio::test]
async fn loaded_skills_returns_the_session_snapshot_with_diagnostics() {
    let session = test_session();
    assert!(
        session.loaded_skills().is_none(),
        "no skill cache before a load"
    );
    session.set_prompt_resources_with_diagnostics(
        Vec::new(),
        vec![crate::skills::Skill {
            name: "demo".to_owned(),
            description: "demo skill".to_owned(),
            file_path: "/tmp/demo/SKILL.md".to_owned(),
            base_dir: "/tmp/demo".to_owned(),
            source_info: crate::source_info::create_synthetic_source_info(
                "/tmp/demo/SKILL.md",
                Default::default(),
            ),
            disable_model_invocation: false,
            provenance: None,
            allowed_set: None,
        }],
        vec![crate::diagnostics::ResourceDiagnostic {
            category: None,
            diagnostic_type: crate::diagnostics::ResourceDiagnosticType::Warning,
            message: "warn".to_owned(),
            path: Some("/tmp/demo/SKILL.md".to_owned()),
            collision: None,
        }],
    );
    let loaded = session.loaded_skills().expect("loaded skills snapshot");
    assert_eq!(loaded.skills.len(), 1);
    assert_eq!(loaded.skills[0].name, "demo");
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(loaded.diagnostics[0].message, "warn");
    session.dispose().await;
}

#[tokio::test]
async fn security_notices_follow_resource_replacement_runtime_removal_and_disposal() {
    let session = test_session();
    let diagnostic = crate::diagnostics::ResourceDiagnostic {
        category: Some("skill-link-blocked".into()),
        diagnostic_type: crate::diagnostics::ResourceDiagnosticType::Warning,
        message: "blocked".into(),
        path: Some("/project/skill".into()),
        collision: None,
    };
    session.set_prompt_resources_with_diagnostics(vec![], vec![], vec![diagnostic]);
    assert_eq!(session.security_notice_state().notices.len(), 1);
    let runner = ExtensionRunner::new(
        Vec::new(),
        Default::default(),
        maho_ext_api::EventBus::default(),
        test_extension_context(&session),
    );
    let events = runner.events.clone();
    session.set_extension_runner(runner).await;
    events.emit(
        "security_notice",
        &serde_json::json!({
            "category":"mcp-env-withheld", "path":"server", "message":"withheld",
        }),
    );
    assert_eq!(session.security_notice_state().notices.len(), 2);
    session.set_prompt_resources(vec![], vec![]);
    assert_eq!(session.security_notice_state().notices.len(), 1);
    session.accept_security_notice(&serde_json::json!({
        "category":"mcp-env-withheld", "path":"server", "message":"", "remove":true,
    }));
    assert!(session.security_notice_state().notices.is_empty());
    session.accept_security_notice(&serde_json::json!({
        "category":"skill-link-blocked", "message":"runtime blocked",
    }));
    session.dispose().await;
    assert!(session.security_notice_state().notices.is_empty());
}

#[tokio::test]
async fn blocked_project_skill_expansion_publishes_notice_without_reading_outside_body() {
    let session = test_session();
    let project = tempfile::tempdir().expect("project");
    let outside = tempfile::tempdir().expect("outside");
    let path = outside.path().join("SKILL.md");
    std::fs::write(&path, "---\nname: blocked\n---\nPrivate outside body").unwrap();
    session.state().skills.push(crate::skills::Skill {
        name: "blocked".into(),
        file_path: path.to_string_lossy().into_owned(),
        base_dir: outside.path().to_string_lossy().into_owned(),
        provenance: Some(crate::skill_access::SkillProvenance {
            kind: crate::skill_access::SkillProvenanceKind::ProjectAuto,
            selected_root: project.path().to_string_lossy().into_owned(),
            project_root: Some(project.path().to_string_lossy().into_owned()),
        }),
        ..Default::default()
    });
    assert_eq!(
        session.expand_input("$blocked task", false).unwrap(),
        "$blocked task"
    );
    let notices = session.security_notice_state().notices;
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].category,
        crate::security_notices::SecurityNoticeCategory::SkillLinkBlocked
    );
    assert_eq!(notices[0].path.as_deref(), path.to_str());
    session.dispose().await;
}

#[tokio::test]
async fn skill_expansion_caps_unique_skills_and_preserves_unexpanded_tokens() {
    let session = test_session();
    let dir = tempfile::tempdir().expect("directory");
    let skills = (0..6)
        .map(|index| {
            let name = format!("skill{index}");
            let path = dir.path().join(format!("{name}.md"));
            std::fs::write(
                &path,
                format!("---\nname: {name}\ndescription: test\n---\nbody {index}"),
            )
            .expect("skill");
            crate::skills::Skill {
                name,
                description: "test".to_owned(),
                file_path: path.to_string_lossy().into_owned(),
                base_dir: dir.path().to_string_lossy().into_owned(),
                disable_model_invocation: false,
                provenance: None,
                allowed_set: None,
                source_info: crate::source_info::create_synthetic_source_info(
                    &path.to_string_lossy(),
                    Default::default(),
                ),
            }
        })
        .collect();
    session.set_prompt_resources(Vec::new(), skills);
    let observed = Arc::new(Mutex::new(Vec::<ExtensionError>::new()));
    let sink = Arc::clone(&observed);
    session.state().extension_error_listener =
        Some(Arc::new(move |error| lock(&sink).push(error.clone())));
    let expanded = session
        .expand_input("$skill0 $skill1 $skill2 $skill3 $skill4 $skill5 task", true)
        .expect("expand");
    assert_eq!(expanded.matches("<skill-instruction name=").count(), 5);
    assert!(expanded.contains("$skill5"));
    assert_eq!(
        lock(&observed)
            .last()
            .map(|error| error.extension_path.clone()),
        Some("skill:expansion".to_owned())
    );
    let duplicate = session
        .expand_input("$skill0 $skill0 task", true)
        .expect("duplicate");
    assert_eq!(duplicate.matches("<skill-instruction name=").count(), 1);
    assert_eq!(
        lock(&observed).last().map(|error| error.error.clone()),
        Some("Skipped duplicate skill invocation: skill0".to_owned())
    );
    std::fs::write(
        dir.path().join("skill1.md"),
        "---\nname: [unterminated\n---\nbody",
    )
    .expect("bad skill");
    assert_eq!(
        session.expand_input("$skill1 task", true).expect("lenient"),
        "$skill1 task"
    );
    assert_eq!(
        lock(&observed).last().map(|error| error.event.clone()),
        Some("skill_expansion".to_owned())
    );
    let unknown = session
        .expand_input("/skill:missing /skill:skill0 task", true)
        .expect("unknown");
    assert!(!unknown.contains("<skill-instruction name="));
    assert!(unknown.contains("/skill:skill0"));
}

#[test]
fn discovered_resources_merge_once_and_keep_extension_metadata() {
    let session = test_session();
    let dir = tempfile::tempdir().expect("directory");
    let prompt = dir.path().join("discovered.md");
    let skill = dir.path().join("skill.md");
    std::fs::write(
        &prompt,
        "---\ndescription: discovered prompt\n---\nreview $1",
    )
    .expect("prompt fixture");
    std::fs::write(
        &skill,
        "---\nname: discovered-skill\ndescription: discovered skill\n---\nskill body",
    )
    .expect("skill fixture");
    let entry = |path: &std::path::Path| maho_ext_api::DiscoveredResourceEntry {
        path: path.to_string_lossy().into_owned(),
        extension_path: "<inline:resources>".to_owned(),
        scope: Some(maho_ext_api::SourceScope::System),
    };
    let resources = maho_ext_api::DiscoveredResources {
        prompt_paths: vec![entry(&prompt)],
        skill_paths: vec![entry(&skill)],
        hook_paths: vec![entry(&dir.path().join("hooks.json"))],
        ..Default::default()
    };
    session.extend_discovered_resources(resources.clone());
    session.extend_discovered_resources(resources);
    let templates = session.prompt_templates();
    assert_eq!(templates.len(), 1);
    assert_eq!(
        templates[0].source_info.scope,
        crate::source_info::SourceScope::System
    );
    assert_eq!(
        templates[0].source_info.source,
        "extension:inline:resources"
    );
    assert_eq!(
        session
            .expand_input("/discovered file", true)
            .expect("prompt expansion"),
        "review file"
    );
    let state = session.state();
    assert_eq!(state.skills.len(), 1);
    assert_eq!(
        state.skills[0].source_info.scope,
        crate::source_info::SourceScope::System
    );
    assert_eq!(state.discovered_resources.hook_paths.len(), 1);
    drop(state);
    let sources = maho_ext_api::ExtensionContextActions::get_loaded_hook_sources(
        &SessionExtensionActions(Arc::downgrade(&session.inner)),
    );
    assert_eq!(
        sources.runtime_hook_source_paths,
        vec![dir.path().join("hooks.json")]
    );
    session.set_hook_sources(Some(sources));
    session.extend_discovered_resources(maho_ext_api::DiscoveredResources {
        hook_paths: vec![entry(&dir.path().join("later-hooks.json"))],
        ..Default::default()
    });
    let combined = maho_ext_api::ExtensionContextActions::get_loaded_hook_sources(
        &SessionExtensionActions(Arc::downgrade(&session.inner)),
    );
    assert_eq!(
        combined.runtime_hook_source_paths,
        vec![
            dir.path().join("hooks.json"),
            dir.path().join("later-hooks.json")
        ]
    );
    std::fs::write(&prompt, "---\ndescription: updated prompt\n---\nupdated $1")
        .expect("updated fixture");
    session.extend_discovered_resources(maho_ext_api::DiscoveredResources {
        prompt_paths: vec![entry(&prompt)],
        ..Default::default()
    });
    assert_eq!(session.prompt_templates().len(), 1);
    assert_eq!(
        session
            .expand_input("/discovered file", true)
            .expect("updated expansion"),
        "updated file"
    );
    let mut updated = entry(&prompt);
    updated.scope = Some(maho_ext_api::SourceScope::Project);
    session.extend_discovered_resources(maho_ext_api::DiscoveredResources {
        prompt_paths: vec![updated],
        ..Default::default()
    });
    assert_eq!(
        session.prompt_templates()[0].source_info.scope,
        crate::source_info::SourceScope::Project
    );
    let revision = session.message_revision();
    session.state().system_prompt_override = Some("active turn prompt".to_owned());
    session
        .agent
        .set_system_prompt("active turn prompt".to_owned());
    session.extend_discovered_resources(Default::default());
    session.extend_discovered_resources(maho_ext_api::DiscoveredResources {
        hook_paths: vec![entry(&dir.path().join("other-hooks.json"))],
        ..Default::default()
    });
    assert_eq!(session.system_prompt(), "active turn prompt");
    assert_eq!(session.message_revision(), revision);
}

#[tokio::test]
async fn native_user_bash_hook_result_is_recorded_without_host_command_execution() {
    let session = test_session();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:user-bash>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::UserBash,
        vec![Arc::new(|_, _| {
            Box::pin(async {
                Ok(maho_ext_api::EventResult::UserBash {
                    operations: None,
                    result: Some(maho_tools::bash_executor::BashResult {
                        output: "hook result".into(),
                        exit_code: Some(0),
                        cancelled: false,
                        truncated: false,
                        full_output_path: None,
                    }),
                })
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let result = session.execute_user_bash("exit 99", None, true, None).await;
    let messages = session.messages();
    session.dispose().await;
    let result = result.expect("hook result");
    assert_eq!(result.output, "hook result");
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role(), "bashExecution");
}

#[tokio::test]
async fn late_subscriber_receives_replays_once_before_live_events() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_string_lossy().into_owned();
    let agent_dir = tmp.path().join("agent").to_string_lossy().into_owned();
    std::fs::create_dir_all(&agent_dir).expect("create agent dir");
    std::fs::write(tmp.path().join("agent").join("settings.json"), "{}").expect("write settings");
    let settings = SettingsManager::create(&cwd, &agent_dir, &cwd, false);
    let runtime = ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()),
        ..Default::default()
    })
    .expect("valid model runtime");
    let session = AgentSession::new(AgentSessionConfig {
        agent: stub_agent(),
        session_manager: SessionManager::in_memory(&cwd, None, None),
        settings_manager: settings,
        cwd: cwd.clone(),
        agent_dir: Some(agent_dir),
        fallback_now: None,
        retry_random: None,
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: BTreeMap::new(),
        custom_tools: Vec::new(),
        model_runtime: Some(runtime),
        model_registry: None,
        uses_default_stream_function: Some(true),
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: None,
    })
    .expect("session");

    session.admit_resume_compaction_required(
        serde_json::json!({"usable": false, "admission": "resume"}),
        "compacting before first prompt".to_owned(),
    );
    session.apply_resume_slice(ResumeSliceState {
        tokens_before: 10000,
        tokens_after: 5000,
        dropped_entries: 10,
        notice: "reduced context".to_owned(),
    });

    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let reentrant_session = session.clone();
    let _sub = session.subscribe(Arc::new(move |event| {
        lock(&observed).push(event.clone());
        if matches!(event, AgentSessionEvent::ResumeCompactionRequired { .. }) {
            reentrant_session.emit(AgentSessionEvent::AgentIdle);
        }
    }));

    let captured = lock(&events).clone();
    assert!(
        captured
            .iter()
            .any(|e| matches!(e, AgentSessionEvent::ResumeCompactionRequired { .. }))
    );
    let resume_index = captured
        .iter()
        .position(|e| matches!(e, AgentSessionEvent::ResumeCompactionRequired { .. }))
        .expect("resume replay");
    assert!(matches!(
        captured.get(resume_index + 1),
        Some(AgentSessionEvent::AgentIdle)
    ));
    assert!(
        captured
            .iter()
            .any(|e| matches!(e, AgentSessionEvent::ResumeContextReduced { .. }))
    );
    assert!(
        captured
            .iter()
            .any(|e| matches!(e, AgentSessionEvent::SettingsSourceSelected { .. }))
    );

    // Emit a live event and verify it is received after replays
    session.emit(AgentSessionEvent::AgentIdle);
    let final_events = lock(&events).clone();
    assert!(matches!(
        final_events.last(),
        Some(AgentSessionEvent::AgentIdle)
    ));
}

#[tokio::test]
async fn registered_tool_context_disposer_runs_once_on_dispose() {
    use std::sync::atomic::AtomicUsize;
    let session = test_session();
    let ran = Arc::new(AtomicUsize::new(0));
    let ran_clone = ran.clone();
    session.register_tool_context_disposer(move || {
        ran_clone.fetch_add(1, Ordering::SeqCst);
    });

    assert_eq!(ran.load(Ordering::SeqCst), 0);
    session.dispose().await;
    assert_eq!(ran.load(Ordering::SeqCst), 1);

    // Second dispose must not run the disposer again
    session.dispose().await;
    assert_eq!(ran.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dispose_is_idempotent_and_cleans_resources() {
    let session = test_session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let _sub = session.subscribe(Arc::new(move |event| {
        lock(&observed).push(event.clone());
    }));

    assert!(!session.state().disposed);
    session.dispose().await;
    assert!(session.state().disposed);
    assert_eq!(session.inner.event_listeners.lock().unwrap().len(), 0);

    // Second dispose should be a no-op, no panic
    session.dispose().await;
    assert!(session.state().disposed);

    // Subscribing after dispose works without panic
    let sub2 = session.subscribe(Arc::new(|_| {}));
    drop(sub2);
}

#[tokio::test]
async fn dispose_aborts_running_bash_and_removes_listeners() {
    let session = test_session();
    let root = tempfile::tempdir().expect("tempdir");
    let fifo = root.path().join("blocked-input");
    let (started, ready) = tokio::sync::oneshot::channel();
    let started = Arc::new(Mutex::new(Some(started)));
    let callback: maho_tools::bash_executor::BashChunkCallback = Arc::new(move |chunk| {
        if chunk.contains("BASH_READY")
            && let Some(sender) = lock(&started).take()
        {
            let _ = sender.send(());
        }
        Ok(())
    });
    let _subscription = session.subscribe(Arc::new(|_| {}));
    let command = format!(
        "mkfifo '{}' && printf BASH_READY && read -r value < '{}'",
        fifo.display(),
        fifo.display()
    );
    let execution = session.execute_bash(&command, Some(callback), true, None, None);
    let disposal = async {
        ready.await.expect("bash startup signal");
        assert!(session.is_bash_running());
        session.dispose().await;
        assert!(lock(&session.inner.event_listeners).is_empty());
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(execution, disposal)
    })
    .await
    .expect("running bash must settle after disposal");
    assert!(result.expect("cancelled bash result").cancelled);
    assert!(!session.is_bash_running());
}

mod compaction_log_attempts {
    use super::*;

    fn fixture() -> (tempfile::TempDir, AgentSession) {
        fixture_with_settings(SettingsManager::from_storage(
            Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
            false,
        ))
    }

    fn fixture_with_settings(
        settings_manager: SettingsManager,
    ) -> (tempfile::TempDir, AgentSession) {
        let root = tempfile::tempdir().expect("log fixture");
        let runtime = ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
            providers: Some(Vec::new()),
            ..Default::default()
        })
        .expect("valid model runtime");
        let session = AgentSession::new(AgentSessionConfig {
            agent: stub_agent(),
            session_manager: SessionManager::in_memory("/tmp", None, None),
            settings_manager,
            cwd: "/tmp".to_owned(),
            agent_dir: Some(root.path().to_string_lossy().into_owned()),
            fallback_now: None,
            retry_random: None,
            scoped_models: Vec::new(),
            favorite_models: Vec::new(),
            flag_values: BTreeMap::new(),
            custom_tools: Vec::new(),
            model_runtime: Some(runtime),
            model_registry: None,
            uses_default_stream_function: Some(false),
            initial_active_tool_names: None,
            default_tool_names: None,
            eval_only_tool_names: None,
            allowed_tool_names: None,
            excluded_tool_names: None,
            base_tools_override: None,
            session_start_event: None,
            auto_title_sessions: None,
        })
        .expect("session");
        (root, session)
    }

    #[test]
    fn malformed_raw_fallback_chain_logs_startup_validation_warning() {
        let mut settings = SettingsManager::from_storage(
            Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
            false,
        );
        settings.apply_overrides(&Map::from_iter([(
            "retry".to_owned(),
            serde_json::json!({
                "enabled":true, "fallbackChains":{"faux/faux-1":"not-an-array"}
            }),
        )]));
        let (root, session) = fixture_with_settings(settings);
        assert!(!session.fallback_validation_warnings().is_empty());
        let rows: Vec<Value> = std::fs::read_to_string(root.path().join("logs/fallback.log"))
            .expect("fallback log")
            .lines()
            .map(|line| serde_json::from_str(line).expect("log JSON"))
            .collect();
        assert!(rows.iter().any(|row| row["event"] == "validation_warning"));
    }

    fn decisions(root: &tempfile::TempDir) -> Vec<Value> {
        std::fs::read_to_string(root.path().join("logs/session.log"))
            .expect("session log")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("log JSON"))
            .filter(|entry| entry["event"] == "compaction_decision")
            .collect()
    }

    fn start(session: &AgentSession, id: &str, reason: maho_ext_api::CompactionReason) {
        session.emit(AgentSessionEvent::CompactionStart {
            reason,
            request_id: Some(id.to_owned()),
        });
    }

    fn end(session: &AgentSession, id: Option<&str>, accepted: bool) {
        session.emit(AgentSessionEvent::CompactionEnd {
            reason: maho_ext_api::CompactionReason::Threshold,
            result: None,
            aborted: false,
            will_retry: false,
            request_id: id.map(str::to_owned),
            accepted: Some(accepted),
            rejection_cause: None,
            error_message: (!accepted).then(|| "fixture failure".to_owned()),
        });
    }

    #[test]
    fn retry_exhaustion_does_not_invent_an_attempt() {
        let (root, session) = fixture();
        end(&session, None, false);
        let rows = decisions(&root);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["action"], "none");
        assert_eq!(rows[0]["disposition"], "skipped");
        assert!(rows[0].get("attemptId").is_none());
    }

    #[test]
    fn accepted_end_without_attempt_is_committed() {
        let (root, session) = fixture();
        end(&session, None, true);
        let rows = decisions(&root);
        assert_eq!(rows[0]["action"], "compact");
        assert_eq!(rows[0]["disposition"], "committed");
        assert_eq!(rows[0]["skipped"], false);
        assert!(rows[0].get("attemptId").is_none());
    }

    #[test]
    fn superseded_starts_keep_distinct_attempt_identifiers() {
        let (root, session) = fixture();
        start(&session, "first", maho_ext_api::CompactionReason::Threshold);
        start(&session, "second", maho_ext_api::CompactionReason::Overflow);
        end(&session, Some("second"), false);
        let rows = decisions(&root);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["attemptId"], "first");
        assert_eq!(rows[0]["disposition"], "superseded");
        assert_eq!(rows[1]["attemptId"], "second");
        assert_eq!(rows[1]["disposition"], "failed");
    }

    #[test]
    fn stale_same_reason_end_cannot_commit_newer_attempt() {
        let (root, session) = fixture();
        start(&session, "first", maho_ext_api::CompactionReason::Threshold);
        start(
            &session,
            "second",
            maho_ext_api::CompactionReason::Threshold,
        );
        end(&session, Some("first"), true);
        assert_eq!(
            session
                .state()
                .active_compaction_log_attempt
                .as_ref()
                .expect("active")
                .id,
            "second"
        );
        end(&session, Some("second"), true);
        let rows = decisions(&root);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["disposition"], "superseded");
        assert_eq!(rows[1]["attemptId"], "second");
        assert_eq!(rows[1]["disposition"], "committed");
    }

    #[test]
    fn stale_terminal_is_suppressed_after_more_than_64_supersessions() {
        let (root, session) = fixture();
        for id in 0..66 {
            start(
                &session,
                &id.to_string(),
                maho_ext_api::CompactionReason::Threshold,
            );
        }
        end(&session, Some("0"), true);
        end(&session, Some("65"), true);
        let rows = decisions(&root);
        assert_eq!(rows.len(), 66);
        assert_eq!(rows.iter().filter(|row| row["attemptId"] == "0").count(), 1);
        assert_eq!(rows[0]["disposition"], "superseded");
        assert_eq!(rows.last().expect("latest")["attemptId"], "65");
        assert_eq!(rows.last().expect("latest")["disposition"], "committed");
    }

    #[test]
    fn no_id_exhaustion_does_not_consume_active_attempt() {
        let (root, session) = fixture();
        start(
            &session,
            "active",
            maho_ext_api::CompactionReason::Threshold,
        );
        end(&session, None, false);
        assert_eq!(
            session
                .state()
                .active_compaction_log_attempt
                .as_ref()
                .expect("active")
                .id,
            "active"
        );
        end(&session, Some("active"), true);
        let rows = decisions(&root);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].get("attemptId").is_none());
        assert_eq!(rows[0]["disposition"], "skipped");
        assert_eq!(rows[1]["attemptId"], "active");
        assert_eq!(rows[1]["disposition"], "committed");
    }

    #[test]
    fn committed_attempt_records_active_token_reduction() {
        let (root, session) = fixture();
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({
            "role":"user", "content":[{"type":"text","text":"persisted payload ".repeat(40)}], "timestamp":0
        })));
        start(
            &session,
            "commit",
            maho_ext_api::CompactionReason::Threshold,
        );
        session.agent.set_messages(Vec::new());
        end(&session, Some("commit"), true);
        let rows = decisions(&root);
        assert_eq!(rows[0]["disposition"], "committed");
        assert!(
            rows[0]["tokensAfter"].as_u64().expect("after")
                < rows[0]["tokensBefore"].as_u64().expect("before")
        );
        assert!(session.state().active_compaction_log_attempt.is_none());
    }

    #[test]
    fn consecutive_threshold_attempts_remain_independently_observable() {
        let (root, session) = fixture();
        for id in ["first", "second"] {
            session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({
                "role":"user", "content":[{"type":"text","text":"persisted payload ".repeat(40)}], "timestamp":0
            })));
            start(&session, id, maho_ext_api::CompactionReason::Threshold);
            session.agent.set_messages(Vec::new());
            end(&session, Some(id), true);
        }
        let rows = decisions(&root);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["attemptId"], "first");
        assert_eq!(rows[1]["attemptId"], "second");
        for row in rows {
            assert_eq!(row["disposition"], "committed");
            assert!(
                row["tokensAfter"].as_u64().expect("after")
                    < row["tokensBefore"].as_u64().expect("before")
            );
        }
        assert!(session.state().active_compaction_log_attempt.is_none());
    }

    #[test]
    fn overflow_rollback_uses_persisted_token_snapshot() {
        let (root, session) = fixture();
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({
            "role":"user", "content":[{"type":"text","text":"persisted payload ".repeat(40)}], "timestamp":0
        })));
        let persisted = session.with_session_manager(|manager| manager.current_context().messages);
        let expected = crate::compaction::estimate_context_tokens(
            &crate::messages::filter_context_excluded_messages(persisted),
        )
        .tokens;
        start(
            &session,
            "rollback",
            maho_ext_api::CompactionReason::Overflow,
        );
        session.emit(AgentSessionEvent::CompactionEnd {
            reason: maho_ext_api::CompactionReason::Overflow,
            request_id: Some("rollback".to_owned()),
            result: None,
            aborted: true,
            will_retry: false,
            accepted: Some(false),
            rejection_cause: Some(maho_ext_api::CompactionRejectionCause::CancelledByExtension),
            error_message: None,
        });
        let rows = decisions(&root);
        assert_eq!(rows[0]["disposition"], "rejected");
        assert_eq!(rows[0]["tokensBefore"], expected);
        assert_eq!(rows[0]["tokensAfter"], expected);
    }
}

#[tokio::test]
async fn auto_compaction_session_override_and_event() {
    let session = test_session();
    assert!(session.auto_compaction_enabled());

    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let _sub = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::SessionSettingsChanged {
            auto_compaction_enabled,
            ..
        } = event
        {
            lock(&observed).push(*auto_compaction_enabled);
        }
    }));

    session.set_auto_compaction_enabled(false);
    assert!(!session.auto_compaction_enabled());
    assert_eq!(*lock(&events), vec![false]);

    session.set_auto_compaction_enabled(true);
    assert!(session.auto_compaction_enabled());
    assert_eq!(*lock(&events), vec![false, true]);
}

#[tokio::test]
async fn auto_compaction_override_false_reflected_in_compaction_preparation() {
    let session = retry_session(Vec::new(), 0);
    session.with_settings_manager_mut(|manager| {
        manager.apply_overrides(&Map::from_iter([
            ("compaction".into(), serde_json::json!({ "enabled": true, "reserveTokens": 0, "reserveScalingEnabled": false, "keepRecentTokens": 1 })),
        ]));
    });
    session.set_auto_compaction_enabled(false);
    assert!(!session.auto_compaction_enabled());

    for text in ["task-1".repeat(200), "task-2".repeat(200)] {
        session.with_session_manager_mut(|manager| {
            manager.append_message(
                serde_json::json!({ "role": "user", "content": text, "timestamp": 0 }),
            );
        });
    }
    session.rebuild_session_context().expect("context");

    let preparation_enabled = Arc::new(Mutex::new(None));
    let captured = preparation_enabled.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:test-compaction-override>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeCompact,
        vec![Arc::new(move |event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else {
                panic!("expected session_before_compact");
            };
            *lock(&captured) = Some(event.preparation.settings.enabled);
            Box::pin(async move {
                Ok(maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(maho_ext_api::CompactionResult {
                            summary: "digest".into(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                            tokens_before: event.preparation.tokens_before,
                            details: None,
                        }),
                        ..Default::default()
                    },
                ))
            })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;

    session.compact(None).await.expect("compact");
    assert_eq!(*lock(&preparation_enabled), Some(false));
}

/// senpi 4106-barrier-on-refused-turn.test.ts `interruptedSession`: a restored session whose run was active and
/// never sealed, so it was interrupted.
fn interrupted_test_session() -> AgentSession {
    let mut manager = SessionManager::in_memory("/tmp", None, None);
    let session_id = manager.session_id().to_owned();
    manager.append_custom(
        "session-lifecycle",
        Some(json!({
            "sessionId": session_id, "state": "running", "seq": 2, "epoch": 1, "epochOrigin": "human",
            "humanInputRevision": 1, "selfSettleEligible": true, "runPaused": false,
            "since": "2026-10-23T00:00:00.000Z", "role": "root", "rootSessionId": session_id,
            "persisted": true, "runActive": true, "epochSealed": false, "runId": "run-1",
        })),
    );
    let session = test_session_with_manager(manager);
    let mut runtime = session.model_runtime().clone();
    runtime
        .register_provider(
            "faux",
            crate::provider_composer::ProviderConfigInput {
                config: crate::model_config_schema::ModelsJsonProvider {
                    api_key: Some("faux-test".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("configured faux provider");
    session.agent.set_model(test_model());
    session
}

/// senpi test/suite/regressions/4204-interrupted-resume.test.ts: opening an interrupted session installs the
/// interrupted-run session hold before session_start producers run, and automatic work is refused until a
/// person continues.
#[tokio::test]
async fn opening_an_interrupted_session_holds_it_for_a_person_before_session_start() {
    let session = interrupted_test_session();
    assert!(session.requires_human_continuation());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observed = seen.clone();
    let weak = Arc::downgrade(&session.inner);
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:interrupted-open>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionStart,
        vec![Arc::new(move |_, _| {
            let session = AgentSession {
                inner: weak.upgrade().expect("session"),
            };
            lock(&observed).extend(
                session
                    .holds()
                    .into_iter()
                    .map(|hold| (hold.kind, hold.key)),
            );
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    session
        .bind_extensions(ExtensionBindings {
            mode: Some(ExtensionMode::Rpc),
            ..Default::default()
        })
        .await;
    assert!(lock(&seen).contains(&(
        crate::engine_turn_hold::HoldKind::SessionHold,
        crate::engine_turn_hold::INTERRUPTED_RUN_HOLD_KEY.to_owned()
    )));
    assert_eq!(
        session.resume_cause(),
        crate::stop_resume_directive::ResumeCause::Interrupted
    );
    // A person's input is the continuation: it admits the next run.
    session.human_input_admitted(HumanInputKind::Prompt);
    assert!(!session.requires_human_continuation());
}

/// senpi test/suite/regressions/4204-interrupted-resume.test.ts (binding restore): consumed custody marks the
/// reopened session interrupted through the production lifecycle host.
#[tokio::test]
async fn binding_restore_marks_consumed_custody_interrupted() {
    let session = Arc::new(test_session());
    let ticket = session
        .with_work_obligations(|registry| {
            let token = registry.open(
                "delivery:fixture",
                crate::work_custody_record::ObligationKind::Background,
                None,
            );
            token.ticket(registry)
        })
        .expect("ticket");
    session
        .with_work_obligations(|registry| registry.consumed(&ticket, "run-1", "entry-1"))
        .expect("consumed");
    assert!(!session.requires_human_continuation());
    let mut binding = session.bind_session_lifecycle();
    binding.restore().await.expect("restore");
    assert!(session.requires_human_continuation());
    assert_eq!(
        session.opening_hold_key(),
        Some(crate::engine_turn_hold::INTERRUPTED_RUN_HOLD_KEY)
    );
}

/// senpi test/suite/regressions/4106-barrier-on-refused-turn.test.ts: a non-person prompt refused after an
/// interrupted run leaves no tool barrier armed.
#[tokio::test]
async fn a_refused_turn_releases_the_tool_barrier_it_armed() {
    let session = interrupted_test_session();
    let calls = Arc::new(AtomicU64::new(0));
    let counted = calls.clone();
    session.agent.set_stream_function(Arc::new(move |_, _, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        maho_ai::types::AssistantMessageEventStream::assistant()
    }));
    let refused = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.prompt(
            "Automatic follow-up",
            PromptOptions {
                source: Some(InputSource::Extension),
                tool_barrier: Some(crate::session_work_barrier::ToolBarrierSpec {
                    id: "turn-1".into(),
                    deadline_ms: 60_000,
                }),
                ..Default::default()
            },
        ),
    )
    .await
    .expect("bounded refusal")
    .expect_err("refused");
    assert!(
        refused.contains("Human input or Resume is required"),
        "{refused}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(session.tool_barrier.id().is_none());
    assert!(!session.release_tool_barrier("turn-1"));
}

/// senpi `_promptAgent` refuses an interrupted run before `onTurnAdmitted`: a refused custom-message turn never
/// claims the released wake it carries, so the wake stays in custody.
#[tokio::test]
async fn a_refused_custom_turn_does_not_claim_its_released_wake() {
    let session = interrupted_test_session();
    let claims = Arc::new(AtomicU64::new(0));
    let counted = claims.clone();
    let admit: ReleasedWakeAdmit = Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        true
    });
    let generation = session.user_abort_generation.load(Ordering::SeqCst);
    let refused = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.trigger_custom_message_turn_with(
            maho_agent::harness::messages::CustomMessage {
                role: "custom".to_owned(),
                custom_type: "wake".to_owned(),
                content: maho_agent::harness::messages::CustomMessageContent::Text(
                    "task done".to_owned(),
                ),
                display: false,
                details: None,
                timestamp: 0,
            },
            None,
            generation,
            Some(admit),
        ),
    )
    .await
    .expect("bounded refusal")
    .expect_err("refused");
    assert_eq!(refused, INTERRUPTED_RUN_REFUSAL);
    assert_eq!(claims.load(Ordering::SeqCst), 0);
}

fn lifecycle_record_count(session: &AgentSession) -> usize {
    session.with_session_manager(|manager| {
        manager
            .entries()
            .iter()
            .filter(|entry| entry["type"] == "custom" && entry["customType"] == "session-lifecycle")
            .count()
    })
}

/// Bash operations that report their start and run until the test releases them.
struct HeldBash {
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}
impl maho_tools::bash::BashOperations for HeldBash {
    fn exec<'a>(
        &'a self,
        _: &'a str,
        _: &'a std::path::Path,
        _: maho_tools::bash::BashExecOptions,
    ) -> maho_tools::definition::ToolFuture<'a, maho_tools::bash::BashExit> {
        Box::pin(async move {
            if let Some(started) = lock(&self.started).take() {
                let _ = started.send(());
            }
            if let Some(release) = self.release.lock().await.take() {
                let _ = release.await;
            }
            Ok(maho_tools::bash::BashExit { exit_code: Some(0) })
        })
    }
}

/// senpi test/suite/regressions/3980-lifecycle-read-only.test.ts: reading the lifecycle while a user shell
/// command runs sees it running and writes nothing; the command's start and end are the lifecycle edges.
#[tokio::test]
async fn reading_the_lifecycle_during_a_user_shell_command_writes_nothing() {
    let session = test_session();
    session.refresh_lifecycle();
    assert_eq!(session.lifecycle_snapshot()["state"], "done");
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let operations = Arc::new(HeldBash {
        started: Mutex::new(Some(started_tx)),
        release: tokio::sync::Mutex::new(Some(release_rx)),
    });
    let bash = session.execute_bash("long command", None, true, None, Some(operations));
    tokio::pin!(bash);
    tokio::select! {
        biased;
        _ = &mut bash => panic!("the held command finished early"),
        started = tokio::time::timeout(std::time::Duration::from_secs(5), started_rx) => {
            started.expect("bounded start").expect("started");
        }
    }
    let before = lifecycle_record_count(&session);
    let reads = [
        session.lifecycle_snapshot()["state"].clone(),
        session.lifecycle_snapshot()["state"].clone(),
        session.lifecycle_snapshot()["state"].clone(),
    ];
    assert_eq!(
        reads,
        [json!("running"), json!("running"), json!("running")]
    );
    assert_eq!(lifecycle_record_count(&session), before);
    release_tx.send(()).expect("release");
    tokio::time::timeout(std::time::Duration::from_secs(5), bash)
        .await
        .expect("bounded bash")
        .expect("bash");
    assert_eq!(session.lifecycle_snapshot()["state"], "done");
    // A fact that changes without its edge stays unread: the getter never recomputes or writes.
    let settled = lifecycle_record_count(&session);
    session
        .state()
        .bash_abort_signals
        .insert("unannounced".into(), Default::default());
    assert_eq!(session.lifecycle_snapshot()["state"], "done");
    assert_eq!(lifecycle_record_count(&session), settled);
}

/// senpi test/suite/regressions/3980-lifecycle-compaction-edges.test.ts: compaction start and end are lifecycle
/// edges, so the state a client reads follows them with nothing else happening, and a read never recomputes.
#[tokio::test]
async fn compaction_start_and_end_are_lifecycle_edges() {
    let session = retry_session(Vec::new(), 0);
    session.refresh_lifecycle();
    assert_eq!(session.lifecycle_snapshot()["state"], "done");
    let actions = session.extension_context_actions();
    actions
        .begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual,
        })
        .expect("feedback signal");
    assert_eq!(session.lifecycle_snapshot()["state"], "running");
    actions.end_compaction(maho_ext_api::EndCompactionOptions {
        reason: maho_ext_api::CompactionReason::Manual,
        signal: None,
        aborted: Some(true),
        error_message: None,
    });
    assert!(!session.is_compacting());
    assert_eq!(session.lifecycle_snapshot()["state"], "done");
}

/// senpi test/suite/regressions/3864-activity-delivery.test.ts and 3864-activity-ordering.test.ts: a run's tool
/// edges project live `activity` on the lifecycle and emit `session_lifecycle` with a new activity revision, but
/// write no durable record, and durable records never carry the activity.
#[tokio::test]
async fn run_activity_is_projected_live_and_never_written() {
    use maho_agent::types::AgentEvent;
    let session = test_session();
    let emitted = Arc::new(Mutex::new(Vec::new()));
    let seen = emitted.clone();
    let _subscription = session.on_extension_event(
        "session_lifecycle",
        Arc::new(move |data| lock(&seen).push(data["lifecycle"].clone())),
    );
    let signal = || maho_ai::utils::abort::AbortController::new().signal();
    session
        .process_agent_event(AgentEvent::AgentStart, signal())
        .await;
    session.refresh_lifecycle();
    assert_eq!(session.lifecycle_snapshot()["state"], "running");
    assert!(session.lifecycle_snapshot().get("activity").is_none());
    let records = lifecycle_record_count(&session);
    let emits = lock(&emitted).len();
    session
        .process_agent_event(
            AgentEvent::ToolExecutionStart {
                tool_call_id: "call-a".into(),
                tool_name: "bash".into(),
                args: Value::Null,
                parent_tool_call_id: None,
            },
            signal(),
        )
        .await;
    let activity = session.lifecycle_snapshot()["activity"].clone();
    assert_eq!(activity["kind"], "tool");
    assert_eq!(activity["toolCallId"], "call-a");
    assert_eq!(activity["outstanding"], 1);
    assert_eq!(lifecycle_record_count(&session), records);
    assert!(lock(&emitted).len() > emits);
    let last = lock(&emitted).last().cloned().expect("activity emit");
    assert_eq!(last["activity"]["toolCallId"], "call-a");
    assert!(last["activityRevision"]["revision"].as_u64() > Some(0));
    session
        .process_agent_event(
            AgentEvent::ToolExecutionEnd {
                tool_call_id: "call-a".into(),
                tool_name: "bash".into(),
                result: Default::default(),
                is_error: false,
                parent_tool_call_id: None,
            },
            signal(),
        )
        .await;
    assert!(session.lifecycle_snapshot().get("activity").is_none());
    assert_eq!(lifecycle_record_count(&session), records);
    assert!(session.with_session_manager(|manager| {
        manager
            .entries()
            .iter()
            .filter(|entry| entry["customType"] == "session-lifecycle")
            .all(|entry| entry["data"].get("activity").is_none())
    }));
}

/// senpi test/suite/regressions/3980-lifecycle-compaction-edges.test.ts: releasing the session-work barrier,
/// and setting or clearing a compaction abort controller, are lifecycle edges; the getter never recomputes, so
/// without them the state a client reads would stay stale.
#[tokio::test]
async fn work_barrier_release_and_compaction_controllers_raise_lifecycle_edges() {
    let session = test_session();
    session.refresh_lifecycle();
    assert_eq!(session.lifecycle_snapshot()["state"], "done");
    let guard = session.work_barrier.begin();
    session.refresh_lifecycle();
    assert_eq!(session.lifecycle_snapshot()["state"], "running");
    drop(guard);
    assert_eq!(session.lifecycle_snapshot()["state"], "done");

    let controller = crate::compaction::lifecycle::CompactionAbortController::new();
    session.set_compaction_abort_controller(Some(controller.clone()));
    assert_eq!(session.lifecycle_snapshot()["state"], "running");
    session.set_compaction_abort_controller(None);
    assert_eq!(session.lifecycle_snapshot()["state"], "done");

    // Only the end of a branch summary is an edge: setting it writes nothing, clearing it settles.
    let records = lifecycle_record_count(&session);
    session
        .set_branch_summary_abort_controller(Some(maho_ai::utils::abort::AbortController::new()));
    assert_eq!(session.lifecycle_snapshot()["state"], "done");
    assert_eq!(lifecycle_record_count(&session), records);
    session.refresh_lifecycle();
    assert_eq!(session.lifecycle_snapshot()["state"], "running");
    session.set_branch_summary_abort_controller(None);
    assert_eq!(session.lifecycle_snapshot()["state"], "done");
}

/// senpi test/suite/regressions/4154-prompt-waits-for-branch-summary.test.ts and
/// 3980-lifecycle-compaction-edges.test.ts: a tree navigation holds the session-work barrier for its whole
/// duration, writes no lifecycle record while it runs, and reads settled once it ends.
#[tokio::test]
async fn tree_navigation_holds_the_work_barrier_and_writes_no_lifecycle_record() {
    let session = test_session();
    let root = session.with_session_manager_mut(|manager| {
        manager.append_message(json!({"role":"user","content":"root","timestamp":0}))
    });
    session.with_session_manager_mut(|manager| {
        manager.append_message(json!({"role":"user","content":"branch","timestamp":1}))
    });
    session.rebuild_session_context().expect("context");
    session.refresh_lifecycle();
    assert_eq!(session.lifecycle_snapshot()["state"], "done");
    let observed = Arc::new(Mutex::new(Vec::new()));
    let weak = Arc::downgrade(&session.inner);
    let seen = observed.clone();
    let mut extension = maho_ext_api::LoadedExtension::new(
        "<inline:tree-barrier>",
        session.cwd().into(),
        Default::default(),
    );
    extension.handlers.insert(
        maho_ext_api::EventKind::SessionBeforeTree,
        vec![Arc::new(move |_, _| {
            let session = AgentSession {
                inner: weak.upgrade().expect("session"),
            };
            lock(&seen).push((
                session.work_barrier.has_active_work(),
                session.is_compacting(),
                lifecycle_record_count(&session),
            ));
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })],
    );
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            test_extension_context(&session),
        ))
        .await;
    let before = lifecycle_record_count(&session);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.navigate_tree(root["id"].as_str().expect("root"), Default::default()),
    )
    .await
    .expect("bounded navigation")
    .expect("navigation");
    assert!(!result.cancelled);
    assert_eq!(*lock(&observed), vec![(true, true, before)]);
    assert!(!session.work_barrier.has_active_work());
    assert!(!session.is_compacting());
    assert_eq!(session.lifecycle_snapshot()["state"], "done");
}

/// senpi test/suite/regressions/2848-sdk-side-cwd.test.ts: the 1-token fallback probe-back request carries the
/// session cwd in its stream options, like every other model request.
#[tokio::test]
async fn the_probe_back_request_carries_the_session_cwd() {
    use maho_ai::providers::faux::{
        FauxModelDefinition, RegisterFauxProviderOptions, faux_assistant_message, faux_provider,
    };
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        models: Some(vec![
            FauxModelDefinition {
                id: "faux-1".into(),
                ..Default::default()
            },
            FauxModelDefinition {
                id: "faux-2".into(),
                ..Default::default()
            },
        ]),
        ..Default::default()
    })
    .expect("valid faux provider");
    provider.set_responses(vec![
        faux_assistant_message("OK", Default::default()).into(),
    ]);
    let session = test_session_with_stream_function(false);
    let mut runtime = session.model_runtime().clone();
    runtime.register_native_provider(provider.provider.clone());
    runtime
        .credentials
        .set("faux", Some(json!({"type":"api_key","key":"faux-test"})))
        .expect("stored auth");
    session
        .agent
        .set_model(provider.get_model(Some("faux-1")).expect("model"));
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = Mutex::new(Some(tx));
    let _subscription = session.subscribe(Arc::new(move |event| {
        if let AgentSessionEvent::RetryProbeResult { ok, .. } = event
            && let Some(tx) = lock(&tx).take()
        {
            let _ = tx.send(*ok);
        }
    }));
    session.arm_probe_back("faux/faux-2", 0.0);
    let ok = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .expect("bounded probe")
        .expect("probe result");
    assert!(ok);
    let log = provider.get_call_log();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].model_id, "faux-2");
    assert_eq!(
        log[0]
            .options
            .as_ref()
            .expect("options")
            .request
            .cwd
            .as_deref(),
        Some(session.cwd().as_str())
    );
}

#[tokio::test]
async fn local_bash_records_native_custom_context_and_history() {
    let session = test_session();
    let result = session
        .execute_bash("printf native", None, true, None, None)
        .await
        .expect("bash");
    assert_eq!(result.output, "native");
    assert_eq!(result.exit_code, Some(0));
    assert!(!session.is_bash_running());
    assert!(!session.has_pending_bash_messages());
    assert_eq!(session.messages()[0].role(), "bashExecution");
    let entry =
        session.with_session_manager(|manager| conversation_entries(manager.entries())[0].clone());
    assert_eq!(entry["message"]["command"], "printf native");
    assert_eq!(entry["message"]["excludeFromContext"], true);
}
