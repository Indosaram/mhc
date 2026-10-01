//! Port of senpi `packages/agent/src/harness/execution/assistant.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ai::model::Model;
use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, BoxFuture, Context as AiContext, Message,
    SimpleStreamOptions, StreamOptions, ThinkingLevel, Tool,
};
use serde_json::{Map, Value};

use crate::harness::context::{Context, get_telemetry_context};
use crate::harness::execution::effect_gate::{AbortRequested, Cancellation};
use crate::harness::session::types::SettledAssistantMessage;
use crate::harness::types::AgentHarnessStreamOptions;
use crate::types::AgentMessage;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssistantResponseMetadata {
    pub status: Option<u16>,
    pub headers: Option<BTreeMap<String, String>>,
}

pub trait AssistantStreamObserver: Send + Sync {
    fn start<'a>(
        &'a self,
        message: AssistantMessage,
        event: AssistantMessageEvent,
        context: &'a Context,
    ) -> BoxFuture<'a, ()>;
    fn update<'a>(
        &'a self,
        message: AssistantMessage,
        event: AssistantMessageEvent,
        context: &'a Context,
    ) -> BoxFuture<'a, ()>;
    fn end<'a>(&'a self, message: SettledAssistantMessage, context: &'a Context) -> BoxFuture<'a, ()>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum AfterResponseError {
    Abort(Cancellation),
    Other(String),
}

pub type TransformContextFn =
    Arc<dyn Fn(TransformContextRequest, Context) -> BoxFuture<'static, TransformContextRequest> + Send + Sync>;
pub type ToProviderMessagesFn =
    Arc<dyn Fn(Vec<AgentMessage>, Context) -> BoxFuture<'static, Vec<Message>> + Send + Sync>;
pub type BeforePayloadFn = Arc<dyn Fn(Value, Model, Context) -> BoxFuture<'static, Option<Value>> + Send + Sync>;
pub type AfterResponseFn = Arc<
    dyn Fn(SettledAssistantMessage, AssistantResponseMetadata, Context) -> BoxFuture<'static, Result<SettledAssistantMessage, AfterResponseError>>
        + Send
        + Sync,
>;
pub type AssistantRequestFn = Arc<
    dyn Fn(AiContext, SimpleStreamOptions, Context) -> BoxFuture<'static, Result<AssistantMessageEventStream, String>>
        + Send
        + Sync,
>;
pub type AfterResponseCaptureFn = Arc<
    dyn Fn(SettledAssistantMessage, Context) -> BoxFuture<'static, Result<SettledAssistantMessage, AfterResponseError>>
        + Send
        + Sync,
>;

