use maho_ext_api::*;
use maho_ext_host::*;
use std::{collections::BTreeMap, path::Path, sync::{Arc, Mutex}};

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
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
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
async fn lifecycle_ui_emits_prompt_pair_and_rejects_stale_prompts() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let runtime = ExtensionRuntime::default();
    let ui = maho_ext_host::ui::LifecycleUi::new(Arc::new(TestUi), runtime.clone(), Arc::new(move |event| observed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event)));
    assert!(!ui.confirm("Confirm", "message", ExtensionUiDialogOptions::default()).await);
    {
        let events = events.lock().unwrap();
        assert!(matches!(&events[0], ExtensionEvent::UiPromptStart { kind: UiPromptKind::Confirm, title: Some(title) } if title == "Confirm"));
        assert!(matches!(&events[1], ExtensionEvent::UiPromptEnd { kind: UiPromptKind::Confirm, title: Some(title) } if title == "Confirm"));
    }
    runtime.invalidate("reloaded");
    assert_eq!(ui.editor("late", None).await.unwrap_err().message, "reloaded");
}
fn extension(path: &str, kind: EventKind, handler: ExtensionHandler) -> LoadedExtension {
    let mut ext = LoadedExtension::new(path, "/tmp".into(), SourceInfo { path: path.into(), source: "inline".into(), ..Default::default() });
    ext.handlers.insert(kind, vec![handler]); ext
}
fn runner(extensions: Vec<LoadedExtension>) -> ExtensionRunner { ExtensionRunner::new(extensions, ExtensionRuntime::default(), EventBus::default(), context()) }
fn before() -> BeforeAgentStartEvent { BeforeAgentStartEvent { prompt: "hello".into(), images: None, system_prompt: "base".into(), system_prompt_options: BuildSystemPromptOptions::default() } }
fn input() -> InputEvent { InputEvent { input_id: "id".into(), text: "X".into(), images: None, source: InputSource::Interactive, streaming_behavior: None } }
fn result_event() -> ToolResultEvent { ToolResultEvent { tool_name: "bash".into(), tool_call_id: "call".into(), input: JsonValue::Null, content: vec![ToolContent::text("base")], details: None, is_error: false, usage: None } }
fn none() -> ExtensionHandler { Arc::new(|_, _| Box::pin(async { Ok(EventResult::None) })) }
fn failure() -> ExtensionHandler { Arc::new(|_, _| Box::pin(async { Err(ExtensionFailure { message: "boom".into(), stack: Some("test-stack".into()) }) })) }
fn transform(suffix: &'static str) -> ExtensionHandler {
    Arc::new(move |event, _| Box::pin(async move {
        if let ExtensionEvent::Input(input) = event { return Ok(EventResult::Input(InputEventResult::Transform { text: format!("{}{suffix}", input.text), images: None })); }
        Err("wrong event".into())
    }))
}
fn prompt(suffix: &'static str) -> ExtensionHandler {
    Arc::new(move |event, ctx| Box::pin(async move {
        let ExtensionEvent::BeforeAgentStart(event) = event else { return Err("wrong event".into()); };
        assert_eq!(ctx.get_system_prompt(), event.system_prompt);
        Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult { system_prompt: Some(format!("{}{suffix}", event.system_prompt)), message: None }))
    }))
}

