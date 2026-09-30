//! Port of senpi packages/ai/src/providers/faux.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::types::{
    AbortSource, AssistantMessage, AssistantMessageEvent, AssistantStopDetails, Context, ContentBlock,
    DeferredFetchOptions, DeferredHandle, DoneReason, ErrorReason, ImageContent, Message, Model,
    ProviderStreams, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent, ToolCall,
    Usage, UsageCost, UserContent,
};
use crate::utils::diagnostics::now_ms;
use crate::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

const DEFAULT_API: &str = "faux";
const DEFAULT_PROVIDER: &str = "faux";
const DEFAULT_MODEL_ID: &str = "faux-1";
const DEFAULT_MODEL_NAME: &str = "Faux Model";
const DEFAULT_BASE_URL: &str = "http://localhost:0";
const DEFAULT_MIN_TOKEN_SIZE: u64 = 3;
const DEFAULT_MAX_TOKEN_SIZE: u64 = 5;
const DEFAULT_CONTEXT_WINDOW: u64 = 128000;
const DEFAULT_MAX_TOKENS: u64 = 16384;

#[derive(Debug, Clone, Default)]
pub struct FauxModelDefinition {
    pub id: String,
    pub name: Option<String>,
    pub reasoning: Option<bool>,
    pub input: Option<Vec<crate::types::InputModality>>,
    pub cost: Option<crate::types::ModelCost>,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FauxTokenSize {
    pub min: Option<u64>,
    pub max: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FauxDeferredOptions {
    pub pending_fetches: Option<u64>,
    pub poll_after_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct FauxProviderState {
    pub call_count: u64,
    pub deferred_fetch_count: u64,
    pub cancelled_deferred: Vec<DeferredHandle>,
}

#[derive(Debug, Clone)]
pub struct FauxCallLogEntry {
    pub context: Context,
    pub options: Option<StreamOptions>,
    pub timestamp: i64,
    pub model_id: String,
}

pub fn faux_text(text: &str) -> ContentBlock {
    ContentBlock::Text(TextContent { text: text.to_owned(), ..TextContent::default() })
}

pub fn faux_thinking(thinking: &str) -> ContentBlock {
    ContentBlock::Thinking(ThinkingContent { thinking: thinking.to_owned(), ..ThinkingContent::default() })
}

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn random_id(prefix: &str) -> String {
    format!("{prefix}:{}:{}", now_ms(), ID_COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub fn faux_tool_call(name: &str, arguments: Map<String, Value>, id: Option<&str>) -> ContentBlock {
    ContentBlock::ToolCall(ToolCall {
        id: id.map(str::to_owned).unwrap_or_else(|| random_id("tool")),
        name: name.to_owned(),
        arguments,
        ..ToolCall::default()
    })
}

/// \`string | FauxContentBlock | FauxContentBlock[]\` of \`fauxAssistantMessage\`.
pub enum FauxContent {
    Text(String),
    Block(ContentBlock),
    Blocks(Vec<ContentBlock>),
}

impl From<&str> for FauxContent {
    fn from(text: &str) -> Self {
        FauxContent::Text(text.to_owned())
    }
}

impl From<String> for FauxContent {
    fn from(text: String) -> Self {
        FauxContent::Text(text)
    }
}

impl From<ContentBlock> for FauxContent {
    fn from(block: ContentBlock) -> Self {
        FauxContent::Block(block)
    }
}

impl From<Vec<ContentBlock>> for FauxContent {
    fn from(blocks: Vec<ContentBlock>) -> Self {
        FauxContent::Blocks(blocks)
    }
}

#[derive(Debug, Clone, Default)]
pub struct FauxAssistantMessageOptions {
    pub stop_reason: Option<StopReason>,
    pub deferred: Option<DeferredHandle>,
    pub error_message: Option<String>,
    pub abort_source: Option<AbortSource>,
    pub stop_details: Option<AssistantStopDetails>,
    pub response_id: Option<String>,
    pub timestamp: Option<i64>,
}

fn normalize_faux_assistant_content(content: FauxContent) -> Vec<ContentBlock> {
    match content {
        FauxContent::Text(text) => vec![faux_text(&text)],
        FauxContent::Block(block) => vec![block],
        FauxContent::Blocks(blocks) => blocks,
    }
}

pub fn faux_assistant_message(content: impl Into<FauxContent>, options: FauxAssistantMessageOptions) -> AssistantMessage {
    AssistantMessage {
        content: normalize_faux_assistant_content(content.into()),
        api: DEFAULT_API.to_owned(),
        provider: DEFAULT_PROVIDER.to_owned(),
        model: DEFAULT_MODEL_ID.to_owned(),
        response_model: None,
        response_id: options.response_id,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: options.stop_reason.unwrap_or(StopReason::Stop),
        stop_details: options.stop_details,
        deferred: options.deferred,
        error_message: options.error_message,
        abort_source: options.abort_source,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: options.timestamp.unwrap_or_else(now_ms),
    }
}

pub type FauxResponseFactory = Arc<
    dyn Fn(&Context, Option<&SimpleStreamOptions>, &FauxProviderState, &Model) -> crate::types::BoxFuture<'static, AssistantMessage>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub enum FauxResponseStep {
    Message(Box<AssistantMessage>),
    Factory(FauxResponseFactory),
}

impl From<AssistantMessage> for FauxResponseStep {
    fn from(message: AssistantMessage) -> Self {
        FauxResponseStep::Message(Box::new(message))
    }
}

#[derive(Clone, Default)]
pub struct RegisterFauxProviderOptions {
    pub api: Option<String>,
    pub provider: Option<String>,
    pub models: Option<Vec<FauxModelDefinition>>,
    pub deferred: Option<FauxDeferredOptions>,
    pub tokens_per_second: Option<f64>,
    pub scheduler_hook: Option<Arc<dyn Fn() -> crate::types::BoxFuture<'static, ()> + Send + Sync>>,
    pub token_size: Option<FauxTokenSize>,
}

struct DeferredEntry {
    handle: DeferredHandle,
    step: FauxResponseStep,
    context: Context,
    options: Option<SimpleStreamOptions>,
    model: Model,
    pending_fetches: u64,
    cancelled: bool,
    final_message: Option<AssistantMessage>,
}

struct FauxInner {
    pending_responses: VecDeque<FauxResponseStep>,
    state: FauxProviderState,
    prompt_cache: IndexMap<String, String>,
    call_log: Vec<FauxCallLogEntry>,
    deferred_responses: IndexMap<String, DeferredEntry>,
}

pub struct FauxCore {
    api: String,
    provider: String,
    models: Vec<Model>,
    min_token_size: u64,
    max_token_size: u64,
    tokens_per_second: Option<f64>,
    scheduler_hook: Option<Arc<dyn Fn() -> crate::types::BoxFuture<'static, ()> + Send + Sync>>,
    deferred_options: FauxDeferredOptions,
    inner: Mutex<FauxInner>,
}

impl std::fmt::Debug for FauxCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FauxCore").field("api", &self.api).field("provider", &self.provider).finish_non_exhaustive()
    }
}

fn estimate_tokens(text: &str) -> u64 {
    (text.encode_utf16().count() as u64).div_ceil(4)
}

fn clone_context_for_log(context: &Context) -> Context {
    context.clone()
}

fn clone_stream_options_for_log(options: Option<&StreamOptions>) -> Option<StreamOptions> {
    options.cloned()
}

fn content_to_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                ContentBlock::Text(text) => text.text.clone(),
                ContentBlock::Image(ImageContent { mime_type, data }) => format!("[image:{mime_type}:{}]", data.len()),
                other => other.type_name().to_owned(),
            })
            .collect::<Vec<String>>()
            .join("\n"),
    }
}

