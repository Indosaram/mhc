use maho_ai::types::{ModelThinkingLevel, ThinkingLevel};
use maho_ext_api::*;
use maho_ext_reasoning::{Reasoning, ReasoningHost, ThinkingPreferences};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};

struct Host {
    level: Mutex<ModelThinkingLevel>,
    last_on: Mutex<Option<ModelThinkingLevel>>,
    remembered: Mutex<Option<ModelThinkingLevel>>,
    global: Mutex<Option<ModelThinkingLevel>>,
    operations: Mutex<Vec<&'static str>>,
}
impl Host {
    fn new(level: ModelThinkingLevel, last_on: Option<ModelThinkingLevel>, remembered: Option<ModelThinkingLevel>, global: Option<ModelThinkingLevel>) -> Self {
        Self { level: Mutex::new(level), last_on: Mutex::new(last_on), remembered: Mutex::new(remembered), global: Mutex::new(global), operations: Mutex::new(Vec::new()) }
    }
}
impl ReasoningHost for Host {
    fn level(&self, _: &ExtensionContext) -> Result<ModelThinkingLevel, ExtensionFailure> { Ok(*self.level.lock().expect("level")) }
    fn set_level(&self, _: &ExtensionContext, level: ModelThinkingLevel) -> Result<(), ExtensionFailure> {
        self.operations.lock().expect("operations").push("set");
        *self.level.lock().expect("level") = level;
        *self.remembered.lock().expect("remembered") = Some(level);
        Ok(())
    }
    fn preferences(&self, _: &ExtensionContext, _: &Model) -> Result<ThinkingPreferences, ExtensionFailure> {
        Ok(ThinkingPreferences { last_on: *self.last_on.lock().expect("last_on"), remembered: *self.remembered.lock().expect("remembered"), global: *self.global.lock().expect("global") })
    }
    fn remember_last_on<'a>(&'a self, _: &'a ExtensionContext, _: &'a Model, level: ModelThinkingLevel) -> ExtensionFuture<'a, ()> {
        Box::pin(async move { self.operations.lock().expect("operations").push("remember"); *self.last_on.lock().expect("last_on") = Some(level); Ok(()) })
    }
    fn restore_on<'a>(&'a self, _: &'a ExtensionContext, _: &'a Model, level: ThinkingLevel) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.operations.lock().expect("operations").push("restore");
            *self.level.lock().expect("level") = level.into();
            *self.remembered.lock().expect("remembered") = Some(level.into());
            *self.global.lock().expect("global") = Some(level.into());
            Ok(())
        })
    }
}

#[derive(Default)]
struct Ui(Mutex<Vec<(String, NotificationType)>>);
impl ExtensionUi for Ui {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, message: &str, kind: NotificationType) { self.0.lock().expect("ui").push((message.into(), kind)); }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err(ExtensionFailure::new("No custom UI")) }) }
    fn theme(&self) -> Theme { Theme::default() }
}

