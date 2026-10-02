use maho_ext_api::*;
use std::sync::{Arc, Mutex};

#[test]
fn every_event_kind_roundtrips_without_duplicate_names() {
    let names: std::collections::BTreeSet<_> = EventKind::ALL.iter().map(|kind| kind.as_str()).collect();
    assert_eq!(names.len(), 43);
    for kind in EventKind::ALL { assert_eq!(EventKind::parse(kind.as_str()), Some(kind)); }
    assert_eq!(EventKind::parse("missing"), None);
}

#[test]
fn event_bus_subscription_drop_unsubscribes_and_keeps_order() {
    let bus = EventBus::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let first_seen = Arc::clone(&seen);
    let first = bus.on("x", Arc::new(move |_| first_seen.lock().unwrap().push(1)));
    let second_seen = Arc::clone(&seen);
    let second = bus.on("x", Arc::new(move |_| second_seen.lock().unwrap().push(2)));
    bus.emit("x", &JsonValue::Null);
    drop(first);
    bus.emit("x", &JsonValue::Null);
    assert_eq!(*seen.lock().unwrap(), vec![1, 2, 2]);
    drop(second);
}

#[test]
fn flag_defaults_are_first_wins_and_cli_values_override() {
    let runtime = ExtensionRuntime::default();
    let mut api = ExtensionApi::new(LoadedExtension::new("first", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), runtime.clone());
    api.register_flag("enabled", FlagType::Boolean { default: Some(true) }, None);
    api.register_flag("enabled", FlagType::Boolean { default: Some(false) }, None);
    assert_eq!(api.get_flag("enabled"), Some(FlagValue::Boolean(true)));
    api.set_flag("enabled", FlagValue::Boolean(false));
    assert_eq!(api.get_flag("enabled"), Some(FlagValue::Boolean(false)));
}

#[test]
fn runtime_invalidation_preserves_first_reason_and_rejects_actions() {
    let runtime = ExtensionRuntime::default();
    runtime.invalidate("replacement");
    runtime.invalidate("later");
    assert_eq!(runtime.assert_active().unwrap_err().message, "replacement");
}

#[test]
fn stale_legacy_registration_throws_without_mutating_extension() {
    let runtime = ExtensionRuntime::default();
    let mut api = api(runtime.clone());
    runtime.invalidate("replacement");
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        api.register_flag("late", FlagType::Boolean { default: Some(true) }, None);
    })).unwrap_err();
    assert_eq!(failure.downcast_ref::<ExtensionFailure>().unwrap().message, "replacement");
    assert!(api.registered.flags.is_empty());
}

fn api(runtime: ExtensionRuntime) -> ExtensionApi {
    ExtensionApi::new(LoadedExtension::new("test", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), runtime)
}

#[test]
fn captured_api_event_bus_rejects_access_after_runtime_replacement() {
    let runtime = ExtensionRuntime::default();
    let api = api(runtime.clone());
    let captured = api.events.clone();
    runtime.invalidate("replacement");
    let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| captured.emit("late", &JsonValue::Null))).unwrap_err();
    assert_eq!(error.downcast_ref::<ExtensionFailure>().unwrap().message, "replacement");
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| captured.on("late", Arc::new(|_| {})))).is_err());
}
#[derive(Default)]
struct Providers(Mutex<Vec<String>>);
impl ExtensionProviderActions for Providers {
    fn register_provider(&self, registration: ProviderRegistration, path: &str) -> Result<(), ExtensionFailure> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(format!("{}:{path}", registration.name())); Ok(())
    }
    fn unregister_provider(&self, name: &str, _: &str) -> Result<(), ExtensionFailure> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(format!("remove:{name}")); Ok(())
    }
}
#[test]
fn providers_queue_in_order_then_register_immediately_after_binding() {
    let runtime = ExtensionRuntime::default(); let api = api(runtime.clone());
    api.register_provider("first", ProviderConfig::default()).unwrap();
    api.register_provider("removed", ProviderConfig::default()).unwrap();
    api.unregister_provider("removed").unwrap();
    api.register_provider("second", ProviderConfig::default()).unwrap();
    let providers = Arc::new(Providers::default()); runtime.bind_providers(providers.clone()).unwrap();
    api.register_provider("third", ProviderConfig::default()).unwrap(); api.unregister_provider("first").unwrap();
    assert_eq!(*providers.0.lock().unwrap(), ["first:test", "second:test", "third:test", "remove:first"]);
}