fn assistant_content_to_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => text.text.clone(),
            ContentBlock::Thinking(thinking) => thinking.thinking.clone(),
            ContentBlock::ProviderNative(_) => String::new(),
            ContentBlock::ToolCall(call) => format!("{}:{}", call.name, Value::Object(call.arguments.clone())),
            ContentBlock::Image(image) => format!("[image:{}:{}]", image.mime_type, image.data.len()),
        })
        .collect::<Vec<String>>()
        .join("\n")
}

fn tool_result_to_text(message: &crate::types::ToolResultMessage) -> String {
    let mut parts = vec![message.tool_name.clone()];
    for block in &message.content {
        parts.push(match block {
            ContentBlock::Text(text) => text.text.clone(),
            ContentBlock::Image(ImageContent { mime_type, data }) => format!("[image:{mime_type}:{}]", data.len()),
            other => other.type_name().to_owned(),
        });
    }
    parts.join("\n")
}

fn message_to_text(message: &Message) -> String {
    match message {
        Message::User(user) => content_to_text(&user.content),
        Message::Assistant(assistant) => assistant_content_to_text(&assistant.content),
        Message::ConfigurationUpdate(_) => String::new(),
        Message::ToolResult(result) => tool_result_to_text(result),
    }
}

fn serialize_context(context: &Context) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(system_prompt) = &context.system_prompt {
        parts.push(format!("system:{system_prompt}"));
    }
    for message in &context.messages {
        parts.push(format!("{}:{}", message.role(), message_to_text(message)));
    }
    let tools = context.tools.as_deref().filter(|tools| !tools.is_empty());
    if let Some(tools) = tools {
        parts.push(format!("tools:{}", Value::Array(tools.iter().map(|tool| serde_json::to_value(tool).unwrap_or(Value::Null)).collect())));
    }
    parts.join("\n\n")
}