struct Session;
impl ToolSessionManager for Session {
    fn session_id(&self) -> &str { "session" }
    fn session_file(&self) -> Option<&Path> { None }
}
impl SessionManager for Session {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}
struct Registry;
impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
}
fn ctx(model: Model, ui: Arc<Ui>) -> ExtensionContext {
    ExtensionContext {
        ui, mode: ExtensionMode::Tui, has_ui: true, cwd: "/tmp".into(), agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(Session), model_registry: Arc::new(Registry), model: Some(model), thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true), is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(String::new), get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default(),
    }
}
fn model(reasoning: bool, thinking_level_map: Option<Value>) -> Model {
    let mut value = json!({"id":"faux-reasoner","name":"Faux","api":"openai-completions","provider":"faux","baseUrl":"","reasoning":reasoning,"input":["text"],"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},"contextWindow":1000,"maxTokens":100});
    if let Some(map) = thinking_level_map { value["thinkingLevelMap"] = map; }
    serde_json::from_value(value).expect("model")
}
fn graded_full() -> Model { model(true, Some(json!({"xhigh":"xhigh","max":"max"}))) }
fn graded_no_xhigh() -> Model { model(true, Some(json!({"xhigh":null,"max":null}))) }
fn on_off() -> Model { model(true, Some(json!({"minimal":null,"low":null,"medium":null,"xhigh":null,"max":null}))) }
fn always_on() -> Model { model(true, Some(json!({"off":null,"xhigh":null,"max":null}))) }
fn plain() -> Model { model(false, None) }
const KEY: &str = "faux/faux-reasoner";

struct Harness { api: ExtensionApi, host: Arc<Host>, ui: Arc<Ui> }
async fn harness(model: Model, host: Host) -> (Harness, ExtensionContext) {
    let ui = Arc::new(Ui::default());
    let context = ctx(model, ui.clone());
    let host = Arc::new(host);
    let mut api = ExtensionApi::new(LoadedExtension::new("reasoning", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    Reasoning { host: host.clone() }.register(&mut api);
    let start = api.registered.handlers[&EventKind::SessionStart][0].clone();
    start(&mut ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None }), &context).await.expect("session start");
    (Harness { api, host, ui }, context)
}
fn note(message: &str, kind: NotificationType) -> (String, NotificationType) { (message.to_string(), kind) }
async fn run(h: &Harness, ctx: &ExtensionContext, name: &str, args: &str) -> (String, NotificationType) {
    let handler = h.api.registered.commands.iter().find(|command| command.name == name).expect("command").handler.clone();
    handler(args, ctx).await.expect("command");
    h.ui.0.lock().expect("ui").last().cloned().expect("notification")
}
async fn completions(h: &Harness, name: &str, prefix: &str) -> Option<Vec<String>> {
    let items = (h.api.registered.command_argument_completions[name])(prefix).await.expect("completions")?;
    Some(items.into_iter().map(|item| item.value).collect())
}

#[tokio::test]
async fn reasoning_status_off_for_graded_at_off() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Off, None, None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "").await, note("Reasoning: off.", NotificationType::Info));
}
#[tokio::test]
async fn reasoning_status_on_for_graded_at_high() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "").await, note("Reasoning: on (high).", NotificationType::Info));
}
#[tokio::test]
async fn reasoning_status_off_for_non_reasoning() {
    let (h, c) = harness(plain(), Host::new(ModelThinkingLevel::Off, None, None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "").await, note("Reasoning: off.", NotificationType::Info));
}
#[tokio::test]
async fn reasoning_on_restores_remembered_level() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Off, None, Some(ModelThinkingLevel::Xhigh), None)).await;
    assert_eq!(run(&h, &c, "reasoning", "on").await, note("Reasoning: on (xhigh).", NotificationType::Info));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::Xhigh);
}
#[tokio::test]
async fn reasoning_on_roundtrips_with_and_without_restart() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Medium, None, None, Some(ModelThinkingLevel::Minimal))).await;
    run(&h, &c, "efforts", "high").await;
    run(&h, &c, "reasoning", "off").await;
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::Off);
    run(&h, &c, "reasoning", "on").await;
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::High);
    let (restarted, rc) = harness(graded_full(), Host::new(ModelThinkingLevel::Off, Some(ModelThinkingLevel::High), Some(ModelThinkingLevel::Off), Some(ModelThinkingLevel::Minimal))).await;
    run(&restarted, &rc, "reasoning", "on").await;
    assert_eq!(restarted.host.level(&rc).expect("level"), ModelThinkingLevel::High);
}
#[tokio::test]
async fn reasoning_on_falls_back_to_non_off_global() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Off, None, None, Some(ModelThinkingLevel::Low))).await;
    assert_eq!(run(&h, &c, "reasoning", "on").await, note("Reasoning: on (low).", NotificationType::Info));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::Low);
}
#[tokio::test]
async fn reasoning_on_falls_back_to_medium() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Off, None, None, Some(ModelThinkingLevel::Off))).await;
    assert_eq!(run(&h, &c, "reasoning", "on").await, note("Reasoning: on (medium).", NotificationType::Info));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::Medium);
}
#[tokio::test]
async fn reasoning_on_clamps_stale_level_without_rewriting_preference() {
    let (h, c) = harness(on_off(), Host::new(ModelThinkingLevel::Off, Some(ModelThinkingLevel::Low), None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "on").await, note("Reasoning: on (high).", NotificationType::Info));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::High);
    assert_eq!(*h.host.last_on.lock().expect("last_on"), Some(ModelThinkingLevel::Low));
}
#[tokio::test]
async fn reasoning_on_keeps_current_level_when_already_on() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "on").await, note("Reasoning: on (high).", NotificationType::Info));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::High);
}
#[tokio::test]
async fn reasoning_on_errors_for_non_reasoning() {
    let (h, c) = harness(plain(), Host::new(ModelThinkingLevel::Off, None, None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "on").await, note(&format!("Model {KEY} does not support reasoning."), NotificationType::Error));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::Off);
}
#[tokio::test]
async fn reasoning_off_turns_off_graded_and_saves_last_on() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "off").await, note("Reasoning: off.", NotificationType::Info));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::Off);
    assert_eq!(*h.host.last_on.lock().expect("last_on"), Some(ModelThinkingLevel::High));
}
#[tokio::test]
async fn reasoning_off_is_idempotent_for_non_reasoning() {
    let (h, c) = harness(plain(), Host::new(ModelThinkingLevel::Off, None, None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "off").await, note("Reasoning: off.", NotificationType::Info));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::Off);
}
#[tokio::test]
async fn reasoning_off_refuses_always_on() {
    let (h, c) = harness(always_on(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "off").await, note(&format!("Reasoning cannot be disabled for {KEY}."), NotificationType::Error));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::High);
}
#[tokio::test]
async fn reasoning_rejects_unknown_argument() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "maybe").await, note("Usage: /reasoning [on|off]", NotificationType::Error));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::High);
}
#[tokio::test]
async fn reasoning_rejects_extra_arguments() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "on now").await, note("Usage: /reasoning [on|off]", NotificationType::Error));
}
#[tokio::test]
async fn reasoning_whitespace_argument_is_status() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Medium, None, None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "    ").await, note("Reasoning: on (medium).", NotificationType::Info));
}
#[tokio::test]
async fn reasoning_rejects_case_shifted_argument() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(run(&h, &c, "reasoning", "ON").await, note("Usage: /reasoning [on|off]", NotificationType::Error));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::High);
}
#[tokio::test]
async fn efforts_status_lists_full_ladder() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "").await, note("Reasoning effort: high. Available: minimal, low, medium, high, xhigh, max.", NotificationType::Info));
}
#[tokio::test]
async fn efforts_status_omits_unsupported_extended_levels() {
    let (h, c) = harness(graded_no_xhigh(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "").await, note("Reasoning effort: high. Available: minimal, low, medium, high.", NotificationType::Info));
}
#[tokio::test]
async fn efforts_status_reports_off_when_disabled() {
    let (h, c) = harness(graded_no_xhigh(), Host::new(ModelThinkingLevel::Off, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "").await, note("Reasoning effort: off. Available: minimal, low, medium, high.", NotificationType::Info));
}
#[tokio::test]
async fn efforts_status_refuses_on_off_only() {
    let (h, c) = harness(on_off(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "").await, note(&format!("Reasoning effort is not configurable for {KEY}; this model supports on/off only. Use /reasoning on or /reasoning off."), NotificationType::Error));
}
#[tokio::test]
async fn efforts_status_refuses_non_reasoning() {
    let (h, c) = harness(plain(), Host::new(ModelThinkingLevel::Off, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "").await, note(&format!("Model {KEY} does not support reasoning."), NotificationType::Error));
}
#[tokio::test]
async fn efforts_sets_supported_level() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Off, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "high").await, note("Reasoning effort: high. Available: minimal, low, medium, high, xhigh, max.", NotificationType::Info));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::High);
}
#[tokio::test]
async fn efforts_sets_xhigh_when_supported() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Off, None, None, None)).await;
    run(&h, &c, "efforts", "xhigh").await;
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::Xhigh);
}
#[tokio::test]
async fn efforts_persists_chosen_level_as_remembered() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Off, None, None, None)).await;
    run(&h, &c, "efforts", "high").await;
    assert_eq!(*h.host.remembered.lock().expect("remembered"), Some(ModelThinkingLevel::High));
}
#[tokio::test]
async fn efforts_rejects_unsupported_with_available_list() {
    let (h, c) = harness(graded_no_xhigh(), Host::new(ModelThinkingLevel::Medium, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "xhigh").await, note(&format!("Reasoning effort \"xhigh\" is not supported by {KEY}. Available: minimal, low, medium, high."), NotificationType::Error));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::Medium);
}
#[tokio::test]
async fn efforts_rejects_unsupported_on_always_on() {
    let (h, c) = harness(always_on(), Host::new(ModelThinkingLevel::Medium, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "max").await, note(&format!("Reasoning effort \"max\" is not supported by {KEY}. Available: minimal, low, medium, high."), NotificationType::Error));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::Medium);
}
#[tokio::test]
async fn efforts_refuses_level_on_on_off_only() {
    let (h, c) = harness(on_off(), Host::new(ModelThinkingLevel::Off, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "high").await, note(&format!("Reasoning effort is not configurable for {KEY}; this model supports on/off only. Use /reasoning on or /reasoning off."), NotificationType::Error));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::Off);
}
#[tokio::test]
async fn efforts_refuses_level_on_non_reasoning() {
    let (h, c) = harness(plain(), Host::new(ModelThinkingLevel::Off, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "high").await, note(&format!("Model {KEY} does not support reasoning."), NotificationType::Error));
}
#[tokio::test]
async fn efforts_rejects_unknown_token_with_usage() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Medium, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "turbo").await, note("Usage: /efforts [minimal|low|medium|high|xhigh|max]", NotificationType::Error));
}
#[tokio::test]
async fn efforts_rejects_case_shifted_token_with_usage() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Medium, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "HIGH").await, note("Usage: /efforts [minimal|low|medium|high|xhigh|max]", NotificationType::Error));
}
#[tokio::test]
async fn efforts_rejects_extra_arguments_with_usage() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Medium, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "high extra-arg").await, note("Usage: /efforts [minimal|low|medium|high|xhigh|max]", NotificationType::Error));
}
#[tokio::test]
async fn efforts_rejects_unicode_token_with_usage() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Medium, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "高").await, note("Usage: /efforts [minimal|low|medium|high|xhigh|max]", NotificationType::Error));
}
#[tokio::test]
async fn efforts_whitespace_argument_is_status() {
    let (h, c) = harness(graded_no_xhigh(), Host::new(ModelThinkingLevel::Medium, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "   ").await, note("Reasoning effort: medium. Available: minimal, low, medium, high.", NotificationType::Info));
}
#[tokio::test]
async fn efforts_rejects_off_pseudo_level() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::Medium, None, None, None)).await;
    assert_eq!(run(&h, &c, "efforts", "off").await, note(&format!("Reasoning effort \"off\" is not supported by {KEY}. Available: minimal, low, medium, high, xhigh, max."), NotificationType::Error));
    assert_eq!(h.host.level(&c).expect("level"), ModelThinkingLevel::Medium);
}
#[tokio::test]
async fn completions_offer_on_off_for_reasoning() {
    let (h, _c) = harness(graded_full(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(completions(&h, "reasoning", "").await, Some(vec!["on".into(), "off".into()]));
}
#[tokio::test]
async fn completions_offer_only_supported_efforts() {
    let (h, _c) = harness(graded_no_xhigh(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(completions(&h, "efforts", "").await, Some(vec!["minimal".into(), "low".into(), "medium".into(), "high".into()]));
}
#[tokio::test]
async fn completions_include_extended_levels_only_when_supported() {
    let (h, _c) = harness(graded_full(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(completions(&h, "efforts", "").await, Some(vec!["minimal".into(), "low".into(), "medium".into(), "high".into(), "xhigh".into(), "max".into()]));
}
#[tokio::test]
async fn completions_filter_by_prefix() {
    let (h, _c) = harness(graded_full(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert_eq!(completions(&h, "efforts", "m").await, Some(vec!["minimal".into(), "medium".into(), "max".into()]));
}
#[tokio::test]
async fn no_thinking_alias_is_registered() {
    let (h, _c) = harness(graded_full(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    assert!(h.api.registered.commands.iter().all(|command| command.name != "thinking"));
}
#[tokio::test]
async fn classifies_current_model_not_session_first_model() {
    let (h, c) = harness(graded_full(), Host::new(ModelThinkingLevel::High, None, None, None)).await;
    run(&h, &c, "efforts", "high").await;
    let switched = ctx(plain(), h.ui.clone());
    assert_eq!(run(&h, &switched, "efforts", "high").await, note(&format!("Model {KEY} does not support reasoning."), NotificationType::Error));
    assert_eq!(completions(&h, "efforts", "").await, None);
}