#[tokio::test]
async fn before_agent_start_chains_in_senpi_order() {
    let mut runner = runner(vec![extension("first", EventKind::BeforeAgentStart, prompt("\nfirst")), extension("second", EventKind::BeforeAgentStart, prompt("\nsecond"))]);
    let merged = runner.emit_before_agent_start(before()).await.unwrap().unwrap();
    assert_eq!(merged.system_prompt.as_deref(), Some("base\nfirst\nsecond"));
    assert!(merged.messages.is_empty());
    println!("systemPrompt={:?}; handlers=first,second; errors={}", merged.system_prompt, runner.errors.len());
}
#[tokio::test]
async fn failing_handler_is_isolated_and_reported() {
    let mut runner = runner(vec![extension("/ext/failing", EventKind::BeforeAgentStart, failure()), extension("good", EventKind::BeforeAgentStart, prompt("\ngood"))]);
    let merged = runner.emit_before_agent_start(before()).await.unwrap().unwrap();
    assert_eq!(merged.system_prompt.as_deref(), Some("base\ngood"));
    assert_eq!(runner.errors.len(), 1);
    assert_eq!(runner.errors[0].event, "before_agent_start");
    assert_eq!(runner.errors[0].stack.as_deref(), Some("test-stack"));
    println!("{}; later handler=good; systemPrompt={:?}", format_extension_error_headline(&runner.errors[0]), merged.system_prompt);
}
#[tokio::test]
async fn before_agent_start_no_changes_returns_none() { assert!(runner(vec![extension("empty", EventKind::BeforeAgentStart, none())]).emit_before_agent_start(before()).await.unwrap().is_none()); }
#[tokio::test]
async fn before_agent_start_collects_messages_in_order() {
    let message = |name: &'static str| -> ExtensionHandler { Arc::new(move |_, _| Box::pin(async move { Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult { message: Some(CustomMessage { custom_type: name.into(), content: vec![], display: true, details: None }), system_prompt: None })) })) };
    let mut runner = runner(vec![extension("a", EventKind::BeforeAgentStart, message("a")), extension("b", EventKind::BeforeAgentStart, message("b"))]);
    assert_eq!(runner.emit_before_agent_start(before()).await.unwrap().unwrap().messages.iter().map(|m| m.custom_type.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
}
#[tokio::test]
async fn multiple_handlers_per_extension_preserve_registration_order() {
    let mut ext = extension("one", EventKind::BeforeAgentStart, prompt("1")); ext.handlers.get_mut(&EventKind::BeforeAgentStart).unwrap().push(prompt("2"));
    assert_eq!(runner(vec![ext]).emit_before_agent_start(before()).await.unwrap().unwrap().system_prompt.as_deref(), Some("base12"));
}
#[tokio::test]
async fn input_no_handlers_returns_continue() { assert_eq!(runner(vec![]).emit_input(input()).await.unwrap(), InputEventResult::Continue); }
#[tokio::test]
async fn input_undefined_return_continues() { assert_eq!(runner(vec![extension("a", EventKind::Input, none())]).emit_input(input()).await.unwrap(), InputEventResult::Continue); }
#[tokio::test]
async fn input_explicit_continue_continues() {
    let handler: ExtensionHandler = Arc::new(|_, _| Box::pin(async { Ok(EventResult::Input(InputEventResult::Continue)) }));
    assert_eq!(runner(vec![extension("a", EventKind::Input, handler)]).emit_input(input()).await.unwrap(), InputEventResult::Continue);
}
#[tokio::test]
async fn input_transform_preserves_images_when_omitted() {
    let mut input = input(); input.images = Some(vec![ImageContent { data: "orig".into(), mime_type: "image/png".into() }]);
    let images = input.images.clone();
    assert_eq!(runner(vec![extension("a", EventKind::Input, transform("1"))]).emit_input(input).await.unwrap(), InputEventResult::Transform { text: "X1".into(), images });
}
#[tokio::test]
async fn input_transform_replaces_images_when_present() {
    let images = vec![ImageContent { data: "new".into(), mime_type: "image/jpeg".into() }]; let expected = images.clone();
    let handler: ExtensionHandler = Arc::new(move |_, _| { let images = images.clone(); Box::pin(async move { Ok(EventResult::Input(InputEventResult::Transform { text: "new".into(), images: Some(images) })) }) });
    assert_eq!(runner(vec![extension("a", EventKind::Input, handler)]).emit_input(input()).await.unwrap(), InputEventResult::Transform { text: "new".into(), images: Some(expected) });
}
#[tokio::test]
async fn input_transforms_chain_across_extensions() {
    assert_eq!(runner(vec![extension("a", EventKind::Input, transform("[1]")), extension("b", EventKind::Input, transform("[2]"))]).emit_input(input()).await.unwrap(), InputEventResult::Transform { text: "X[1][2]".into(), images: None });
}
#[tokio::test]
async fn input_handled_short_circuits() {
    let handled: ExtensionHandler = Arc::new(|_, _| Box::pin(async { Ok(EventResult::Input(InputEventResult::Handled)) }));
    let never: ExtensionHandler = Arc::new(|_, _| Box::pin(async { panic!("subsequent handler ran") }));
    assert_eq!(runner(vec![extension("a", EventKind::Input, handled), extension("b", EventKind::Input, never)]).emit_input(input()).await.unwrap(), InputEventResult::Handled);
}
#[tokio::test]
async fn input_passes_all_sources() {
    for source in [InputSource::Interactive, InputSource::Rpc, InputSource::Extension] {
        let handler: ExtensionHandler = Arc::new(move |event, _| Box::pin(async move { let ExtensionEvent::Input(event) = event else { panic!() }; assert_eq!(event.source, source); Ok(EventResult::None) }));
        let mut input = input(); input.source = source; runner(vec![extension("a", EventKind::Input, handler)]).emit_input(input).await.unwrap();
    }
}
#[tokio::test]
async fn input_passes_streaming_behavior_and_id() {
    for behavior in [None, Some(StreamingBehavior::Steer), Some(StreamingBehavior::FollowUp)] {
        let handler: ExtensionHandler = Arc::new(move |event, _| Box::pin(async move { let ExtensionEvent::Input(event) = event else { panic!() }; assert_eq!(event.streaming_behavior, behavior); assert_eq!(event.input_id, "id"); Ok(EventResult::None) }));
        let mut input = input(); input.streaming_behavior = behavior; runner(vec![extension("a", EventKind::Input, handler)]).emit_input(input).await.unwrap();
    }
}
#[tokio::test]
async fn input_error_continues_to_later_transform() {
    let mut runner = runner(vec![extension("bad", EventKind::Input, failure()), extension("good", EventKind::Input, transform("1"))]);
    assert_eq!(runner.emit_input(input()).await.unwrap(), InputEventResult::Transform { text: "X1".into(), images: None }); assert_eq!(runner.errors[0].error, "boom");
}
#[test]
fn has_handlers_matches_each_registered_event() {
    for kind in EventKind::ALL { let runner = runner(vec![extension("a", kind, none())]); assert!(runner.has_handlers(kind)); }
    assert!(!runner(vec![]).has_handlers(EventKind::Input));
}
#[tokio::test]
async fn generic_emit_isolates_errors_and_notifies_listeners() {
    let seen = Arc::new(Mutex::new(Vec::new())); let capture = Arc::clone(&seen);
    let mut runner = runner(vec![extension("bad", EventKind::AgentStart, failure()), extension("good", EventKind::AgentStart, none())]);
    runner.on_error(Arc::new(move |error| capture.lock().unwrap().push(error.clone())));
    runner.emit(ExtensionEvent::AgentStart).await.unwrap(); assert_eq!(*seen.lock().unwrap(), runner.errors);
}
#[tokio::test]
async fn session_before_cancel_short_circuits() {
    let handler: ExtensionHandler = Arc::new(|_, _| Box::pin(async { Ok(EventResult::SessionBefore(SessionBeforeEventResult { cancel: Some(true), reason: Some("busy".into()), ..Default::default() })) }));
    let mut runner = runner(vec![extension("a", EventKind::SessionBeforeReload, handler), extension("b", EventKind::SessionBeforeReload, failure())]);
    let EventResult::SessionBefore(result) = runner.emit(ExtensionEvent::SessionBeforeReload).await.unwrap() else { panic!() }; assert_eq!(result.reason.as_deref(), Some("busy")); assert!(runner.errors.is_empty());
}
#[tokio::test]
async fn session_before_last_non_cancel_result_wins() {
    let handler = |reason: &'static str| -> ExtensionHandler { Arc::new(move |_, _| Box::pin(async move { Ok(EventResult::SessionBefore(SessionBeforeEventResult { reason: Some(reason.into()), ..Default::default() })) })) };
    let EventResult::SessionBefore(result) = runner(vec![extension("a", EventKind::SessionBeforeReload, handler("a")), extension("b", EventKind::SessionBeforeReload, handler("b"))]).emit(ExtensionEvent::SessionBeforeReload).await.unwrap() else { panic!() }; assert_eq!(result.reason.as_deref(), Some("b"));
}
#[tokio::test]
async fn tool_result_content_chains() {
    let handler = |text: &'static str| -> ExtensionHandler { Arc::new(move |event, _| Box::pin(async move { let ExtensionEvent::ToolResult(event) = event else { panic!() }; let mut content = event.content.clone(); content.push(ToolContent::text(text)); Ok(EventResult::ToolResult(ToolResultEventResult { content: Some(content), ..Default::default() })) })) };
    let result = runner(vec![extension("a", EventKind::ToolResult, handler("a")), extension("b", EventKind::ToolResult, handler("b"))]).emit_tool_result(result_event()).await.unwrap().unwrap();
    assert_eq!(result.content, Some(vec![ToolContent::text("base"), ToolContent::text("a"), ToolContent::text("b")]));
}
#[tokio::test]
async fn tool_result_partial_patches_preserve_prior_fields() {
    let first: ExtensionHandler = Arc::new(|_, _| Box::pin(async { Ok(EventResult::ToolResult(ToolResultEventResult { content: Some(vec![ToolContent::text("first")]), details: Some(JsonValue::Bool(true)), ..Default::default() })) }));
    let second: ExtensionHandler = Arc::new(|_, _| Box::pin(async { Ok(EventResult::ToolResult(ToolResultEventResult { is_error: Some(true), ..Default::default() })) }));
    let result = runner(vec![extension("a", EventKind::ToolResult, first), extension("b", EventKind::ToolResult, second)]).emit_tool_result(result_event()).await.unwrap().unwrap();
    assert_eq!(result.content, Some(vec![ToolContent::text("first")])); assert_eq!(result.details, Some(JsonValue::Bool(true))); assert_eq!(result.is_error, Some(true));
}
#[tokio::test]
async fn tool_result_none_returns_none() { assert!(runner(vec![extension("a", EventKind::ToolResult, none())]).emit_tool_result(result_event()).await.unwrap().is_none()); }
#[tokio::test]
async fn tool_result_error_isolated_with_failed_hook_then_completed_hook() {
    let events = Arc::new(Mutex::new(Vec::new())); let capture = Arc::clone(&events);
    let mut runner = runner(vec![extension("bad", EventKind::ToolResult, failure()), extension("good", EventKind::ToolResult, none())]);
    runner.set_tool_hook_lifecycle_observer(Some(Arc::new(move |event| capture.lock().unwrap().push(event.clone()))));
    runner.emit_tool_result(result_event()).await.unwrap();
    let events = events.lock().unwrap(); assert_eq!(events.len(), 4);
    assert!(matches!(events[1].phase, ToolHookPhase::End { status: ToolHookStatus::Failed, .. })); assert!(matches!(events[3].phase, ToolHookPhase::End { status: ToolHookStatus::Completed, .. }));
}
#[tokio::test]
async fn tool_call_mutations_chain_and_reach_caller() {
    let handler: ExtensionHandler = Arc::new(|event, _| Box::pin(async move { let ExtensionEvent::ToolCall(event) = event else { panic!() }; event.input = JsonValue::Bool(true); Ok(EventResult::None) }));
    let mut event = ToolCallEvent { tool_call_id: "call".into(), tool_name: "bash".into(), input: JsonValue::Null };
    runner(vec![extension("a", EventKind::ToolCall, handler)]).emit_tool_call(&mut event).await.unwrap(); assert_eq!(event.input, JsonValue::Bool(true));
}
#[tokio::test]
async fn tool_call_block_short_circuits_and_reports_blocked_hook() {
    let handler: ExtensionHandler = Arc::new(|_, _| Box::pin(async { Ok(EventResult::ToolCall(ToolCallEventResult { block: Some(true), reason: Some("no".into()), terminate: Some(true) })) }));
    let events = Arc::new(Mutex::new(Vec::new())); let capture = Arc::clone(&events);
    let mut runner = runner(vec![extension("a", EventKind::ToolCall, handler), extension("b", EventKind::ToolCall, failure())]); runner.set_tool_hook_lifecycle_observer(Some(Arc::new(move |e| capture.lock().unwrap().push(e.clone()))));
    let mut event = ToolCallEvent { tool_call_id: "call".into(), tool_name: "bash".into(), input: JsonValue::Null };
    assert_eq!(runner.emit_tool_call(&mut event).await.unwrap().unwrap().terminate, Some(true)); assert!(matches!(events.lock().unwrap()[1].phase, ToolHookPhase::End { status: ToolHookStatus::Blocked, .. }));
}
#[tokio::test]
async fn tool_call_errors_propagate_like_upstream() {
    let mut runner = runner(vec![extension("a", EventKind::ToolCall, failure())]); let mut event = ToolCallEvent { tool_call_id: "call".into(), tool_name: "bash".into(), input: JsonValue::Null };
    assert_eq!(runner.emit_tool_call(&mut event).await.unwrap_err().message, "boom"); assert!(runner.errors.is_empty());
}
#[tokio::test]
async fn hook_updates_are_sanitized_bounded_and_stale_updates_ignored() {
    let updater = Arc::new(Mutex::new(None)); let capture = Arc::clone(&updater);
    let handler: ExtensionHandler = Arc::new(move |_, ctx| { let capture = Arc::clone(&capture); Box::pin(async move { let update = ctx.update_tool_hook_status.clone().unwrap(); update("line\u{1b}[31mone\ntwo\t\u{7} done"); update(&"x".repeat(200)); *capture.lock().unwrap() = Some(update); Ok(EventResult::None) }) });
    let events = Arc::new(Mutex::new(Vec::new())); let captured_events = Arc::clone(&events); let mut runner = runner(vec![extension("<builtin:hooks>", EventKind::ToolResult, handler)]);
    runner.set_tool_hook_lifecycle_observer(Some(Arc::new(move |e| captured_events.lock().unwrap().push(e.clone())))); runner.emit_tool_result(result_event()).await.unwrap();
    updater.lock().unwrap().as_ref().unwrap()("late"); let events = events.lock().unwrap(); assert_eq!(events.len(), 4); assert_eq!(events[1].status_message, "lineone two done"); assert_eq!(events[2].status_message, format!("{}...", "x".repeat(76))); assert_eq!(events[3].status_message, events[2].status_message);
}
#[tokio::test]
async fn headers_mutate_in_place_and_ignore_return_value() {
    let handler: ExtensionHandler = Arc::new(|event, _| Box::pin(async move { let ExtensionEvent::BeforeProviderHeaders { headers } = event else { panic!() }; headers.insert("X-Turn".into(), Some("3".into())); Ok(EventResult::ProviderPayload(JsonValue::Null)) }));
    let result = runner(vec![extension("a", EventKind::BeforeProviderHeaders, handler)]).emit_before_provider_headers(BTreeMap::from([("User-Agent".into(), Some("test".into()))])).await.unwrap(); assert_eq!(result.len(), 2); assert_eq!(result["X-Turn"].as_deref(), Some("3"));
}
#[tokio::test]
async fn headers_error_isolated_before_later_handler() {
    let handler: ExtensionHandler = Arc::new(|event, _| Box::pin(async move { let ExtensionEvent::BeforeProviderHeaders { headers } = event else { panic!() }; headers.insert("good".into(), Some("yes".into())); Ok(EventResult::None) }));
    let mut runner = runner(vec![extension("bad", EventKind::BeforeProviderHeaders, failure()), extension("good", EventKind::BeforeProviderHeaders, handler)]); assert_eq!(runner.emit_before_provider_headers(BTreeMap::new()).await.unwrap()["good"].as_deref(), Some("yes")); assert_eq!(runner.errors.len(), 1);
}
#[tokio::test]
async fn provider_payload_transform_chains_and_supports_null() {
    let first: ExtensionHandler = Arc::new(|_, _| Box::pin(async { Ok(EventResult::ProviderPayload(JsonValue::Bool(true))) }));
    let second: ExtensionHandler = Arc::new(|event, _| Box::pin(async move { let ExtensionEvent::BeforeProviderRequest { payload, .. } = event else { panic!() }; assert_eq!(*payload, JsonValue::Bool(true)); Ok(EventResult::ProviderPayload(JsonValue::Null)) }));
    assert_eq!(runner(vec![extension("a", EventKind::BeforeProviderRequest, first), extension("b", EventKind::BeforeProviderRequest, second)]).emit_before_provider_request(JsonValue::Bool(false), None).await.unwrap(), JsonValue::Null);
}
#[tokio::test]
async fn provider_payload_excluded_extension_does_not_run() { assert_eq!(runner(vec![extension("a", EventKind::BeforeProviderRequest, failure())]).emit_before_provider_request(JsonValue::Null, Some("a")).await.unwrap(), JsonValue::Null); }
#[tokio::test]
async fn context_clones_and_isolates_original_messages() {
    let handler: ExtensionHandler = Arc::new(|event, _| Box::pin(async move { let ExtensionEvent::Context { messages } = event else { panic!() }; messages.clear(); Ok(EventResult::None) }));
    let messages = vec![AgentMessage::Llm(Message::User(UserMessage { content: UserContent::Text("original".into()), timestamp: 1 }))]; assert!(runner(vec![extension("a", EventKind::Context, handler)]).emit_context(&messages, None).await.unwrap().is_empty()); assert_eq!(messages.len(), 1);
}
#[tokio::test]
async fn context_excludes_originating_extension() { let mut runner = runner(vec![extension("a", EventKind::Context, failure())]); runner.emit_context(&[], Some("a")).await.unwrap(); assert!(runner.errors.is_empty()); }
#[tokio::test]
async fn project_trust_first_decision_after_undecided_wins() {
    let handler = |trusted| -> ExtensionHandler { Arc::new(move |_, _| Box::pin(async move { Ok(EventResult::ProjectTrust(ProjectTrustEventResult { trusted, remember: Some(true) })) })) };
    let result = runner(vec![extension("a", EventKind::ProjectTrust, handler(TrustDecision::Undecided)), extension("b", EventKind::ProjectTrust, handler(TrustDecision::No)), extension("c", EventKind::ProjectTrust, failure())]).emit_project_trust("/tmp".into()).await.unwrap().unwrap(); assert_eq!(result.trusted, TrustDecision::No);
}
#[tokio::test]
async fn resources_preserve_scope_origin_and_order() {
    let handler: ExtensionHandler = Arc::new(|event, _| Box::pin(async move { let ExtensionEvent::ResourcesDiscover(event) = event else { panic!() }; assert!(event.scoped_entries); Ok(EventResult::ResourcesDiscover(ResourcesDiscoverResult { skill_paths: vec![ResourceDiscoverEntry { path: "one".into(), scope: Some(SourceScope::User) }, "two".to_string().into()], ..Default::default() })) }));
    let result = runner(vec![extension("a", EventKind::ResourcesDiscover, handler)]).emit_resources_discover("/tmp".into(), SessionReason::Startup).await.unwrap(); assert_eq!(result.skill_paths[0].scope, Some(SourceScope::User)); assert_eq!(result.skill_paths[1].scope, None); assert_eq!(result.skill_paths[1].extension_path, "a");
}
#[test]
fn mcp_servers_first_wins_and_context_exposes_aggregate() {
    let mut a = extension("a", EventKind::AgentStart, none()); let mut b = extension("b", EventKind::AgentStart, none());
    for ext in [&mut a, &mut b] { ext.mcp_servers.push(RegisteredMcpServerDeclaration { name: "dup".into(), config: McpServerDeclaration { command: Some(ext.identity.path.clone()), ..Default::default() }, extension_path: ext.identity.path.clone(), registration_cwd: "/tmp".into() }); }
    let runner = runner(vec![a, b]); assert_eq!(runner.get_registered_mcp_servers().len(), 1); assert_eq!(runner.create_context().unwrap().registered_mcp_servers[0].config.command.as_deref(), Some("a"));
}
#[test]
fn mcp_servers_empty_without_declarations() { assert!(runner(vec![]).get_registered_mcp_servers().is_empty()); }
#[test]
fn command_duplicates_receive_insertion_order_suffixes() {
    let mut a = extension("a", EventKind::AgentStart, none()); let mut b = extension("b", EventKind::AgentStart, none());
    for ext in [&mut a, &mut b] { ext.commands.push(RegisteredCommand { name: "same".into(), source_info: ext.source_info.clone(), description: Some(ext.identity.path.clone()), argument_hint: None, handler: Arc::new(|_, _| Box::pin(async { Ok(()) })) }); }
    let runner = runner(vec![a, b]); assert_eq!(runner.get_registered_commands().iter().map(|c| c.invocation_name.as_str()).collect::<Vec<_>>(), vec!["same:1", "same:2"]); assert_eq!(runner.get_command("same:2").unwrap().command.description.as_deref(), Some("b")); assert!(runner.get_command("same").is_none());
}
#[test]
fn context_live_idle_and_compaction_actions() {
    let state = Arc::new(std::sync::atomic::AtomicBool::new(true)); let mut ctx = context(); let idle = Arc::clone(&state); let compacting = Arc::clone(&state);
    ctx.is_idle_fn = Arc::new(move || idle.load(std::sync::atomic::Ordering::SeqCst)); ctx.is_compacting_fn = Arc::new(move || compacting.load(std::sync::atomic::Ordering::SeqCst));
    let runner = ExtensionRunner::new(vec![], ExtensionRuntime::default(), EventBus::default(), ctx); let ctx = runner.create_context().unwrap(); assert!(ctx.is_idle()); state.store(false, std::sync::atomic::Ordering::SeqCst); assert!(!ctx.is_idle()); assert!(!ctx.is_compacting());
}
#[test]
fn context_signal_observes_cancellation() { let mut ctx = context(); let signal = AbortSignal::default(); ctx.signal = Some(signal.clone()); assert!(!ctx.signal.as_ref().unwrap().is_aborted()); signal.abort(); assert!(ctx.signal.as_ref().unwrap().is_aborted()); }
#[test]
fn context_print_mode_and_tool_context_contract() { let ctx = context(); assert_eq!(ctx.mode, ExtensionMode::Print); assert!(!ctx.has_ui); assert_eq!(ToolContext::cwd(&ctx), Path::new("/tmp")); assert_eq!(ToolContext::session_manager(&ctx).session_id(), "session"); }
#[test]
fn invalidated_runner_rejects_new_context() { let runner = runner(vec![]); runner.invalidate("stale"); assert_eq!(runner.create_context().err().unwrap().message, "stale"); }
#[tokio::test(start_paused = true)]
async fn shutdown_hard_cap_aborts_and_runs_next_handler() {
    let signal = Arc::new(Mutex::new(None)); let capture = Arc::clone(&signal);
    let hung: ExtensionHandler = Arc::new(move |event, _| { let capture = Arc::clone(&capture); Box::pin(async move { let ExtensionEvent::SessionShutdown(event) = event else { panic!() }; *capture.lock().unwrap() = event.signal.clone(); std::future::pending().await }) });
    let seen = Arc::new(Mutex::new(false)); let capture = Arc::clone(&seen); let good: ExtensionHandler = Arc::new(move |_, _| { let capture = Arc::clone(&capture); Box::pin(async move { *capture.lock().unwrap() = true; Ok(EventResult::None) }) });
    let mut runner = runner(vec![extension("hung", EventKind::SessionShutdown, hung), extension("good", EventKind::SessionShutdown, good)]); runner.shutdown_warn_ms = 0; runner.shutdown_timeout_ms = 50;
    runner.emit(ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None })).await.unwrap(); assert!(*seen.lock().unwrap()); assert!(signal.lock().unwrap().as_ref().unwrap().is_aborted()); assert_eq!(runner.errors[0].error, "handler timed out after 50ms");
}
#[tokio::test(start_paused = true)]
async fn shutdown_warning_event_releases_handler_without_error() {
    let gate = Arc::new(tokio::sync::Notify::new()); let wait = Arc::clone(&gate);
    let handler: ExtensionHandler = Arc::new(move |_, _| { let wait = Arc::clone(&wait); Box::pin(async move { wait.notified().await; Ok(EventResult::None) }) });
    let mut runner = runner(vec![extension("slow", EventKind::SessionShutdown, handler)]); runner.shutdown_warn_ms = 5; runner.set_warning_listener(Some(Arc::new(move |_| gate.notify_one())));
    runner.emit(ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Reload, target_session_file: None, signal: None })).await.unwrap(); assert_eq!(runner.warnings.len(), 1); assert!(runner.errors.is_empty());
}
#[tokio::test]
async fn shutdown_fast_handler_keeps_signal_unaborted() {
    let handler: ExtensionHandler = Arc::new(|event, _| Box::pin(async move { let ExtensionEvent::SessionShutdown(event) = event else { panic!() }; assert_eq!(event.reason, SessionReason::New); assert!(!event.signal.as_ref().unwrap().is_aborted()); Ok(EventResult::None) }));
    let mut runner = runner(vec![extension("fast", EventKind::SessionShutdown, handler)]); runner.emit(ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::New, target_session_file: Some("next".into()), signal: None })).await.unwrap(); assert!(runner.errors.is_empty()); assert!(runner.warnings.is_empty());
}
#[tokio::test]
async fn shutdown_error_preserves_extension_error_shape() { let mut runner = runner(vec![extension("bad", EventKind::SessionShutdown, failure())]); runner.emit(ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None })).await.unwrap(); assert_eq!(runner.errors[0].event, "session_shutdown"); assert_eq!(runner.errors[0].stack.as_deref(), Some("test-stack")); }
#[test]
fn extension_error_runtime_with_event() { assert_eq!(format_extension_error_headline(&ExtensionError { extension_path: "<runtime>".into(), event: "title".into(), error: "boom".into(), stack: None }), "Runtime error (title): boom"); }
#[test]
fn extension_error_runtime_without_event() { assert_eq!(format_extension_error_headline(&ExtensionError { extension_path: "<runtime>".into(), event: "".into(), error: "boom".into(), stack: None }), "Runtime error: boom"); }
#[test]
fn extension_error_real_extension_framing() { assert_eq!(format_extension_error_headline(&ExtensionError { extension_path: "/ext.ts".into(), event: "tool_call".into(), error: "boom".into(), stack: None }), "Extension \"/ext.ts\" error: boom"); }
#[test]
fn extension_error_strips_hostile_control_sequences() {
    let error = ExtensionError { extension_path: "<runtime>".into(), event: "title".into(), error: "Overloaded\u{1b}]52;c;Zm9v\u{7}\u{1b}]0;pwned\u{1b}\\\u{1b}]8;;https://evil\u{7}link\u{1b}]8;;\u{7}\u{1b}[31mred\u{1b}[0m\u{9b}2Jclear\0\u{8}\u{7f}tail".into(), stack: None };
    assert_eq!(format_extension_error_headline(&error), "Runtime error (title): Overloadedlinkredcleartail");
}
#[test]
fn extension_error_sanitizes_path_and_event() { assert_eq!(format_extension_error_headline(&ExtensionError { extension_path: "/ext\u{1b}]52;c;Zm9v\u{7}.ts".into(), event: "tool\u{1b}[31m_call".into(), error: "boom".into(), stack: None }), "Extension \"/ext.ts\" error: boom"); }