fn common_prefix_length(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let length = a.len().min(b.len());
    let mut index = 0;
    while index < length && a[index] == b[index] {
        index += 1;
    }
    index
}

fn with_usage_estimate(
    message: AssistantMessage,
    context: &Context,
    options: Option<&StreamOptions>,
    prompt_cache: &mut IndexMap<String, String>,
) -> AssistantMessage {
    let prompt_text = serialize_context(context);
    let prompt_tokens = estimate_tokens(&prompt_text);
    let output_tokens = estimate_tokens(&assistant_content_to_text(&message.content));
    let mut input = prompt_tokens;
    let mut cache_read = 0;
    let mut cache_write = 0;
    let session_id = options.and_then(|options| options.session_id.clone());

    let caching = options.and_then(|options| options.cache_retention) != Some(crate::types::CacheRetention::None);
    if let Some(session_id) = session_id.filter(|_| caching) {
        {
            match prompt_cache.get(&session_id) {
                Some(previous_prompt) => {
                    let cached_chars = common_prefix_length(previous_prompt, &prompt_text);
                    cache_read = estimate_tokens(&previous_prompt.chars().take(cached_chars).collect::<String>());
                    cache_write = estimate_tokens(&prompt_text.chars().skip(cached_chars).collect::<String>());
                    input = prompt_tokens.saturating_sub(cache_read);
                }
                None => cache_write = prompt_tokens,
            }
            prompt_cache.insert(session_id, prompt_text);
        }
    }

    AssistantMessage {
        usage: Usage {
            input,
            output: output_tokens,
            cache_read,
            cache_write,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: input + output_tokens + cache_read + cache_write,
            cost: UsageCost::default(),
        },
        ..message
    }
}

static CHUNK_SEED: AtomicU64 = AtomicU64::new(0x2545F4914F6CDD1D);

fn next_random() -> f64 {
    let mut x = CHUNK_SEED.load(Ordering::Relaxed);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    CHUNK_SEED.store(x, Ordering::Relaxed);
    (x >> 11) as f64 / (1u64 << 53) as f64
}

fn split_string_by_token_size(text: &str, min_token_size: u64, max_token_size: u64) -> Vec<String> {
    let characters: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        let span = max_token_size.saturating_sub(min_token_size) + 1;
        let token_size = min_token_size + (next_random() * span as f64) as u64;
        let char_size = std::cmp::max(1, token_size * 4) as usize;
        chunks.push(characters[index..std::cmp::min(index + char_size, characters.len())].iter().collect());
        index += char_size;
    }
    if chunks.is_empty() { vec![String::new()] } else { chunks }
}

fn clone_message(message: &AssistantMessage, api: &str, provider: &str, model_id: &str) -> AssistantMessage {
    AssistantMessage {
        api: api.to_owned(),
        provider: provider.to_owned(),
        model: model_id.to_owned(),
        timestamp: if message.timestamp == 0 { now_ms() } else { message.timestamp },
        ..message.clone()
    }
}

fn create_deferred_message(model: &Model, handle: &DeferredHandle) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Deferred,
        stop_details: None,
        deferred: Some(handle.clone()),
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now_ms(),
    }
}

fn create_error_message(error: &str, api: &str, provider: &str, model_id: &str) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: api.to_owned(),
        provider: provider.to_owned(),
        model: model_id.to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        stop_details: None,
        deferred: None,
        error_message: Some(error.to_owned()),
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now_ms(),
    }
}