#[test]
fn factory_runtime_stages_flags_and_provider_changes_until_commit() {
    let runtime = ExtensionRuntime::default();
    let providers = Arc::new(Providers::default());
    runtime.bind_providers(providers.clone()).unwrap();
    let scope = runtime.registration_scope();
    let mut api = api(scope.clone());
    api.register_flag("pending", FlagType::String { default: Some("default".into()) }, None);
    api.register_provider("one", ProviderConfig::default()).unwrap();
    api.unregister_provider("one").unwrap();
    assert_eq!(api.get_flag("pending"), Some(FlagValue::String("default".into())));
    assert_eq!(runtime.get_flag("pending"), None);
    assert!(providers.0.lock().unwrap().is_empty());
    runtime.set_flag("pending", FlagValue::String("external".into()));
    scope.commit_registration().unwrap();
    assert_eq!(runtime.get_flag("pending"), Some(FlagValue::String("external".into())));
    assert_eq!(*providers.0.lock().unwrap(), ["one:test", "remove:one"]);
    api.register_provider("two", ProviderConfig::default()).unwrap();
    assert_eq!(providers.0.lock().unwrap().last().unwrap(), "two:test");
}

#[test]
fn discarded_factory_does_not_revert_another_runtime_writer() {
    let runtime = ExtensionRuntime::default();
    let scope = runtime.registration_scope();
    let mut api = api(scope.clone());
    api.register_flag("pending", FlagType::Boolean { default: Some(true) }, None);
    let leaked = api.register_read_classifier(Arc::new(|_, _| Some(CompactReadClassification { kind: CompactReadKind::Docs, label: "discard".into(), headline: None }))).unwrap();
    let good = ExtensionApi::new(LoadedExtension::new("good", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), runtime.clone());
    let retained = good.register_read_classifier(Arc::new(|_, _| Some(CompactReadClassification { kind: CompactReadKind::Docs, label: "keep".into(), headline: None }))).unwrap();
    runtime.set_flag("external", FlagValue::Boolean(false));
    scope.invalidate_registration("failed");
    assert_eq!(runtime.get_flag("pending"), None);
    assert_eq!(runtime.get_flag("external"), Some(FlagValue::Boolean(false)));
    assert!(scope.commit_registration().is_err());
    assert_eq!(runtime.classify_read(std::path::Path::new("/docs"), std::path::Path::new("/tmp")).unwrap().label, "keep");
    drop((leaked, retained));
}
#[test]
fn invalidation_rejects_provider_changes_and_discards_pending_registrations() {
    let runtime = ExtensionRuntime::default(); let api = api(runtime.clone());
    api.register_provider("queued", ProviderConfig::default()).unwrap(); runtime.invalidate("old generation");
    assert_eq!(api.unregister_provider("queued").unwrap_err().message, "old generation");
    assert!(runtime.bind_providers(Arc::new(Providers::default())).is_err());
}
#[test]
fn read_classifiers_are_first_wins_and_unsubscribe_on_drop_or_invalidation() {
    let runtime = ExtensionRuntime::default(); let api = api(runtime.clone());
    let make = |label: &'static str| -> ReadClassifier { Arc::new(move |_, _| Some(CompactReadClassification { kind: CompactReadKind::Docs, label: label.into(), headline: None })) };
    let first = api.register_read_classifier(make("first")).unwrap();
    let _second = api.register_read_classifier(make("second")).unwrap();
    assert_eq!(runtime.classify_read(std::path::Path::new("/docs"), &api.cwd).unwrap().label, "first");
    drop(first);
    assert_eq!(runtime.classify_read(std::path::Path::new("/docs"), &api.cwd).unwrap().label, "second");
    runtime.invalidate("reload"); assert!(runtime.classify_read(std::path::Path::new("/docs"), &api.cwd).is_none());
}
#[test]
fn classifier_panics_do_not_block_later_classifiers() {
    let runtime = ExtensionRuntime::default(); let api = api(runtime.clone());
    let _bad = api.register_read_classifier(Arc::new(|_, _| panic!("classifier"))).unwrap();
    let _good = api.register_read_classifier(Arc::new(|_, _| Some(CompactReadClassification { kind: CompactReadKind::Memory, label: "memory".into(), headline: None }))).unwrap();
    assert_eq!(runtime.classify_read(std::path::Path::new("/memory"), &api.cwd).unwrap().kind, CompactReadKind::Memory);
}
#[test]
fn session_actions_fail_explicitly_before_host_binding() {
    let api = api(ExtensionRuntime::default());
    assert!(api.set_session_name("name").is_err()); assert!(api.get_session_name().is_err());
    assert!(api.get_active_tools().is_err()); assert!(api.set_active_tools(vec![]).is_err());
    assert!(api.get_commands().is_err()); assert!(api.get_thinking_level().is_err());
    assert!(api.set_session_fast_mode(true).is_err());
}