pub struct HarnessAssistantStreamConfig {
    pub model: Model,
    pub system_prompt: String,
    pub tools: Option<Vec<Tool>>,
    pub thinking_level: Option<ThinkingLevel>,
    pub stream_options: AgentHarnessStreamOptions,
    pub transform_context: Option<TransformContextFn>,
    pub to_provider_messages: ToProviderMessagesFn,
    pub before_payload: Option<BeforePayloadFn>,
    pub after_response: Option<AfterResponseFn>,
    pub request: AssistantRequestFn,
    pub observer: Arc<dyn AssistantStreamObserver>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransformContextRequest {
    pub messages: Vec<AgentMessage>,
    pub system_prompt: String,
}

pub fn create_request_options(
    config: &HarnessAssistantStreamConfig,
    capture_metadata: Arc<dyn Fn(AssistantResponseMetadata) + Send + Sync>,
    context: &Context,
) -> SimpleStreamOptions {
    let options = &config.stream_options;
    let mut stream = StreamOptions {
        transport: options.transport,
        cache_retention: options.cache_retention,
        metadata: options.metadata.clone(),
        ..StreamOptions::default()
    };
    stream.request.timeout_ms = options.timeout_ms;
    stream.request.max_retries = options.max_retries;
    stream.request.max_retry_delay_ms = options.max_retry_delay_ms;
    stream.request.headers = options.headers.as_ref().map(|headers| {
        headers
            .iter()
            .map(|(key, value)| (key.clone(), value.as_str().map(ToOwned::to_owned)))
            .collect()
    });
    stream.request.signal = context.abort_signal();
    let _ = get_telemetry_context(context);
    if let Some(before_payload) = config.before_payload.clone() {
        let model = config.model.clone();
        let context = context.clone();
        stream.request.on_payload = Some(Arc::new(move |_payload, _, _| {
            let _ = (&before_payload, &model, &context);
            None
        }));
    }
    stream.request.on_response = Some(Arc::new(move |response, _| {
        capture_metadata(AssistantResponseMetadata { status: Some(response.status), headers: Some(response.headers.clone()) });
    }));
    SimpleStreamOptions {
        stream,
        reasoning: config.thinking_level,
        deferred: options.deferred,
        ..SimpleStreamOptions::default()
    }
}

pub fn is_update_event(event: &AssistantMessageEvent) -> bool {
    !matches!(
        event,
        AssistantMessageEvent::Start { .. } | AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
    )
}

pub async fn consume_assistant_stream(
    stream: &AssistantMessageEventStream,
    observer: &Arc<dyn AssistantStreamObserver>,
    after_response: Option<AfterResponseCaptureFn>,
    context: &Context,
) -> Result<SettledAssistantMessage, String> {
    let mut started = false;
    while let Some(event) = stream.next().await.map_err(|error| error.message)? {
        match &event {
            AssistantMessageEvent::Start { partial } => {
                if started {
                    return Err("Assistant message stream emitted more than one start event".to_owned());
                }
                started = true;
                observer.start(partial.clone(), event.clone(), context).await;
            }
            AssistantMessageEvent::Done { .. } => {
                if !started {
                    return Err("Assistant message stream emitted done before start".to_owned());
                }
            }
            AssistantMessageEvent::Error { .. } => {}
            update => {
                if !started {
                    return Err(format!("Assistant message stream emitted {} before start", event_type(update)));
                }
                let partial = partial_of(update);
                observer.update(partial, update.clone(), context).await;
            }
        }
    }

    let settled = stream.result().await.map_err(|error| error.message)?;
    let mut final_message = settle(settled)?;
    if let Some(after_response) = after_response {
        match after_response(final_message.clone(), context.clone()).await {
            Ok(message) => final_message = message,
            Err(AfterResponseError::Abort(abort)) => {
                abort.wait().await;
            }
            Err(AfterResponseError::Other(error)) => return Err(error),
        }
    }
    observer.end(final_message.clone(), context).await;
    Ok(final_message)
}

pub async fn stream_harness_assistant(
    messages: Vec<AgentMessage>,
    config: Arc<HarnessAssistantStreamConfig>,
    context: &Context,
) -> Result<SettledAssistantMessage, String> {
    let mut request_context = TransformContextRequest { messages, system_prompt: config.system_prompt.clone() };
    if let Some(transform_context) = config.transform_context.clone() {
        request_context = transform_context(request_context, context.clone()).await;
    }

    let provider_messages = (config.to_provider_messages)(request_context.messages.clone(), context.clone()).await;
    let ai_context = AiContext {
        system_prompt: Some(request_context.system_prompt.clone()),
        messages: provider_messages,
        tools: config.tools.clone(),
    };

    let metadata = Arc::new(std::sync::Mutex::new(AssistantResponseMetadata::default()));
    let capture = metadata.clone();
    let request_options = create_request_options(
        &config,
        Arc::new(move |next| {
            *capture.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = next;
        }),
        context,
    );

    let stream = (config.request)(ai_context, request_options, context.clone()).await?;

    let after_response = config.after_response.clone().map(|after_response| {
        let metadata = metadata.clone();
        let captured: AfterResponseCaptureFn = Arc::new(move |message, after_context| {
            let metadata = metadata.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            after_response(message, metadata, after_context)
        });
        captured
    });

    consume_assistant_stream(&stream, &config.observer, after_response, context).await
}

fn settle(message: AssistantMessage) -> Result<SettledAssistantMessage, String> {
    if matches!(message.stop_reason, maho_ai::types::StopReason::Pending) {
        return Err("Assistant message settled with a pending stop reason".to_owned());
    }
    Ok(message)
}

fn event_type(event: &AssistantMessageEvent) -> &'static str {
    match event {
        AssistantMessageEvent::Start { .. } => "start",
        AssistantMessageEvent::TextStart { .. } => "text_start",
        AssistantMessageEvent::TextDelta { .. } => "text_delta",
        AssistantMessageEvent::TextEnd { .. } => "text_end",
        AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
        AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
        AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
        AssistantMessageEvent::ToolcallStart { .. } => "toolcall_start",
        AssistantMessageEvent::ToolcallDelta { .. } => "toolcall_delta",
        AssistantMessageEvent::ToolcallEnd { .. } => "toolcall_end",
        AssistantMessageEvent::Done { .. } => "done",
        AssistantMessageEvent::Error { .. } => "error",
    }
}

fn partial_of(event: &AssistantMessageEvent) -> AssistantMessage {
    match event {
        AssistantMessageEvent::Start { partial }
        | AssistantMessageEvent::TextStart { partial, .. }
        | AssistantMessageEvent::TextDelta { partial, .. }
        | AssistantMessageEvent::TextEnd { partial, .. }
        | AssistantMessageEvent::ThinkingStart { partial, .. }
        | AssistantMessageEvent::ThinkingDelta { partial, .. }
        | AssistantMessageEvent::ThinkingEnd { partial, .. }
        | AssistantMessageEvent::ToolcallStart { partial, .. }
        | AssistantMessageEvent::ToolcallDelta { partial, .. }
        | AssistantMessageEvent::ToolcallEnd { partial, .. } => partial.clone(),
        AssistantMessageEvent::Done { message, .. } => message.clone(),
        AssistantMessageEvent::Error { error, .. } => error.clone(),
    }
}

pub fn abort_requested(cancellation: Cancellation) -> AbortRequested {
    AbortRequested::new(cancellation)
}

pub fn empty_headers() -> Option<Map<String, Value>> {
    None
}