fn create_aborted_message(partial: &AssistantMessage) -> AssistantMessage {
    AssistantMessage {
        stop_reason: StopReason::Aborted,
        error_message: Some("Request was aborted".to_owned()),
        timestamp: now_ms(),
        ..partial.clone()
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "Faux response factory panicked".to_owned()
}

fn aborted(signal: Option<&crate::utils::abort::AbortSignal>) -> bool {
    signal.is_some_and(|signal| signal.aborted())
}

async fn schedule_chunk(
    chunk: &str,
    tokens_per_second: Option<f64>,
    scheduler_hook: Option<&Arc<dyn Fn() -> crate::types::BoxFuture<'static, ()> + Send + Sync>>,
) {
    if let Some(scheduler_hook) = scheduler_hook {
        scheduler_hook().await;
        return;
    }
    match tokens_per_second {
        Some(rate) if rate > 0.0 => {
            let delay_ms = (estimate_tokens(chunk) as f64 / rate) * 1000.0;
            tokio::time::sleep(Duration::from_secs_f64(delay_ms / 1000.0)).await;
        }
        _ => tokio::task::yield_now().await,
    }
}

struct StreamDeltas<'a> {
    stream: &'a AssistantMessageEventStream,
}

impl StreamDeltas<'_> {
    fn abort_now(&self, partial: &AssistantMessage) -> AssistantMessage {
        let aborted = create_aborted_message(partial);
        self.stream.push(AssistantMessageEvent::Error { reason: ErrorReason::Aborted, error: aborted.clone() });
        self.stream.end(Some(aborted.clone()));
        aborted
    }
}