#[test]
fn mcp_registration_requires_the_enabled_transport_endpoint() {
    let mut api = api(ExtensionRuntime::default());
    assert!(api.try_register_mcp_server("stdio", McpServerDeclaration::default()).is_err());
    assert!(api.try_register_mcp_server("http", McpServerDeclaration { transport: Some(McpTransport::Http), url: Some("  ".into()), ..Default::default() }).is_err());
    api.try_register_mcp_server("disabled", McpServerDeclaration { enabled: Some(false), ..Default::default() }).unwrap();
    api.try_register_mcp_server("inferred", McpServerDeclaration { url: Some("https://example.invalid/mcp".into()), ..Default::default() }).unwrap();
    api.try_register_mcp_server("explicit", McpServerDeclaration { transport: Some(McpTransport::Stdio), command: Some("server".into()), url: Some("ignored".into()), ..Default::default() }).unwrap();
    assert_eq!(api.registered.mcp_servers.len(), 3);
    api.runtime.invalidate("reloaded");
    assert!(api.try_register_mcp_server("late", McpServerDeclaration { enabled: Some(false), ..Default::default() }).is_err());
}

#[test]
fn failed_queued_provider_does_not_discard_later_registrations() {
    struct Selective(Providers);
    impl ExtensionProviderActions for Selective {
        fn register_provider(&self, registration: ProviderRegistration, path: &str) -> Result<(), ExtensionFailure> {
            if registration.name() == "bad" { return Err(ExtensionFailure::new("invalid provider")); }
            self.0.register_provider(registration, path)
        }
        fn unregister_provider(&self, name: &str, path: &str) -> Result<(), ExtensionFailure> { self.0.unregister_provider(name, path) }
    }
    let runtime = ExtensionRuntime::default(); let api = api(runtime.clone());
    api.register_provider("bad", ProviderConfig::default()).unwrap(); api.register_provider("good", ProviderConfig::default()).unwrap();
    let actions = Arc::new(Selective(Providers::default())); runtime.bind_providers(actions.clone()).unwrap();
    assert_eq!(*actions.0.0.lock().unwrap(), ["good:test"]);
    let errors = runtime.take_provider_errors(); assert_eq!(errors.len(), 1); assert_eq!(errors[0].extension_path, "test"); assert_eq!(errors[0].event, "register_provider");
    assert!(runtime.take_provider_errors().is_empty());
}

#[test]
fn checked_tool_registration_rejects_reserved_name_and_nonobject_schema() {
    let mut api = api(ExtensionRuntime::default());
    let execute: maho_tools::definition::ToolExecutor = Arc::new(|_| Box::pin(async { Ok(ToolResult::text("ok")) }));
    assert!(api.try_register_tool(ToolDefinition::new("tool_search", "reserved", JsonValue::Object(Default::default()), execute.clone())).is_err());
    assert!(api.try_register_tool(ToolDefinition::new("bad", "bad schema", JsonValue::Null, execute.clone())).is_err());
    api.try_register_tool(ToolDefinition::new("good", "valid", JsonValue::Object(Default::default()), execute.clone())).unwrap();
    assert_eq!(api.registered.tools.len(), 1);
    api.runtime.invalidate("replaced");
    assert!(api.try_register_tool(ToolDefinition::new("later", "stale", JsonValue::Object(Default::default()), execute)).is_err());
}

#[test]
fn ui_prompt_events_expose_their_wire_reason_discriminant() {
    let event = ExtensionEvent::UiPromptStart { kind: UiPromptKind::Input, title: None };
    assert_eq!(event.ui_prompt_reason().unwrap().as_str(), "ui_prompt");
    assert_eq!(event.ui_prompt_start_event(), Some(UiPromptStartEvent { reason: UiPromptReason::UiPrompt, kind: UiPromptKind::Input, title: None }));
    assert!(event.ui_prompt_end_event().is_none());
    assert!(event.system_prompt_change_source().is_none());
}