#[tokio::test]
async fn static_registration_executes_real_extension_traits_in_order() {
    struct Append(&'static str);
    impl Extension for Append { fn register(&self, api: &mut ExtensionApi) { api.on(EventKind::BeforeAgentStart, prompt(self.0)); } }
    let mut runner = ExtensionRunner::from_static(vec![Box::new(Append("1")), Box::new(Append("2"))], context());
    assert_eq!(runner.emit_before_agent_start(before()).await.unwrap().unwrap().system_prompt.as_deref(), Some("base12"));
}
#[tokio::test]
async fn message_end_chains_same_role_replacements() {
    let handler = |text: &'static str| -> ExtensionHandler { Arc::new(move |_, _| Box::pin(async move { Ok(EventResult::MessageEnd { message: Some(AgentMessage::Llm(Message::User(UserMessage { content: UserContent::Text(text.into()), timestamp: 1 }))) }) })) };
    let original = AgentMessage::Llm(Message::User(UserMessage { content: UserContent::Text("original".into()), timestamp: 1 }));
    let result = runner(vec![extension("a", EventKind::MessageEnd, handler("one")), extension("b", EventKind::MessageEnd, handler("two"))]).emit_message_end(original).await.unwrap().unwrap();
    assert_eq!(result, AgentMessage::Llm(Message::User(UserMessage { content: UserContent::Text("two".into()), timestamp: 1 })));
}
#[tokio::test]
async fn message_end_no_replacement_returns_none() {
    let original = AgentMessage::Llm(Message::User(UserMessage { content: UserContent::Text("original".into()), timestamp: 1 }));
    assert!(runner(vec![extension("a", EventKind::MessageEnd, none())]).emit_message_end(original).await.unwrap().is_none());
}
#[test]
fn mcp_collision_diagnostic_names_both_owners() {
    let mut a = extension("a", EventKind::AgentStart, none()); let mut b = extension("b", EventKind::AgentStart, none());
    for ext in [&mut a, &mut b] { ext.mcp_servers.push(RegisteredMcpServerDeclaration { name: "dup".into(), config: McpServerDeclaration::default(), extension_path: ext.identity.path.clone(), registration_cwd: "/tmp".into() }); }
    let warnings = runner(vec![a,b]).get_mcp_server_diagnostics(); assert_eq!(warnings, vec!["MCP server 'dup' declared by both a and b; keeping first declaration from a."]);
}
#[test]
fn tools_first_wins_with_non_builtin_override() {
    let tool = |description: &str| ToolDefinition::new("shared", description, JsonValue::Object(Default::default()), Arc::new(|_| Box::pin(async { Ok(ToolResult::text("ok")) })));
    let mut a = extension("a", EventKind::AgentStart, none()); let mut b = extension("b", EventKind::AgentStart, none());
    a.source_info.source = "builtin".into();
    a.tools.push(RegisteredTool { definition: tool("builtin"), source_info: a.source_info.clone() }); b.tools.push(RegisteredTool { definition: tool("user"), source_info: b.source_info.clone() });
    let runner = runner(vec![a,b]); assert_eq!(runner.get_all_tools()[0].description, "user"); assert_eq!(runner.get_tool_definition("shared").unwrap().description, "builtin");
}
#[test]
fn normalized_tool_metadata_discards_non_search_text() {
    let mut tool = ToolDefinition::new("test", "test", JsonValue::Object(Default::default()), Arc::new(|_| Box::pin(async { Ok(ToolResult::text("ok")) })));
    tool.search_text = Some("hidden".into()); tool.allow_lazy_activation = Some(false);
    let info = normalize_tool_exposure(&tool, SourceInfo::default()); assert_eq!(info.exposure, ToolExposure::Direct); assert_eq!(info.search_text, None); assert!(!info.allow_lazy_activation);
    tool.exposure = Some(ToolExposure::Search); assert_eq!(normalize_tool_exposure(&tool, SourceInfo::default()).search_text.as_deref(), Some("hidden"));
}
#[test]
fn flags_first_definition_wins_across_extensions() {
    let mut a = extension("a", EventKind::AgentStart, none()); let mut b = extension("b", EventKind::AgentStart, none());
    for ext in [&mut a, &mut b] { ext.flags.push(ExtensionFlag { name: "shared".into(), description: Some(ext.identity.path.clone()), kind: FlagType::Boolean { default: Some(true) }, extension_path: ext.identity.path.clone() }); }
    assert_eq!(runner(vec![a,b]).get_flags()["shared"].description.as_deref(), Some("a"));
}
#[test]
fn renderers_first_owner_and_options_stay_together() {
    let mut a = extension("a", EventKind::AgentStart, none()); let mut b = extension("b", EventKind::AgentStart, none());
    a.message_renderers.insert("custom".into(), Arc::new(|_, _, _| None));
    a.entry_renderers.insert("custom".into(), Arc::new(|_, _, _| None));
    b.entry_renderers.insert("custom".into(), Arc::new(|_, _, _| None)); b.entry_renderer_options.insert("custom".into(), EntryRendererOptions::default());
    let runner = runner(vec![a,b]); assert!(runner.get_message_renderer("custom").is_some()); assert!(runner.get_entry_renderer("custom").is_some()); assert!(runner.get_entry_renderer_options("custom").is_none()); assert!(runner.get_entry_renderer("missing").is_none());
}
#[test]
fn extension_actions_reject_before_binding_and_after_invalidation() {
    let runtime = ExtensionRuntime::default(); let api = ExtensionApi::new(LoadedExtension::new("a", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), runtime.clone());
    assert!(api.append_entry("state", None).is_err()); runtime.invalidate("stale"); assert_eq!(api.get_all_tools().unwrap_err().message, "stale");
}
#[tokio::test]
async fn wait_for_idle_uses_host_event_signal() {
    let (sender, receiver) = tokio::sync::oneshot::channel(); let receiver = Arc::new(Mutex::new(Some(receiver))); let mut ctx = context();
    ctx.wait_for_idle_fn = Arc::new(move || { let receiver = receiver.lock().unwrap().take().unwrap(); Box::pin(async move { receiver.await.unwrap(); }) });
    sender.send(()).unwrap(); ctx.wait_for_idle().await;
}
#[test]
fn widgets_support_both_senpi_placements() { assert_eq!(WidgetPlacement::default().as_str(), "aboveEditor"); assert_eq!(WidgetPlacement::BelowEditor.as_str(), "belowEditor"); }
#[test]
fn extension_error_normalizes_carriage_returns_and_spaces() { assert_eq!(format_extension_error_headline(&ExtensionError { extension_path: "/ext.ts".into(), event: "x".into(), error: "a\r\nb\rc\t  d".into(), stack: None }), "Extension \"/ext.ts\" error: a\nb\nc d"); }

#[tokio::test]
async fn model_select_merges_partial_results_and_preserves_null_reset() {
    let model = Model { id: "test".into(), name: "test".into(), api: "openai-completions".into(), provider: "test".into(), base_url: "https://example.test".into(), reasoning: false, thinking_level_map: None, input: vec![], cost: Default::default(), context_window: 1000, max_tokens: 100, sampling_params: None, headers: None, cache_retention: None, upstream_model_id: None, service_tier: None, recover_text_tool_calls: None, compat: None };
    let first: ExtensionHandler = Arc::new(|_, _| Box::pin(async { Ok(EventResult::ModelSelect(ModelSelectEventResult { system_prompt: Some(None), system_prompt_name: None })) }));
    let second: ExtensionHandler = Arc::new(|event, ctx| Box::pin(async move { let ExtensionEvent::ModelSelect(event) = event else { panic!() }; assert_eq!(event.system_prompt_options, ctx.get_system_prompt_options()); Ok(EventResult::ModelSelect(ModelSelectEventResult { system_prompt: None, system_prompt_name: Some("default".into()) })) }));
    let result = runner(vec![extension("a", EventKind::ModelSelect, first), extension("b", EventKind::ModelSelect, second)]).emit_model_select(ModelSelectEvent { model, previous_model: None, source: ModelSelectSource::Set, system_prompt: "old".into(), system_prompt_options: Default::default() }).await.unwrap().unwrap();
    assert_eq!(result.system_prompt, Some(None)); assert_eq!(result.system_prompt_name.as_deref(), Some("default"));
}
#[test]
fn bus_panics_are_isolated_and_invalidation_unsubscribes() {
    let runner = runner(vec![]); let count = Arc::new(Mutex::new(0)); let capture = Arc::clone(&count);
    let _bad = runner.events.on("x", Arc::new(|_| panic!("bad listener")));
    let _good = runner.events.on("x", Arc::new(move |_| *capture.lock().unwrap() += 1));
    runner.events.emit("x", &JsonValue::Null); assert_eq!(*count.lock().unwrap(), 1);
    runner.invalidate("reload"); runner.events.emit("x", &JsonValue::Null); assert_eq!(*count.lock().unwrap(), 1);
}

#[tokio::test]
async fn rpc_requests_require_one_owner_and_an_active_runtime() {
    let mut a = extension("a", EventKind::AgentStart, none());
    a.rpc_handlers.insert("echo".into(), Arc::new(|data| Box::pin(async move { Ok(data) })));
    let mut b = extension("b", EventKind::AgentStart, none()); b.rpc_handlers = a.rpc_handlers.clone();
    let duplicate = runner(vec![a.clone(), b]);
    assert_eq!(duplicate.handle_rpc_request("echo", JsonValue::Null).await.unwrap_err().message, "Multiple extension RPC request handlers registered: echo");
    let one = runner(vec![a]);
    assert_eq!(one.handle_rpc_request(" echo ", JsonValue::Bool(true)).await.unwrap(), JsonValue::Bool(true));
    assert!(one.handle_rpc_request(" ", JsonValue::Null).await.is_err());
    assert!(one.handle_rpc_request("missing", JsonValue::Null).await.is_err());
    one.invalidate("replaced"); assert_eq!(one.handle_rpc_request("echo", JsonValue::Null).await.unwrap_err().message, "replaced");
}
#[test]
fn markdown_transformers_chain_in_extension_order() {
    let mut a = extension("a", EventKind::AgentStart, none()); let mut b = extension("b", EventKind::AgentStart, none());
    a.markdown_transformer = Some(Arc::new(|text, _| format!("{text}A")));
    b.markdown_transformer = Some(Arc::new(|text, context| format!("{text}{}", context.available_width)));
    let context = MarkdownTransformContext { message_type: MarkdownMessageType::AssistantThinking, is_streaming: true, available_width: 80 };
    assert_eq!(runner(vec![a,b]).transform_markdown("base", &context), "baseA80");
}
#[test]
fn shortcuts_normalize_case_and_last_extension_wins() {
    let mut a = extension("a", EventKind::AgentStart, none()); let mut b = extension("b", EventKind::AgentStart, none());
    for (extension, key) in [(&mut a, "Ctrl+X"), (&mut b, "ctrl+x")] {
        extension.shortcuts.insert(key.into(), ExtensionShortcut { shortcut: key.into(), description: None, handler: Arc::new(|_| Box::pin(async { Ok(()) })), extension_path: extension.identity.path.clone() });
    }
    let shortcuts = runner(vec![a,b]).get_shortcuts(); assert_eq!(shortcuts.len(), 1); assert_eq!(shortcuts["ctrl+x"].extension_path, "b");
}

#[derive(Default)]
struct SessionActions { active: Mutex<Vec<String>>, name: Mutex<Option<String>>, fast: Mutex<bool>, entries: Mutex<Vec<(String, Option<JsonValue>)>> }
impl ExtensionActions for SessionActions {
    fn send_message(&self, _: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> { Err("not used".into()) }
    fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> { Err("not used".into()) }
    fn append_entry(&self, name: &str, data: Option<JsonValue>) -> Result<(), ExtensionFailure> { self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((name.into(), data)); Ok(()) }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> { Ok(vec![]) }
}
impl ExtensionSessionActions for SessionActions {
    fn set_session_name(&self, name: &str) -> Result<(), ExtensionFailure> { *self.name.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(name.into()); Ok(()) }
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> { Ok(self.name.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()) }
    fn set_label(&self, _: &str, _: Option<&str>) -> Result<(), ExtensionFailure> { Err("not used".into()) }
    fn execute_tool<'a>(&'a self, name: &'a str, _: JsonValue, _: ExecuteToolOptions) -> ExecuteToolFuture<'a> {
        Box::pin(async move {
            let active = self.get_active_tools().unwrap_or_default();
            Err(ExecuteToolError { code: ExecuteToolErrorCode::InactiveTool, tool_name: name.into(), message: "inactive".into(), active_tools: active })
        })
    }
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> { Ok(self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()) }
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure> { *self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = names; Ok(()) }
    fn refresh_tools(&self) -> Result<(), ExtensionFailure> { self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(("refresh".into(), None)); Ok(()) }
    fn register_removed_tool_hint(&self, name: &str, hint: &str) -> Result<(), ExtensionFailure> { self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((name.into(), Some(JsonValue::String(hint.into())))); Ok(()) }
    fn register_lazy_tool_activator(&self, activator: LazyToolActivator) -> Result<(), ExtensionFailure> { self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(("activator".into(), Some(JsonValue::Bool(activator("hidden"))))); Ok(()) }
    fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure> { Ok(vec![]) }
    fn set_model(&self, _: Model) -> ExtensionFuture<'_, bool> { Box::pin(async { Err("not used".into()) }) }
    fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure> { Ok(ThinkingLevel::Low) }
    fn set_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> { Ok(()) }
    fn set_session_model(&self, _: Model) -> ExtensionFuture<'_, bool> { Box::pin(async { Err("not used".into()) }) }
    fn set_session_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> { Ok(()) }
    fn set_session_fast_mode(&self, enabled: bool) -> Result<(), ExtensionFailure> { *self.fast.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = enabled; Ok(()) }
    fn exec<'a>(&'a self, _: &'a str, _: &'a [String], _: &'a Path, _: ExecOptions) -> ExtensionFuture<'a, ExecResult> { Box::pin(async { Err("not used".into()) }) }
}
#[tokio::test]
async fn bound_session_actions_forward_state_and_preserve_typed_tool_errors() {
    let actions = Arc::new(SessionActions::default());
    let runtime = ExtensionRuntime::default();
    let runner = ExtensionRunner::new(vec![], runtime.clone(), EventBus::default(), context());
    runner.bind_session_actions(actions.clone()).unwrap();
    let api = ExtensionApi::new(LoadedExtension::new("test", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), runtime);
    api.set_session_name("new name").unwrap(); assert_eq!(api.get_session_name().unwrap().as_deref(), Some("new name"));
    api.set_active_tools(vec!["read".into()]).unwrap(); assert_eq!(api.get_active_tools().unwrap(), ["read"]);
    api.set_session_fast_mode(true).unwrap(); assert!(*actions.fast.lock().unwrap());
    assert_eq!(api.get_thinking_level().unwrap(), ThinkingLevel::Low);
    let error = api.execute_tool("hidden", JsonValue::Null, ExecuteToolOptions::default()).await.unwrap_err();
    assert_eq!(error.code, ExecuteToolErrorCode::InactiveTool); assert_eq!(error.active_tools, ["read"]);
}

#[test]
fn runtime_registrations_forward_to_already_bound_session() {
    let actions = Arc::new(SessionActions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind_session_actions(actions.clone());
    let mut api = ExtensionApi::new(LoadedExtension::new("live", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), runtime);
    api.register_lazy_tool_activator(Arc::new(|name| name == "hidden"));
    api.register_removed_tool_hint("old", "use new");
    api.register_tool(ToolDefinition::new("new", "new", JsonValue::Object(Default::default()), Arc::new(|_| Box::pin(async { Ok(ToolResult::text("ok")) }))));
    assert_eq!(*actions.entries.lock().unwrap(), vec![("activator".into(), Some(JsonValue::Bool(true))), ("old".into(), Some(JsonValue::String("use new".into()))), ("refresh".into(), None)]);
}
#[test]
fn native_loader_preserves_factory_identity_profile_and_order() {
    use maho_ext_host::loader::*;
    struct Register;
    impl Extension for Register {
        fn register(&self, api: &mut ExtensionApi) {
            assert_eq!(api.profile.session_kind, SessionKind::Worker);
            api.register_flag("test", FlagType::String { default: Some(api.registered.identity.path.clone()) }, None);
        }
    }
    let factories = ["first", "second"].into_iter().map(|path| NativeExtensionFactory { path: path.into(), source_info: SourceInfo::default(), extension: Box::new(Register) }).collect();
    let loaded = load_extensions(factories, Path::new("/tmp"), ExtensionSessionProfile { session_kind: SessionKind::Worker, ..Default::default() });
    assert_eq!(loaded.extensions.iter().map(|extension| extension.identity.path.as_str()).collect::<Vec<_>>(), ["first", "second"]);
    assert_eq!(loaded.runtime.get_flag("test"), Some(FlagValue::String("first".into()))); assert!(loaded.errors.is_empty());
}
#[tokio::test]
async fn registered_tool_wrapper_supplies_context_and_reports_new_active_tools() {
    let actions = Arc::new(SessionActions::default()); actions.set_active_tools(vec!["read".into()]).unwrap();
    let runtime = ExtensionRuntime::default(); runtime.bind_session_actions(actions.clone());
    let captured = actions.clone();
    let definition = ToolDefinition::new("activate", "activate", JsonValue::Object(Default::default()), Arc::new(move |call| {
        let actions = captured.clone();
        Box::pin(async move {
            assert_eq!(call.context.unwrap().cwd(), Path::new("/tmp"));
            actions.set_active_tools(vec!["read".into(), "new".into()]).unwrap_or_else(|error| panic!("{error}"));
            Ok(ToolResult::text("activated"))
        })
    }));
    let tool = maho_ext_host::wrapper::wrap_registered_tool(RegisteredTool { definition, source_info: SourceInfo::default() }, runtime, Arc::new(|| Ok(context())));
    let result = (tool.execute)("call".into(), JsonValue::Null, None, None).await;
    assert_eq!(result.added_tool_names, Some(vec!["new".into()])); assert_ne!(result.is_error, Some(true));
}

#[test]
fn static_runner_uses_one_based_identity_and_isolates_failed_factories() {
    struct Failed;
    impl Extension for Failed { fn register(&self, api: &mut ExtensionApi) {
        api.register_flag("discarded", FlagType::Boolean { default: Some(true) }, None);
        panic!("factory failed");
    } }
    struct Good;
    impl Extension for Good { fn register(&self, api: &mut ExtensionApi) {
        api.register_flag("kept", FlagType::Boolean { default: Some(true) }, None);
    } }
    let runner = ExtensionRunner::from_static(vec![Box::new(Failed), Box::new(Good)], context());
    assert_eq!(runner.extensions.len(), 1);
    assert_eq!(runner.extensions[0].identity.path, "<inline:2>");
    assert_eq!(runner.errors[0].extension_path, "<inline:1>");
    assert_eq!(runner.runtime.get_flag("discarded"), None);
    assert_eq!(runner.runtime.get_flag("kept"), Some(FlagValue::Boolean(true)));
}
#[test]
fn rpc_events_use_the_shared_channel_and_normalized_envelope() {
    let runtime = ExtensionRuntime::default(); let events = EventBus::default();
    let captured = Arc::new(Mutex::new(None)); let output = captured.clone();
    let _subscription = events.on("senpi:extension-rpc-event", Arc::new(move |data| *output.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(data.clone())));
    let mut api = ExtensionApi::new(LoadedExtension::new("test", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), events, runtime.clone());
    api.rpc_emit(" progress ", &JsonValue::Bool(true)).unwrap();
    let result = captured.lock().unwrap().clone().unwrap(); assert_eq!(result["name"], JsonValue::String("progress".into())); assert_eq!(result["data"], JsonValue::Bool(true));
    assert!(api.rpc_emit(" ", &JsonValue::Null).is_err());
    api.rpc_handle(" echo ", Arc::new(|data| Box::pin(async move { Ok(data) }))).unwrap();
    assert!(api.rpc_handle("echo", Arc::new(|data| Box::pin(async move { Ok(data) }))).is_err());
    runtime.invalidate("stale"); assert!(api.rpc_emit("progress", &JsonValue::Null).is_err());
}

#[tokio::test]
async fn invocation_disposes_when_tool_execution_fails() {
    let disposed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = disposed.clone();
    let runtime = ExtensionRuntime::default();
    runtime.bind_session_actions(Arc::new(SessionActions::default()));
    let definition = ToolDefinition::new("fail", "fail", JsonValue::Object(Default::default()), Arc::new(|_| {
        Box::pin(async { Err(ToolError::Message("failed".into())) })
    }));
    let factory = Arc::new(move |signal| {
        let mut ctx = context();
        ctx.signal = signal;
        let disposed = disposed.clone();
        Ok(maho_ext_host::wrapper::ToolInvocation::new(ctx, move || {
            disposed.store(true, std::sync::atomic::Ordering::SeqCst);
        }))
    });
    let tool = maho_ext_host::wrapper::wrap_registered_tool_with_invocation(
        RegisteredTool { definition, source_info: SourceInfo::default() }, runtime, factory,
    );
    let result = (tool.execute)("call".into(), JsonValue::Null, None, None).await;
    assert_eq!(result.is_error, Some(true));
    assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn failed_async_factory_rolls_back_flags_and_subscriptions() {
    use maho_ext_host::loader::*;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let observed = calls.clone();
    let failed = NativeAsyncExtensionFactory {
        path: "failed".into(), source_info: SourceInfo::default(),
        factory: Arc::new(move |api| {
            let calls = calls.clone();
            Box::pin(async move {
                api.register_flag("owned", FlagType::Boolean { default: Some(true) }, None);
                let subscription = api.events.on("test", Arc::new(move |_| calls.lock().unwrap().push("failed")));
                std::mem::forget(subscription);
                Err("factory failed".into())
            })
        }),
    };
    let succeeding = NativeAsyncExtensionFactory {
        path: "good".into(), source_info: SourceInfo::default(),
        factory: Arc::new(|api| Box::pin(async move {
            api.register_flag("owned", FlagType::Boolean { default: Some(false) }, None);
            Ok(())
        })),
    };
    let loaded = load_extensions_async(vec![failed, succeeding], Path::new("/tmp"), ExtensionSessionProfile::default()).await;
    loaded.events.emit("test", &JsonValue::Null);
    assert!(observed.lock().unwrap().is_empty());
    assert_eq!(loaded.runtime.get_flag("owned"), Some(FlagValue::Boolean(false)));
    assert_eq!(loaded.extensions.len(), 1);
    assert_eq!(loaded.extensions[0].identity.path, "good");
    assert_eq!(loaded.errors[0].extension_path, "failed");
}

#[test]
fn protected_shortcut_is_rejected_and_extension_collision_is_reported() {
    let mut first = LoadedExtension::new("first", "/tmp".into(), SourceInfo::default());
    let mut second = LoadedExtension::new("second", "/tmp".into(), SourceInfo::default());
    for extension in [&mut first, &mut second] {
        for key in ["ctrl+c", "ctrl+x"] {
            extension.shortcuts.insert(key.into(), ExtensionShortcut {
                shortcut: key.into(), description: None,
                handler: Arc::new(|_| Box::pin(async { Ok(()) })),
                extension_path: extension.identity.path.clone(),
            });
        }
    }
    let builtins = [("ctrl+c".into(), BuiltinShortcut { keybinding: "interrupt".into(), restrict_override: true })].into_iter().collect();
    let (shortcuts, diagnostics) = runner(vec![first, second]).resolve_shortcuts(&builtins);
    assert!(!shortcuts.contains_key("ctrl+c"));
    assert_eq!(shortcuts["ctrl+x"].extension_path, "second");
    assert_eq!(diagnostics.len(), 3);
    assert_eq!(diagnostics.iter().map(|diagnostic| diagnostic.path.as_str()).collect::<Vec<_>>(), ["first", "second", "second"]);
}

#[tokio::test]
async fn panicking_async_factory_is_isolated_and_rolls_back_registration() {
    use maho_ext_host::loader::*;
    let failed = NativeAsyncExtensionFactory {
        path: "panic".into(), source_info: SourceInfo::default(),
        factory: Arc::new(|api| Box::pin(async move {
            api.register_flag("owned", FlagType::Boolean { default: Some(true) }, None);
            panic!("factory panic");
        })),
    };
    let good = NativeAsyncExtensionFactory {
        path: "good".into(), source_info: SourceInfo::default(),
        factory: Arc::new(|api| Box::pin(async move {
            api.register_flag("owned", FlagType::Boolean { default: Some(false) }, None);
            Ok(())
        })),
    };
    let loaded = load_extensions_async(vec![failed, good], Path::new("/tmp"), ExtensionSessionProfile::default()).await;
    assert_eq!(loaded.runtime.get_flag("owned"), Some(FlagValue::Boolean(false)));
    assert_eq!(loaded.extensions.len(), 1);
    assert_eq!(loaded.errors.len(), 1);
    assert_eq!(loaded.errors[0].extension_path, "panic");
}

#[tokio::test]
async fn inline_factory_names_and_hidden_identities_preserve_load_order() {
    use maho_ext_host::loader::*;
    let factory: AsyncExtensionFactory = Arc::new(|_| Box::pin(async { Ok(()) }));
    let loaded = load_inline_extensions(vec![
        NativeInlineExtension { name: None, hidden: false, factory: factory.clone() },
        NativeInlineExtension { name: Some("named".into()), hidden: true, factory },
    ], Path::new("/tmp"), ExtensionSessionProfile::default()).await;
    assert_eq!(loaded.loaded.extensions.iter().map(|extension| extension.identity.path.as_str()).collect::<Vec<_>>(), ["<inline:1>", "<inline:named>"]);
    assert_eq!(loaded.hidden_paths.into_iter().collect::<Vec<_>>(), ["<inline:named>"]);
}

#[tokio::test]
async fn provider_transform_rejects_result_after_runtime_invalidation() {
    let runtime = ExtensionRuntime::default();
    let captured = runtime.clone();
    let handler: ExtensionHandler = Arc::new(move |_, _| {
        let runtime = captured.clone();
        Box::pin(async move {
            runtime.invalidate("reloaded");
            Ok(EventResult::ProviderPayload(JsonValue::Bool(true)))
        })
    });
    let mut runner = ExtensionRunner::new(vec![extension("invalidate", EventKind::BeforeProviderRequest, handler)], runtime, EventBus::default(), context());
    let error = runner.emit_before_provider_request(JsonValue::Null, None).await.unwrap_err();
    assert_eq!(error.message, "reloaded");
}

#[tokio::test]
async fn failed_factory_retained_runtime_is_stale_without_poisoning_successful_factory() {
    use maho_ext_host::loader::*;
    let retained = Arc::new(std::sync::Mutex::new(None));
    let captured = retained.clone();
    let failed: AsyncExtensionFactory = Arc::new(move |api| {
        *captured.lock().unwrap() = Some(api.runtime.clone());
        Box::pin(async { Err(ExtensionFailure::new("factory error")) })
    });
    let loaded = load_extensions_async(vec![
        NativeAsyncExtensionFactory { path: "failed".into(), source_info: SourceInfo::default(), factory: failed },
        NativeAsyncExtensionFactory { path: "success".into(), source_info: SourceInfo::default(), factory: Arc::new(|_| Box::pin(async { Ok(()) })) },
    ], Path::new("/tmp"), ExtensionSessionProfile::default()).await;
    let failed_runtime = retained.lock().unwrap().take().unwrap();
    assert!(failed_runtime.register_provider(ProviderRegistration::Config { name: "late".into(), config: Box::default() }, "failed").is_err());
    assert!(loaded.runtime.assert_active().is_ok());
    assert_eq!(loaded.extensions.len(), 1);
    assert_eq!(loaded.extensions[0].identity.path, "success");
}

#[tokio::test]
async fn invocation_disposes_when_pending_execution_is_dropped() {
    let disposed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = disposed.clone();
    let runtime = ExtensionRuntime::default();
    runtime.bind_session_actions(Arc::new(SessionActions::default()));
    let definition = ToolDefinition::new("pending", "pending", JsonValue::Object(Default::default()), Arc::new(|_| {
        Box::pin(std::future::pending())
    }));
    let factory = Arc::new(move |_| {
        let disposed = disposed.clone();
        Ok(maho_ext_host::wrapper::ToolInvocation::new(context(), move || {
            disposed.store(true, std::sync::atomic::Ordering::SeqCst);
        }))
    });
    let tool = maho_ext_host::wrapper::wrap_registered_tool_with_invocation(
        RegisteredTool { definition, source_info: SourceInfo::default() }, runtime, factory,
    );
    let mut execution = (tool.execute)("call".into(), JsonValue::Null, None, None);
    std::future::poll_fn(|cx| {
        assert!(execution.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    }).await;
    drop(execution);
    assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
}

struct ContextActions { revision: std::sync::atomic::AtomicU64, aborted: Mutex<Option<AbortSource>> }
impl ExtensionSessionSettings for ContextActions {
    fn get_retry_fallback_settings(&self) -> RetryFallbackSettings { RetryFallbackSettings { model_fallback: false, chains: BTreeMap::new(), revert_policy: FallbackRevertPolicy::Never } }
    fn set_fallback_chain<'a>(&'a self, _: &'a str, _: &'a [String]) -> ExtensionFuture<'a, ()> { Box::pin(async { Ok(()) }) }
    fn remove_fallback_chain<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, ()> { Box::pin(async { Ok(()) }) }
    fn set_model_fallback_enabled(&self, _: bool) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    fn set_fallback_revert_policy(&self, _: FallbackRevertPolicy) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    fn reload(&self) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    fn get_fallback_status(&self) -> Option<RetryFallbackStatus> { None }
}
impl ExtensionContextActions for ContextActions {
    fn get_model(&self) -> Option<Model> { None }
    fn get_service_tier(&self) -> Option<ServiceTier> { Some(ServiceTier::Flex) }
    fn get_scoped_models(&self) -> Vec<ScopedModel> { vec![] }
    fn get_agent_dir(&self) -> std::path::PathBuf { "/fixture/agent".into() }
    fn is_idle(&self) -> bool { false }
    fn is_project_trusted(&self) -> bool { false }
    fn get_signal(&self) -> Option<AbortSignal> { None }
    fn get_steering_signal(&self) -> Option<AbortSignal> { Some(AbortSignal::default()) }
    fn get_thinking_level(&self) -> Option<ThinkingLevel> { Some(ThinkingLevel::High) }
    fn abort(&self, source: Option<AbortSource>) { *self.aborted.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = source; }
    fn has_pending_messages(&self) -> bool { true }
    fn request_reload(&self) -> ExtensionFuture<'_, ()> { Box::pin(async move {
        self.revision.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }) }
    fn is_compacting(&self) -> bool { true }
    fn check_reload_veto(&self) -> ExtensionFuture<'_, ReloadVetoDecision> { Box::pin(async { Ok(ReloadVetoDecision { cancelled: true, reason: Some("busy".into()) }) }) }
    fn shutdown(&self) {}
    fn get_context_usage(&self) -> Option<ContextUsage> { Some(ContextUsage { tokens: Some(100), context_window: 1000, percent: Some(10.0) }) }
    fn get_compaction_settings(&self) -> CompactionSettings { CompactionSettings { enabled: true, reserve_tokens: 100, keep_recent_tokens: 200 } }
    fn get_prompt_cache_safe_wait_seconds(&self) -> Option<f64> { Some(30.0) }
    fn get_prompt_cache_goal_backstop_max_seconds(&self) -> f64 { 60.0 }
    fn get_prompt_cache_keep_alive_settings(&self) -> PromptCacheKeepAliveSettings { PromptCacheKeepAliveSettings { enabled: false, max_requests_per_session: 1, max_cost_usd_per_session: 0.1, margin_seconds: 5.0 } }
    fn get_look_at_settings(&self) -> LookAtSettings { LookAtSettings { enabled: true, models: None } }
    fn get_ask_user_settings(&self) -> AskUserSettings { AskUserSettings { enabled: true, timeout_minutes: 30.0 } }
    fn get_image_settings(&self) -> ImageSettings { ImageSettings { auto_resize: true, block_images: false } }
    fn session_settings(&self) -> &dyn ExtensionSessionSettings { self }
    fn compact(&self, _: CompactOptions) {}
    fn prepare_provider_request(&self, messages: Vec<AgentMessage>) -> ExtensionFuture<'_, ProviderRequestPreparation> {
        Box::pin(async move { Ok(ProviderRequestPreparation { messages, transform_payload: Arc::new(|payload| Box::pin(async move { Ok(payload) })), transform_headers: Arc::new(|headers| Box::pin(async move { Ok(headers) })) }) })
    }
    fn begin_compaction(&self, _: BeginCompactionOptions) -> Option<AbortSignal> { Some(AbortSignal::default()) }
    fn update_compaction(&self, options: UpdateCompactionOptions) {
        if options.signal.is_some_and(|signal| signal.is_aborted()) { self.revision.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }
    }
    fn end_compaction(&self, options: EndCompactionOptions) {
        if options.signal.is_some_and(|signal| signal.is_aborted()) { self.revision.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }
    }
    fn get_message_revision(&self) -> u64 { self.revision.load(std::sync::atomic::Ordering::SeqCst) }
    fn apply_compaction(&self, _: CompactionResult, options: ApplyCompactionOptions) -> ExtensionFuture<'_, ApplyCompactionResult> {
        Box::pin(async move { Ok(if options.expected_revision == Some(self.get_message_revision()) { ApplyCompactionResult::Applied } else { ApplyCompactionResult::Stale }) })
    }
    fn get_system_prompt(&self) -> String { "live prompt".into() }
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions { BuildSystemPromptOptions::default() }
    fn get_loaded_hook_sources(&self) -> LoadedHookSources { LoadedHookSources { cwd: "/tmp".into(), agent_dir: "/fixture/agent".into(), global_hooks_path: "/global".into(), project_hooks_path: "/project".into(), global_settings_hooks: None, project_settings_hooks: None, global_hook_source_paths: vec![], project_hook_source_paths: vec![], pre_session_hook_source_paths: vec![], runtime_hook_source_paths: vec![] } }
    fn kernel_tools(&self) -> Option<&dyn ExtensionKernelTools> { None }
}
#[tokio::test]
async fn context_binding_reads_live_host_state_and_rejects_after_invalidation() {
    let actions = Arc::new(ContextActions { revision: std::sync::atomic::AtomicU64::new(1), aborted: Mutex::new(None) });
    let mut runner = runner(vec![]); runner.bind_context_actions(actions.clone()).unwrap();
    let ctx = runner.create_context().unwrap();
    assert_eq!(ctx.agent_dir, std::path::PathBuf::from("/fixture/agent")); assert_eq!(ctx.effective_service_tier, Some(ServiceTier::Flex));
    assert_eq!(ctx.thinking_level, Some(ThinkingLevel::High));
    assert!(ctx.steering_signal.is_some());
    assert!(!ctx.is_idle()); assert!(ctx.is_compacting()); assert_eq!(ctx.get_system_prompt(), "live prompt");
    assert!(ctx.has_pending_messages().unwrap()); assert!(ctx.check_reload_veto().await.unwrap().cancelled);
    ctx.abort(Some(AbortSource::User)).unwrap(); assert_eq!(*actions.aborted.lock().unwrap(), Some(AbortSource::User));
    actions.revision.store(2, std::sync::atomic::Ordering::SeqCst); assert_eq!(ctx.get_message_revision().unwrap(), 2);
    let result = ctx.apply_compaction(CompactionResult { summary: "summary".into(), first_kept_entry_id: "id".into(), tokens_before: 100, details: None }, ApplyCompactionOptions { reason: CompactionReason::Extension, expected_revision: Some(1), expected_warm_anchor: None, signal: None }).await.unwrap();
    assert_eq!(result, ApplyCompactionResult::Stale);
    runner.invalidate("old context"); assert_eq!(ctx.get_message_revision().unwrap_err().message, "old context"); assert!(ctx.abort(None).is_err());
    for access in [ExtensionContext::is_idle as fn(&ExtensionContext) -> bool, ExtensionContext::is_compacting, ExtensionContext::is_project_trusted] {
        let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| access(&ctx))).unwrap_err();
        assert_eq!(error.downcast_ref::<ExtensionFailure>().unwrap().message, "old context");
    }
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx.get_system_prompt())).is_err());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx.get_registered_mcp_servers())).is_err());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx.session_manager.get_entries())).is_err());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx.session_manager.session_id())).is_err());
}