async fn stream_with_deltas(
    stream: &AssistantMessageEventStream,
    message: AssistantMessage,
    min_token_size: u64,
    max_token_size: u64,
    tokens_per_second: Option<f64>,
    scheduler_hook: Option<&Arc<dyn Fn() -> crate::types::BoxFuture<'static, ()> + Send + Sync>>,
    signal: Option<crate::utils::abort::AbortSignal>,
) -> Result<(), String> {
    let deltas = StreamDeltas { stream };
    let mut partial = AssistantMessage { content: Vec::new(), stop_reason: StopReason::Pending, ..message.clone() };
    if aborted(signal.as_ref()) {
        deltas.abort_now(&partial);
        return Ok(());
    }

    stream.push(AssistantMessageEvent::Start { partial: partial.clone() });

    for (index, block) in message.content.iter().enumerate() {
        if aborted(signal.as_ref()) {
            deltas.abort_now(&partial);
            return Ok(());
        }

        match block {
            ContentBlock::Thinking(thinking) => {
                partial.content.push(ContentBlock::Thinking(ThinkingContent::default()));
                stream.push(AssistantMessageEvent::ThinkingStart { content_index: index, partial: partial.clone() });
                for chunk in split_string_by_token_size(&thinking.thinking, min_token_size, max_token_size) {
                    schedule_chunk(&chunk, tokens_per_second, scheduler_hook).await;
                    if aborted(signal.as_ref()) {
                        deltas.abort_now(&partial);
                        return Ok(());
                    }
                    if let Some(ContentBlock::Thinking(block)) = partial.content.get_mut(index) {
                        block.thinking.push_str(&chunk);
                    }
                    stream.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                stream.push(AssistantMessageEvent::ThinkingEnd {
                    content_index: index,
                    content: thinking.thinking.clone(),
                    partial: partial.clone(),
                });
            }
            ContentBlock::Text(text) => {
                partial.content.push(ContentBlock::Text(TextContent::default()));
                stream.push(AssistantMessageEvent::TextStart { content_index: index, partial: partial.clone() });
                for chunk in split_string_by_token_size(&text.text, min_token_size, max_token_size) {
                    schedule_chunk(&chunk, tokens_per_second, scheduler_hook).await;
                    if aborted(signal.as_ref()) {
                        deltas.abort_now(&partial);
                        return Ok(());
                    }
                    if let Some(ContentBlock::Text(block)) = partial.content.get_mut(index) {
                        block.text.push_str(&chunk);
                    }
                    stream.push(AssistantMessageEvent::TextDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                stream.push(AssistantMessageEvent::TextEnd {
                    content_index: index,
                    content: text.text.clone(),
                    partial: partial.clone(),
                });
            }
            ContentBlock::ProviderNative(_) => {}
            ContentBlock::Image(_) => {}
            ContentBlock::ToolCall(call) => {
                partial.content.push(ContentBlock::ToolCall(ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: Map::new(),
                    ..ToolCall::default()
                }));
                stream.push(AssistantMessageEvent::ToolcallStart { content_index: index, partial: partial.clone() });
                let serialized = Value::Object(call.arguments.clone()).to_string();
                for chunk in split_string_by_token_size(&serialized, min_token_size, max_token_size) {
                    schedule_chunk(&chunk, tokens_per_second, scheduler_hook).await;
                    if aborted(signal.as_ref()) {
                        deltas.abort_now(&partial);
                        return Ok(());
                    }
                    stream.push(AssistantMessageEvent::ToolcallDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                if let Some(ContentBlock::ToolCall(block)) = partial.content.get_mut(index) {
                    block.arguments = call.arguments.clone();
                }
                stream.push(AssistantMessageEvent::ToolcallEnd {
                    content_index: index,
                    tool_call: call.clone(),
                    partial: partial.clone(),
                });
            }
        }
    }

    if message.stop_reason == StopReason::Pending {
        return Err("Faux response ended without a stop reason".to_owned());
    }
    if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
        let reason = if message.stop_reason == StopReason::Aborted { ErrorReason::Aborted } else { ErrorReason::Error };
        stream.push(AssistantMessageEvent::Error { reason, error: message.clone() });
        stream.end(Some(message));
        return Ok(());
    }

    let reason = match message.stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    };
    stream.push(AssistantMessageEvent::Done { reason, message: message.clone() });
    stream.end(Some(message));
    Ok(())
}

impl FauxCore {
    fn lock(&self) -> MutexGuard<'_, FauxInner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn api(&self) -> &str {
        &self.api
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn models(&self) -> &[Model] {
        &self.models
    }

    pub fn state(&self) -> FauxProviderState {
        self.lock().state.clone()
    }

    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        self.lock().pending_responses = responses.into();
    }

    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        self.lock().pending_responses.extend(responses);
    }

    pub fn get_pending_response_count(&self) -> usize {
        self.lock().pending_responses.len()
    }

    pub fn get_call_log(&self) -> Vec<FauxCallLogEntry> {
        self.lock()
            .call_log
            .iter()
            .map(|entry| FauxCallLogEntry {
                context: clone_context_for_log(&entry.context),
                options: clone_stream_options_for_log(entry.options.as_ref()),
                timestamp: entry.timestamp,
                model_id: entry.model_id.clone(),
            })
            .collect()
    }

    pub fn get_model(&self, requested_model_id: Option<&str>) -> Option<Model> {
        match requested_model_id {
            None => self.models.first().cloned(),
            Some(id) => self.models.iter().find(|model| model.id == id).cloned(),
        }
    }

    async fn resolve_response(
        &self,
        step: &FauxResponseStep,
        context: &Context,
        stream_options: Option<&SimpleStreamOptions>,
        request_model: &Model,
        state: &FauxProviderState,
    ) -> Result<AssistantMessage, String> {
        let resolved = match step {
            FauxResponseStep::Message(message) => (**message).clone(),
            FauxResponseStep::Factory(factory) => {
                let pending = std::panic::AssertUnwindSafe(factory(context, stream_options, state, request_model));
                match futures::FutureExt::catch_unwind(pending).await {
                    Ok(message) => message,
                    Err(payload) => return Err(panic_message(&payload)),
                }
            }
        };
        let cloned = clone_message(&resolved, &self.api, &self.provider, &request_model.id);
        let mut inner = self.lock();
        let mut prompt_cache = std::mem::take(&mut inner.prompt_cache);
        let estimated = with_usage_estimate(cloned, context, stream_options.map(|options| &options.stream), &mut prompt_cache);
        inner.prompt_cache = prompt_cache;
        Ok(estimated)
    }

    fn run(self: &Arc<Self>, model: &Model, context: &Context, options: StreamOptions, deferred: Option<crate::types::DeferredOption>) -> AssistantMessageEventStream {
        let outer = create_assistant_message_event_stream();
        let simple = SimpleStreamOptions { stream: options.clone(), deferred, ..SimpleStreamOptions::default() };
        let (step, state) = {
            let mut inner = self.lock();
            let step = inner.pending_responses.pop_front();
            inner.state.call_count += 1;
            inner.call_log.push(FauxCallLogEntry {
                context: clone_context_for_log(context),
                options: clone_stream_options_for_log(Some(&options)),
                timestamp: now_ms(),
                model_id: model.id.clone(),
            });
            (step, inner.state.clone())
        };

        let core = Arc::clone(self);
        let model = model.clone();
        let context = context.clone();
        let outer_for_task = outer.clone();
        tokio::spawn(async move {
            if let Some(on_response) = options.request.on_response.clone() {
                on_response(&crate::types::ProviderResponse { status: 200, headers: Default::default() }, &model);
            }
            let Some(step) = step else {
                let message = create_error_message("No more faux responses queued", &core.api, &core.provider, &model.id);
                let mut inner = core.lock();
                let mut prompt_cache = std::mem::take(&mut inner.prompt_cache);
                let message = with_usage_estimate(message, &context, Some(&options), &mut prompt_cache);
                inner.prompt_cache = prompt_cache;
                outer_for_task.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message.clone() });
                outer_for_task.end(Some(message));
                return;
            };

            if simple.deferred.is_some() {
                let handle = DeferredHandle {
                    provider: model.provider.clone(),
                    model_id: model.id.clone(),
                    api: model.api.clone(),
                    id: random_id("deferred"),
                    expires_at: None,
                    poll_after_ms: core.deferred_options.poll_after_ms,
                    data: None,
                };
                {
                    let mut inner = core.lock();
                    inner.deferred_responses.insert(
                        handle.id.clone(),
                        DeferredEntry {
                            handle: handle.clone(),
                            step,
                            context: context.clone(),
                            options: Some(simple.clone()),
                            model: model.clone(),
                            pending_fetches: core.deferred_options.pending_fetches.unwrap_or(0),
                            cancelled: false,
                            final_message: None,
                        },
                    );
                }
                let message = create_deferred_message(&model, &handle);
                if let Err(error) = stream_with_deltas(
                    &outer_for_task,
                    message,
                    core.min_token_size,
                    core.max_token_size,
                    core.tokens_per_second,
                    core.scheduler_hook.as_ref(),
                    options.request.signal.clone(),
                )
                .await
                {
                    let message = create_error_message(&error, &core.api, &core.provider, &model.id);
                    outer_for_task.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message.clone() });
                    outer_for_task.end(Some(message));
                }
                return;
            }

            let outcome = core.resolve_response(&step, &context, Some(&simple), &model, &state).await;
            let streamed = match outcome {
                Ok(message) => stream_with_deltas(
                    &outer_for_task,
                    message,
                    core.min_token_size,
                    core.max_token_size,
                    core.tokens_per_second,
                    core.scheduler_hook.as_ref(),
                    options.request.signal.clone(),
                )
                .await
                .err(),
                Err(error) => Some(error),
            };
            if let Some(error) = streamed {
                let message = create_error_message(&error, &core.api, &core.provider, &model.id);
                outer_for_task.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message.clone() });
                outer_for_task.end(Some(message));
            }
        });

        outer
    }

    fn run_fetch_deferred(
        self: &Arc<Self>,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> AssistantMessageEventStream {
        let outer = create_assistant_message_event_stream();
        {
            let mut inner = self.lock();
            inner.state.deferred_fetch_count += 1;
        }
        let core = Arc::clone(self);
        let model = model.clone();
        let handle = handle.clone();
        let outer_for_task = outer.clone();
        tokio::spawn(async move {
            let options = options.unwrap_or_default();
            if let Some(on_response) = options.request.on_response.clone() {
                on_response(&crate::types::ProviderResponse { status: 200, headers: Default::default() }, &model);
            }
            let pending: Result<(DeferredHandle, Option<FauxResponseStep>), String> = {
                let mut inner = core.lock();
                match inner.deferred_responses.get_mut(&handle.id) {
                    None => Err(format!("Unknown faux deferred response: {}", handle.id)),
                    Some(entry)
                        if entry.handle.provider != handle.provider
                            || entry.handle.model_id != handle.model_id
                            || entry.handle.api != handle.api =>
                    {
                        Err(format!("Unknown faux deferred response: {}", handle.id))
                    }
                    Some(entry) if entry.cancelled => {
                        Err(format!("Faux deferred response was cancelled: {}", handle.id))
                    }
                    Some(entry) if entry.pending_fetches > 0 => {
                        entry.pending_fetches -= 1;
                        Ok((entry.handle.clone(), None))
                    }
                    Some(entry) => Ok((entry.handle.clone(), Some(entry.step.clone()))),
                }
            };
            match pending {
                Err(error) => {
                    let message = create_error_message(&error, &core.api, &core.provider, &model.id);
                    outer_for_task.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message.clone() });
                    outer_for_task.end(Some(message));
                }
                Ok((entry_handle, None)) => {
                    let message = create_deferred_message(&model, &entry_handle);
                    if let Err(error) = stream_with_deltas(
                        &outer_for_task,
                        message,
                        core.min_token_size,
                        core.max_token_size,
                        core.tokens_per_second,
                        core.scheduler_hook.as_ref(),
                        options.request.signal.clone(),
                    )
                    .await
                    {
                        let message = create_error_message(&error, &core.api, &core.provider, &model.id);
                        outer_for_task.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message.clone() });
                        outer_for_task.end(Some(message));
                    }
                }
                Ok((_, Some(step))) => {
                    let (submission_options, submission_context, submission_model, final_message) = {
                        let mut inner = core.lock();
                        let entry = inner.deferred_responses.get_mut(&handle.id).expect("entry present");
                        let existing = entry.final_message.clone();
                        (entry.options.clone(), entry.context.clone(), entry.model.clone(), existing)
                    };
                    let final_message = match final_message {
                        Some(message) => message,
                        None => {
                            let simple = submission_options.clone();
                            let message = match core
                                .resolve_response(&step, &submission_context, simple.as_ref(), &submission_model, &core.state())
                                .await
                            {
                                Ok(message) => message,
                                Err(error) => create_error_message(&error, &core.api, &core.provider, &submission_model.id),
                            };
                            if let Some(entry) = core.lock().deferred_responses.get_mut(&handle.id) {
                                entry.final_message = Some(message.clone());
                            }
                            message
                        }
                    };
                    if let Err(error) = stream_with_deltas(
                        &outer_for_task,
                        final_message,
                        core.min_token_size,
                        core.max_token_size,
                        core.tokens_per_second,
                        core.scheduler_hook.as_ref(),
                        options.request.signal.clone(),
                    )
                    .await
                    {
                        let message = create_error_message(&error, &core.api, &core.provider, &model.id);
                        outer_for_task.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message.clone() });
                        outer_for_task.end(Some(message));
                    }
                }
            }
        });
        outer
    }

    pub fn cancel_deferred(&self, handle: &DeferredHandle) {
        let mut inner = self.lock();
        inner.state.cancelled_deferred.push(handle.clone());
        if let Some(entry) = inner.deferred_responses.get_mut(&handle.id) {
            entry.cancelled = true;
        }
    }
}

