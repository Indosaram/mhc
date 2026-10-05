
use maho_ai::types::{AssistantMessage, AssistantMessageEvent, Context, DoneReason, SimpleStreamOptions};
use maho_ai::utils::abort::AbortSignal;
use maho_ai::utils::event_stream::AssistantMessageEventStream;
use maho_ext_api::*;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

pub struct BtwSession {
    pub entries: Vec<SessionEntry>,
}

impl ToolSessionManager for BtwSession {
    fn session_id(&self) -> &str {
        "session"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}

impl SessionManager for BtwSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

pub struct BtwRegistry {
    pub calls: Mutex<Vec<(Context, SimpleStreamOptions)>>,
    pub entered: tokio::sync::watch::Sender<Option<AbortSignal>>,
    pub blocked: bool,
}

impl BtwRegistry {
    pub fn new() -> Self {
        let (entered, _receiver) = tokio::sync::watch::channel(None);
        Self { calls: Mutex::new(Vec::new()), entered, blocked: false }
    }
    pub fn blocked() -> Self {
        let (entered, _receiver) = tokio::sync::watch::channel(None);
        Self { calls: Mutex::new(Vec::new()), entered, blocked: true }
    }
    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }
}

fn assistant_message() -> AssistantMessage {
    serde_json::from_value(serde_json::json!({"role":"assistant","api":"faux","provider":"fixture","model":"m","timestamp":0,"content":[],"stopReason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}})).expect("message")
}

impl ModelRegistry for BtwRegistry {
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
        Box::pin(async { panic!("resolved auth required") })
    }
    fn get_api_key_and_headers<'a>(&'a self, _: &'a Model) -> ExtensionFuture<'a, ResolvedRequestAuth> {
        Box::pin(async { Ok(ResolvedRequestAuth { auth: maho_ai::models::ProviderAuthResult { api_key: Some("side-secret".into()), ..Default::default() }, extra_body: None, upstream_model_id: None, service_tier: None, env: None }) })
    }
    fn stream_simple(&self, _: &Model, context: &Context, options: Option<SimpleStreamOptions>) -> Result<AssistantMessageEventStream, ExtensionFailure> {
        let options = options.expect("options");
        self.entered.send_replace(options.stream.request.signal.clone());
        let mut calls = self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let block = self.blocked && calls.is_empty();
        calls.push((context.clone(), options));
        drop(calls);
        let stream = AssistantMessageEventStream::assistant();
        if !block {
            let message = assistant_message();
            stream.push(AssistantMessageEvent::TextDelta { content_index: 0, delta: "side reply".into(), partial: message.clone() });
            stream.push(AssistantMessageEvent::Done { reason: DoneReason::Stop, message });
        }
        Ok(stream)
    }
}

#[derive(Default)]
pub struct BtwUi {
    pub widgets: Mutex<Vec<(String, Option<WidgetContent>)>>,
    pub inputs: Arc<Mutex<Vec<TerminalInputHandler>>>,
    pub notifications: Mutex<Vec<(String, NotificationType)>>,
}

impl BtwUi {
    pub fn widget_cleared(&self) -> bool {
        self.widgets.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last().is_some_and(|(_, content)| content.is_none())
    }
    pub fn input_count(&self) -> usize {
        self.inputs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }
    pub fn feed_input(&self, data: &str) {
        for handler in self.inputs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() {
            handler(data);
        }
    }
}

impl ExtensionUi for BtwUi {
    fn set_widget(&self, key: &str, content: Option<WidgetContent>, _: ExtensionWidgetOptions) {
        self.widgets.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((key.into(), content));
    }
    fn on_terminal_input(&self, handler: TerminalInputHandler) -> Result<UiUnsubscribe, ExtensionFailure> {
        self.inputs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(handler.clone());
        let inputs = Arc::clone(&self.inputs);
        Ok(Box::new(move || {
            inputs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|existing| !Arc::ptr_eq(existing, &handler));
        }))
    }
    fn notify(&self, message: &str, kind: NotificationType) {
        self.notifications.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((message.into(), kind));
    }
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn set_status(&self, _: &str, _: Option<&str>) {}
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

pub fn model() -> Model {
    serde_json::from_value(serde_json::json!({"id":"m","name":"M","api":"openai-completions","provider":"fixture","baseUrl":"https://example.invalid","reasoning":true,"input":["text"],"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},"contextWindow":10000,"maxTokens":1000})).expect("model")
}

pub fn context(mode: ExtensionMode, has_ui: bool, registry: Arc<BtwRegistry>, ui: Arc<BtwUi>) -> ExtensionContext {
    ExtensionContext {
        ui,
        mode,
        has_ui,
        cwd: "/tmp".into(),
        agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(BtwSession { entries: Vec::new() }),
        model_registry: registry,
        model: Some(model()),
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
        get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions::default()),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

pub fn command_handler() -> CommandHandler {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("btw", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    maho_ext_btw::Btw::default().register(&mut api);
    api.registered.commands.iter().find(|command| command.name == "btw").expect("btw command").handler.clone()
}