#[tokio::test]
async fn before_agent_start_keeps_prompt_chaining_with_bound_live_context() {
    let actions = Arc::new(ContextActions { revision: std::sync::atomic::AtomicU64::new(1), aborted: Mutex::new(None) });
    let mut runner = runner(vec![extension("first", EventKind::BeforeAgentStart, prompt("1")), extension("second", EventKind::BeforeAgentStart, prompt("2"))]);
    runner.bind_context_actions(actions).unwrap();
    assert_eq!(runner.emit_before_agent_start(before()).await.unwrap().unwrap().system_prompt.as_deref(), Some("base12"));
}

#[test]
fn compaction_signal_is_inherited_within_one_context_not_across_invocations() {
    let actions = Arc::new(ContextActions { revision: std::sync::atomic::AtomicU64::new(0), aborted: Mutex::new(None) });
    let mut runner = runner(vec![]);
    runner.bind_context_actions(actions.clone()).unwrap();
    let first = runner.create_context().unwrap();
    let second = runner.create_context().unwrap();
    first.begin_compaction(BeginCompactionOptions { reason: CompactionReason::Extension }).unwrap().unwrap().abort();
    let update = || UpdateCompactionOptions { reason: CompactionReason::Extension, signal: None, delta: None, text: None };
    first.update_compaction(update()).unwrap();
    second.update_compaction(update()).unwrap();
    first.end_compaction(EndCompactionOptions { reason: CompactionReason::Extension, signal: None, aborted: None, error_message: None }).unwrap();
    assert_eq!(actions.revision.load(std::sync::atomic::Ordering::SeqCst), 2);
}