/// A `ProviderStreams` over a faux core: the `api` object `fauxProvider()` hands to `createProvider`.
pub struct FauxStreams(pub Arc<FauxCore>);

pub fn faux_streams(core: Arc<FauxCore>) -> Arc<dyn ProviderStreams> {
    Arc::new(FauxStreams(core))
}

impl ProviderStreams for FauxStreams {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        self.0.run(model, context, options.unwrap_or_default(), None)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let options = options.unwrap_or_default();
        self.0.run(model, context, options.stream.clone(), options.deferred)
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        Some(self.0.run_fetch_deferred(model, handle, options))
    }

    fn supports_deferred(&self) -> bool {
        true
    }
}

pub fn create_faux_core(options: &RegisterFauxProviderOptions) -> Arc<FauxCore> {
    let api = options.api.clone().unwrap_or_else(|| random_id(DEFAULT_API));
    let provider = options.provider.clone().unwrap_or_else(|| DEFAULT_PROVIDER.to_owned());
    let min_token_size =
        std::cmp::max(1, std::cmp::min(options.token_size.and_then(|size| size.min).unwrap_or(DEFAULT_MIN_TOKEN_SIZE), options.token_size.and_then(|size| size.max).unwrap_or(DEFAULT_MAX_TOKEN_SIZE)));
    let max_token_size =
        std::cmp::max(min_token_size, options.token_size.and_then(|size| size.max).unwrap_or(DEFAULT_MAX_TOKEN_SIZE));

    let model_definitions = match &options.models {
        Some(models) if !models.is_empty() => models.clone(),
        _ => vec![FauxModelDefinition {
            id: DEFAULT_MODEL_ID.to_owned(),
            name: Some(DEFAULT_MODEL_NAME.to_owned()),
            reasoning: Some(false),
            input: Some(vec![crate::types::InputModality::Text, crate::types::InputModality::Image]),
            cost: Some(crate::types::ModelCost::default()),
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            max_tokens: Some(DEFAULT_MAX_TOKENS),
        }],
    };
    let models: Vec<Model> = model_definitions
        .into_iter()
        .map(|definition| Model {
            name: definition.name.clone().unwrap_or_else(|| definition.id.clone()),
            id: definition.id,
            api: api.clone(),
            provider: provider.clone(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            reasoning: definition.reasoning.unwrap_or(false),
            thinking_level_map: None,
            input: definition.input.unwrap_or_else(|| vec![crate::types::InputModality::Text, crate::types::InputModality::Image]),
            cost: definition.cost.unwrap_or_default(),
            context_window: definition.context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW),
            max_tokens: definition.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            sampling_params: None,
            headers: None,
            cache_retention: None,
            upstream_model_id: None,
            service_tier: None,
            recover_text_tool_calls: None,
            compat: None,
        })
        .collect();

    Arc::new(FauxCore {
        api,
        provider,
        models,
        min_token_size,
        max_token_size,
        tokens_per_second: options.tokens_per_second,
        scheduler_hook: options.scheduler_hook.clone(),
        deferred_options: options.deferred.unwrap_or_default(),
        inner: Mutex::new(FauxInner {
            pending_responses: VecDeque::new(),
            state: FauxProviderState::default(),
            prompt_cache: IndexMap::new(),
            call_log: Vec::new(),
            deferred_responses: IndexMap::new(),
        }),
    })
}