struct FactoryHost;
impl ExtensionTuiHost for FactoryHost {
    fn request_render(&self) {}
    fn dimensions(&self) -> (u16, u16) { (80, 24) }
}
struct FooterData;
impl ReadonlyFooterDataProvider for FooterData {
    fn get_git_branch(&self) -> Option<String> { Some("work".into()) }
    fn get_extension_statuses(&self) -> std::collections::BTreeMap<String, String> { [("status".into(), "ready".into())].into_iter().collect() }
    fn get_available_provider_count(&self) -> usize { 2 }
    fn on_branch_change(&self, _: Arc<dyn Fn() + Send + Sync>) -> UiUnsubscribe { Box::new(|| {}) }
}
struct FactoryComponent(String);
impl Component for FactoryComponent {
    fn render(&mut self, _: usize) -> Vec<String> { vec![self.0.clone()] }
    fn invalidate(&mut self) {}
}
#[test]
fn footer_factory_receives_tui_and_readonly_footer_data() {
    let factory: FooterComponentFactory = Arc::new(|tui, _, data| {
        Box::new(FactoryComponent(format!("{}:{}:{}", tui.dimensions().0, data.get_git_branch().unwrap(), data.get_available_provider_count())))
    });
    let mut component = factory(&FactoryHost, &Theme::default(), &FooterData);
    assert_eq!(component.render(80), ["80:work:2"]);
}

#[test]
fn provider_extra_body_rejects_nonobjects_before_queuing_registration() {
    let runtime = ExtensionRuntime::default();
    let api = api(runtime.clone());
    let config = ProviderConfig { extra_body: Some(JsonValue::Array(Vec::new())), ..Default::default() };
    assert!(api.register_provider("invalid", config).is_err());
    let providers = Arc::new(Providers::default());
    runtime.bind_providers(providers.clone()).unwrap();
    assert!(providers.0.lock().unwrap().is_empty());
    api.register_provider("valid", ProviderConfig { extra_body: Some(JsonValue::Object(Default::default())), ..Default::default() }).unwrap();
    assert_eq!(*providers.0.lock().unwrap(), ["valid:test"]);
    api.register_provider_object("typed", ProviderObjectConfig {
        extra_body: Some([("enabled".into(), JsonValue::Bool(true))].into_iter().collect()), ..Default::default()
    }).unwrap();
    assert_eq!(*providers.0.lock().unwrap(), ["valid:test", "typed:test"]);
}

#[test]
fn typed_tool_renderer_retains_state_across_render_calls() {
    let renderer: ToolCallRenderer<usize, String> = Arc::new(|args, _, context| {
        context.state += 1;
        Box::new(FactoryComponent(format!("{args}:{}", context.state)))
    });
    let mut context = ToolRenderContext {
        args: "input".into(), tool_call_id: "call".into(), invalidate: std::rc::Rc::new(|| {}),
        last_component: None, state: 0, cwd: "/tmp".into(), execution_started: true,
        args_complete: true, is_partial: false, expanded: false, show_images: false,
        image_protocol: None, is_error: false, has_result: None, spinner_frame: None,
    };
    let mut first = renderer(&"input".into(), &Theme::default(), &mut context);
    assert_eq!(first.render(80), ["input:1"]);
    context.last_component = Some(first);
    let mut second = renderer(&"input".into(), &Theme::default(), &mut context);
    assert_eq!(second.render(80), ["input:2"]);
    let mut api = api(ExtensionRuntime::default());
    api.register_tool_with_renderers(ToolDefinition::new("rendered", "rendered", JsonValue::Object(Default::default()),
        Arc::new(|_| Box::pin(async { Ok(ToolResult::text("complete")) }))), ToolRenderers { render_call: Some(renderer), render_result: None }).unwrap();
    let renderers = api.registered.tool_renderers["rendered"].clone().downcast::<ToolRenderers<usize, String>>().unwrap();
    let mut session = ToolRendererSession { renderers, context };
    assert_eq!(session.render_call(&Theme::default(), 80).unwrap(), ["input:3"]);
    assert!(session.context.last_component.is_some());
}

#[test]
fn invalid_registration_bus_cannot_emit_subscribe_or_clear_shared_handlers() {
    let events = EventBus::default();
    let failed = events.registration_scope();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let _subscription = events.on("shared", Arc::new(move |_| { observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }));
    failed.invalidate_registration();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| failed.emit("shared", &JsonValue::Null))).is_err());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let late_calls = calls.clone();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| failed.on("shared", Arc::new(move |_| { late_calls.fetch_add(10, std::sync::atomic::Ordering::SeqCst); })))).is_err());
    failed.clear();
    events.emit("shared", &JsonValue::Null);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