struct KernelCapabilities(bool);
impl ExtensionKernelTools for KernelCapabilities {
    fn invoke_scope(&self) -> bool { self.0 }
    fn describe<'a>(&'a self, _: &'a [String]) -> ExtensionFuture<'a, JsonValue> { Box::pin(async { Ok(JsonValue::Null) }) }
    fn invoke(&self, _: KernelToolInvokeRequest, _: KernelToolInvokeOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Ok(JsonValue::Null) }) }
}

#[tokio::test]
async fn concurrent_context_reload_requests_share_one_host_operation() {
    let actions = Arc::new(ContextActions { revision: std::sync::atomic::AtomicU64::new(0), aborted: Mutex::new(None) });
    let mut runner = runner(vec![]);
    runner.bind_context_actions(actions.clone()).unwrap();
    let first = runner.create_context().unwrap();
    let second = runner.create_context().unwrap();
    let (one, two) = tokio::join!(first.request_reload(), second.request_reload());
    one.unwrap();
    two.unwrap();
    assert_eq!(actions.revision.load(std::sync::atomic::Ordering::SeqCst), 1);
    first.request_reload().await.unwrap();
    assert_eq!(actions.revision.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn nested_kernel_invocations_restore_outer_capability_scope() {
    use maho_ext_host::kernel_tools_context::*;
    let actions = Arc::new(ContextActions { revision: std::sync::atomic::AtomicU64::new(0), aborted: Mutex::new(None) });
    let mut runner = runner(vec![]);
    runner.bind_context_actions(actions).unwrap();
    with_kernel_tools(Arc::new(KernelCapabilities(true)), async {
        assert!(runner.create_context().unwrap().kernel_tools().unwrap().unwrap().invoke_scope());
        with_kernel_tools(Arc::new(KernelCapabilities(false)), async {
            assert!(!runner.create_context().unwrap().kernel_tools().unwrap().unwrap().invoke_scope());
        }).await;
        assert!(current_kernel_tools().unwrap().invoke_scope());
    }).await;
    assert!(current_kernel_tools().is_none());
    assert!(runner.create_context().unwrap().kernel_tools().unwrap().is_none());
}

#[tokio::test]
async fn provider_request_metadata_reaches_handlers_while_payloads_chain() {
    let handler: ExtensionHandler = Arc::new(|event, _| Box::pin(async move {
        let ExtensionEvent::BeforeProviderRequest { payload, headers, .. } = event else { panic!("wrong event") };
        assert_eq!(headers.as_ref().unwrap()["X-Fixture"].as_deref(), Some("request"));
        Ok(EventResult::ProviderPayload(JsonValue::String(format!("{}:next", payload.as_str().unwrap()))))
    }));
    let mut runner = runner(vec![extension("metadata", EventKind::BeforeProviderRequest, handler)]);
    let headers = [(String::from("X-Fixture"), Some(String::from("request")))].into_iter().collect();
    let result = runner.emit_before_provider_request_with_metadata(JsonValue::String("payload".into()), None, Some(headers), None).await.unwrap();
    assert_eq!(result, JsonValue::String("payload:next".into()));
}

#[tokio::test]
async fn prepared_provider_request_chains_local_transforms_and_rejects_stale_callbacks() {
    let payload: ExtensionHandler = Arc::new(|event, _| Box::pin(async move {
        let ExtensionEvent::BeforeProviderRequest { payload, .. } = event else { panic!("payload event") };
        Ok(EventResult::ProviderPayload(JsonValue::String(format!("{}:next", payload.as_str().unwrap()))))
    }));
    let headers: ExtensionHandler = Arc::new(|event, _| Box::pin(async move {
        let ExtensionEvent::BeforeProviderHeaders { headers } = event else { panic!("headers event") };
        headers.insert("X-Prepared".into(), Some("yes".into()));
        Ok(EventResult::None)
    }));
    let runner = runner(vec![extension("owner", EventKind::BeforeProviderRequest, payload.clone()),
        extension("other", EventKind::BeforeProviderRequest, payload), extension("headers", EventKind::BeforeProviderHeaders, headers)]);
    let preparation = runner.prepare_provider_request(Vec::new(), Some("owner".into())).await.unwrap();
    assert!(preparation.messages.is_empty());
    assert_eq!((preparation.transform_payload)(JsonValue::String("base".into())).await.unwrap(), JsonValue::String("base:next".into()));
    assert_eq!((preparation.transform_headers)(BTreeMap::new()).await.unwrap()["X-Prepared"].as_deref(), Some("yes"));
    runner.invalidate("reloaded");
    assert_eq!((preparation.transform_payload)(JsonValue::Null).await.unwrap_err().message, "reloaded");
    assert_eq!((preparation.transform_headers)(BTreeMap::new()).await.unwrap_err().message, "reloaded");
}

#[tokio::test]
async fn handler_provider_preparation_excludes_its_owner_without_reentering_session_actions() {
    let outer: ExtensionHandler = Arc::new(|_, ctx| Box::pin(async move {
        let request = ctx.prepare_provider_request(Vec::new()).await?;
        let payload = (request.transform_payload)(JsonValue::String("base".into())).await?;
        assert_eq!(payload, JsonValue::String("base:other".into()));
        Ok(EventResult::None)
    }));
    let owner_payload: ExtensionHandler = Arc::new(|_, _| Box::pin(async { panic!("owner must be excluded") }));
    let other_payload: ExtensionHandler = Arc::new(|event, _| Box::pin(async move {
        let ExtensionEvent::BeforeProviderRequest { payload, .. } = event else { panic!("payload event") };
        Ok(EventResult::ProviderPayload(JsonValue::String(format!("{}:other", payload.as_str().unwrap()))))
    }));
    let mut owner = extension("owner", EventKind::AgentStart, outer);
    owner.handlers.insert(EventKind::BeforeProviderRequest, vec![owner_payload]);
    let mut runner = runner(vec![owner, extension("other", EventKind::BeforeProviderRequest, other_payload)]);
    runner.bind_context_actions(Arc::new(ContextActions { revision: std::sync::atomic::AtomicU64::new(0), aborted: Mutex::new(None) })).unwrap();
    runner.emit(ExtensionEvent::AgentStart).await.unwrap();
}

struct CommandActions(Mutex<Vec<String>>);
impl ExtensionCommandContextActions for CommandActions {
    fn wait_for_idle(&self) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    fn new_session(&self, _: NewSessionOptions) -> ExtensionFuture<'_, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult { cancelled: true }) }) }
    fn fork<'a>(&'a self, _: &'a str, _: ForkOptions) -> ExtensionFuture<'a, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult { cancelled: false }) }) }
    fn navigate_tree<'a>(&'a self, target: &'a str, _: ExtensionTreeNavigationOptions) -> ExtensionFuture<'a, SessionNavigationResult> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(target.into()); Box::pin(async { Ok(SessionNavigationResult { cancelled: false }) })
    }
    fn edit_assistant_message<'a>(&'a self, _: &'a str, _: &'a str, _: EditMessageOptions) -> ExtensionFuture<'a, EditMessageResult> { Box::pin(async { Ok(EditMessageResult { unchanged: Some(true), ..Default::default() }) }) }
    fn edit_user_message<'a>(&'a self, _: &'a str, _: &'a str, _: EditMessageOptions) -> ExtensionFuture<'a, EditMessageResult> { Box::pin(async { Ok(EditMessageResult { entry_id: Some("edited".into()), ..Default::default() }) }) }
    fn switch_session<'a>(&'a self, _: &'a str, _: SwitchSessionOptions) -> ExtensionFuture<'a, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult { cancelled: false }) }) }
    fn reload(&self) -> ExtensionFuture<'_, ()> { Box::pin(async move { self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("reload".into()); Ok(()) }) }
}
#[tokio::test]
async fn command_reload_requests_share_the_same_pending_operation() {
    let runner = runner(vec![]);
    let actions = Arc::new(CommandActions(Mutex::new(vec![])));
    let first = runner.create_command_context(actions.clone()).unwrap();
    let second = runner.create_command_context(actions.clone()).unwrap();
    let (one, two) = tokio::join!(first.reload(), second.reload());
    one.unwrap(); two.unwrap();
    assert_eq!(*actions.0.lock().unwrap(), ["reload"]);
    first.reload().await.unwrap();
    assert_eq!(*actions.0.lock().unwrap(), ["reload", "reload"]);
}
#[tokio::test]
async fn command_invocation_uses_command_capable_context_without_changing_legacy_handlers() {
    let mut api = ExtensionApi::new(LoadedExtension::new("commands", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    api.register_command_with_context("jump", None, None, Arc::new(|args, ctx| Box::pin(async move { ctx.navigate_tree(args, ExtensionTreeNavigationOptions::default()).await?; Ok(()) })));
    let runner = runner(vec![api.registered]); let actions = Arc::new(CommandActions(Mutex::new(vec![])));
    let ctx = runner.create_command_context(actions.clone()).unwrap();
    runner.invoke_command("jump", "leaf", &ctx).await.unwrap();
    assert_eq!(*actions.0.lock().unwrap(), ["leaf"]);
    assert!(ctx.new_session(NewSessionOptions::default()).await.unwrap().cancelled);
    assert!(!ctx.fork("entry", ForkOptions::default()).await.unwrap().cancelled);
    assert_eq!(ctx.edit_assistant_message("entry", "text", EditMessageOptions::default()).await.unwrap().unchanged, Some(true));
    assert_eq!(ctx.edit_user_message("entry", "text", EditMessageOptions::default()).await.unwrap().entry_id.as_deref(), Some("edited"));
    ctx.switch_session("next", SwitchSessionOptions::default()).await.unwrap(); ctx.reload().await.unwrap(); ctx.wait_for_idle().await.unwrap();
    assert!(runner.invoke_command("missing", "", &ctx).await.is_err());
    runner.invalidate("replaced command context");
    assert_eq!(ctx.navigate_tree("old leaf", ExtensionTreeNavigationOptions::default()).await.unwrap_err().message, "replaced command context");
    assert_eq!(*actions.0.lock().unwrap(), ["leaf", "reload"]);
}
#[tokio::test]
async fn question_without_ui_returns_unavailable_and_preserves_unanswered_ids() {
    let ctx = context();
    let response = ctx.ui.question(QuestionRequest { request_id: "request".into(), questions: vec![Question { id: "choice".into(), header: "Choice".into(), question: "Select".into(), options: vec![], multi_select: false }], wait_for_answer: false, timeout_ms: 1000 }, QuestionOptions::default()).await.unwrap();
    assert_eq!(response.status, QuestionStatus::Unavailable); assert_eq!(response.unanswered, ["choice"]);
    assert!(ctx.ui.editor("Edit", None).await.is_err()); assert!(ctx.ui.set_working_visible(false).is_err());
}