static REGISTERED_FAUX_PROVIDERS: LazyLock<Mutex<IndexMap<String, Arc<FauxCore>>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));

fn lookup_faux_provider(api: &str) -> Option<crate::api_registry::ApiProviderInternal> {
    let core = REGISTERED_FAUX_PROVIDERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(api)
        .cloned()?;
    Some(crate::api_registry::ApiProviderInternal::new(api, faux_streams(core)))
}

pub fn get_registered_faux_provider(api: &str) -> Option<Arc<FauxCore>> {
    REGISTERED_FAUX_PROVIDERS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(api).cloned()
}

pub fn faux_overflow_error(provider: &str, message: &str) -> AssistantMessage {
    faux_assistant_message(
        Vec::<ContentBlock>::new(),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some(format!("{provider}: {message}")),
            ..FauxAssistantMessageOptions::default()
        },
    )
}

pub struct FauxProviderRegistration {
    pub api: String,
    pub models: Vec<Model>,
    pub core: Arc<FauxCore>,
}

impl FauxProviderRegistration {
    pub fn get_model(&self, model_id: Option<&str>) -> Option<Model> {
        self.core.get_model(model_id)
    }

    pub fn state(&self) -> FauxProviderState {
        self.core.state()
    }

    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        self.core.set_responses(responses);
    }

    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        self.core.append_responses(responses);
    }

    pub fn get_pending_response_count(&self) -> usize {
        self.core.get_pending_response_count()
    }

    pub fn get_call_log(&self) -> Vec<FauxCallLogEntry> {
        self.core.get_call_log()
    }

    pub fn unregister(&self) {
        REGISTERED_FAUX_PROVIDERS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).shift_remove(&self.api);
    }
}

pub fn register_faux_provider(options: RegisterFauxProviderOptions) -> FauxProviderRegistration {
    let core = create_faux_core(&options);
    REGISTERED_FAUX_PROVIDERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(core.api.clone(), core.clone());
    crate::api_registry::install_faux_provider_lookup(Some(lookup_faux_provider));
    FauxProviderRegistration { api: core.api.clone(), models: core.models.clone(), core }
}

pub struct FauxProviderHandle {
    pub provider: Arc<dyn Provider>,
    pub api: String,
    pub models: Vec<Model>,
    pub core: Arc<FauxCore>,
}

impl FauxProviderHandle {
    pub fn get_model(&self, model_id: Option<&str>) -> Option<Model> {
        self.core.get_model(model_id)
    }

    pub fn state(&self) -> FauxProviderState {
        self.core.state()
    }

    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        self.core.set_responses(responses);
    }

    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        self.core.append_responses(responses);
    }

    pub fn get_pending_response_count(&self) -> usize {
        self.core.get_pending_response_count()
    }

    pub fn get_call_log(&self) -> Vec<FauxCallLogEntry> {
        self.core.get_call_log()
    }
}

pub fn faux_provider(options: RegisterFauxProviderOptions) -> FauxProviderHandle {
    let core = create_faux_core(&options);
    let provider = create_provider(CreateProviderOptions {
        id: core.provider.clone(),
        name: None,
        base_url: None,
        headers: None,
        models: core.models.clone(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(faux_streams(core.clone())),
    });
    FauxProviderHandle { provider, api: core.api.clone(), models: core.models.clone(), core }
}
