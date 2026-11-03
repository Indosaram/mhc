//! Port of senpi `packages/agent/src/harness/execution/assistant.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ai::model::Model;
use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, BoxFuture,
    Context as AiContext, Message, SimpleStreamOptions, StreamOptions, ThinkingLevel, Tool,
};
use serde_json::{Map, Value};

use crate::harness::context::{Context, provider_telemetry_context};
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
    fn end<'a>(
        &'a self,
        message: SettledAssistantMessage,
        context: &'a Context,
    ) -> BoxFuture<'a, ()>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum AfterResponseError {
    Abort(Cancellation),
    Other(String),
}

pub type TransformContextFn = Arc<
    dyn Fn(TransformContextRequest, Context) -> BoxFuture<'static, TransformContextRequest>
        + Send
        + Sync,
>;
pub type ToProviderMessagesFn =
    Arc<dyn Fn(Vec<AgentMessage>, Context) -> BoxFuture<'static, Vec<Message>> + Send + Sync>;
pub type BeforePayloadFn =
    Arc<dyn Fn(Value, Model, Context) -> BoxFuture<'static, Option<Value>> + Send + Sync>;
pub type AfterResponseFn = Arc<
    dyn Fn(
            SettledAssistantMessage,
            AssistantResponseMetadata,
            Context,
        ) -> BoxFuture<'static, Result<SettledAssistantMessage, AfterResponseError>>
        + Send
        + Sync,
>;
pub type AssistantRequestFn = Arc<
    dyn Fn(
            AiContext,
            SimpleStreamOptions,
            Context,
        ) -> BoxFuture<'static, Result<AssistantMessageEventStream, String>>
        + Send
        + Sync,
>;
pub type AfterResponseCaptureFn = Arc<
    dyn Fn(
            SettledAssistantMessage,
            Context,
        ) -> BoxFuture<'static, Result<SettledAssistantMessage, AfterResponseError>>
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
    stream.request.telemetry_context = provider_telemetry_context(context);
    if let Some(before_payload) = config.before_payload.clone() {
        let context = context.clone();
        stream.request.async_on_payload = Some(Arc::new(move |payload, model, _metadata| {
            let replacement = before_payload(payload, model, context.clone());
            let future: BoxFuture<'static, Result<Option<Value>, String>> =
                Box::pin(async move { Ok(replacement.await) });
            future
        }));
    }
    stream.request.on_response = Some(Arc::new(move |response, _| {
        capture_metadata(AssistantResponseMetadata {
            status: Some(response.status),
            headers: Some(response.headers.clone()),
        });
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
        AssistantMessageEvent::Start { .. }
            | AssistantMessageEvent::Done { .. }
            | AssistantMessageEvent::Error { .. }
    )
}

pub async fn consume_assistant_stream(
    stream: &AssistantMessageEventStream,
    observer: &Arc<dyn AssistantStreamObserver>,
    after_response: Option<AfterResponseCaptureFn>,
    context: &Context,
) -> Result<SettledAssistantMessage, String> {
    let consume = async {
        let mut started = false;
        while let Some(event) = stream.next().await.map_err(|error| error.message)? {
            match &event {
                AssistantMessageEvent::Start { partial } => {
                    if started {
                        return Err(
                            "Assistant message stream emitted more than one start event".to_owned()
                        );
                    }
                    started = true;
                    observer
                        .start(partial.clone(), event.clone(), context)
                        .await;
                }
                AssistantMessageEvent::Done { .. } => {
                    if !started {
                        return Err("Assistant message stream emitted done before start".to_owned());
                    }
                }
                AssistantMessageEvent::Error { .. } => {}
                update => {
                    if !started {
                        return Err(format!(
                            "Assistant message stream emitted {} before start",
                            event_type(update)
                        ));
                    }
                    let partial = partial_of(update);
                    observer.update(partial, update.clone(), context).await;
                }
            }
        }

        let settled = stream.result().await.map_err(|error| error.message)?;
        stream.cancel().await.map_err(|error| error.message)?;
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
    };
    let signal = context.abort_signal();
    let outcome = tokio::select! {
        biased;
        _ = async {
            match signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        } => Err("Assistant stream consumption was aborted".to_owned()),
        outcome = consume => outcome,
    };
    let cancellation = stream.cancel().await.map_err(|error| error.message);
    match outcome {
        Err(error) => Err(error),
        Ok(message) => {
            cancellation?;
            Ok(message)
        }
    }
}

pub async fn stream_harness_assistant(
    messages: Vec<AgentMessage>,
    config: Arc<HarnessAssistantStreamConfig>,
    context: &Context,
) -> Result<SettledAssistantMessage, String> {
    let mut request_context = TransformContextRequest {
        messages,
        system_prompt: config.system_prompt.clone(),
    };
    if let Some(transform_context) = config.transform_context.clone() {
        request_context = transform_context(request_context, context.clone()).await;
    }

    // PERF-DIVERGENCE(senpi agent-session.ts): upstream passes the transformed message array by reference; we move the Vec into to_provider_messages instead of deep-cloning the whole history (only system_prompt is read afterwards). Output is identical.
    let provider_messages =
        (config.to_provider_messages)(request_context.messages, context.clone()).await;
    let ai_context = AiContext {
        system_prompt: Some(request_context.system_prompt.clone()),
        messages: provider_messages,
        tools: config.tools.clone(),
        active_tool_names: None,
    };

    let metadata = Arc::new(std::sync::Mutex::new(AssistantResponseMetadata::default()));
    let capture = metadata.clone();
    let request_options = create_request_options(
        &config,
        Arc::new(move |next| {
            *capture
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = next;
        }),
        context,
    );

    let stream = (config.request)(ai_context, request_options, context.clone()).await?;

    let after_response = config.after_response.clone().map(|after_response| {
        let metadata = metadata.clone();
        let captured: AfterResponseCaptureFn = Arc::new(move |message, after_context| {
            let metadata = metadata
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
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

#[cfg(test)]
mod closure_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    struct Observer(Arc<AtomicBool>);

    impl AssistantStreamObserver for Observer {
        fn start<'a>(
            &'a self,
            _: AssistantMessage,
            _: AssistantMessageEvent,
            _: &'a Context,
        ) -> BoxFuture<'a, ()> {
            Box::pin(async {})
        }
        fn update<'a>(
            &'a self,
            _: AssistantMessage,
            _: AssistantMessageEvent,
            _: &'a Context,
        ) -> BoxFuture<'a, ()> {
            Box::pin(async {})
        }
        fn end<'a>(&'a self, _: SettledAssistantMessage, _: &'a Context) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                assert!(self.0.load(Ordering::SeqCst));
            })
        }
    }

    #[tokio::test]
    async fn normal_and_malformed_exits_acknowledge_cleanup() {
        for malformed in [false, true] {
            let closed = Arc::new(AtomicBool::new(false));
            let closure = closed.clone();
            let stream = AssistantMessageEventStream::assistant();
            stream.set_cancellation_handler(Box::new(move || {
                Box::pin(async move {
                    closure.store(true, Ordering::SeqCst);
                    Ok(())
                })
            }));
            let model = crate::agent::default_model();
            let message =
                crate::assistant_terminal_state::create_terminal_failure_assistant_message(
                    &model,
                    maho_ai::types::StopReason::Stop,
                    &crate::assistant_terminal_state::AgentStreamError::OAuthRefreshUnavailable {
                        message: String::new(),
                    },
                    None,
                );
            if !malformed {
                stream.push(AssistantMessageEvent::Start {
                    partial: message.clone(),
                });
            }
            stream.push(AssistantMessageEvent::Done {
                reason: maho_ai::types::DoneReason::Stop,
                message,
            });
            let observer: Arc<dyn AssistantStreamObserver> = Arc::new(Observer(closed.clone()));
            let outcome = tokio::time::timeout(
                Duration::from_secs(1),
                consume_assistant_stream(
                    &stream,
                    &observer,
                    None,
                    &crate::harness::context::BACKGROUND_CONTEXT,
                ),
            )
            .await
            .expect("consumer exits");
            assert_eq!(outcome.is_err(), malformed);
            assert!(closed.load(Ordering::SeqCst));
        }
    }

    #[tokio::test]
    async fn abandoned_pending_stream_awaits_cleanup() {
        let (context, cancel) =
            crate::harness::context::with_cancel(&crate::harness::context::BACKGROUND_CONTEXT);
        let closed = Arc::new(AtomicBool::new(false));
        let closure = closed.clone();
        let stream = AssistantMessageEventStream::assistant();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        stream.set_cancellation_handler(Box::new(move || {
            Box::pin(async move {
                entered_tx.send(()).expect("cleanup entered");
                release_rx.await.expect("cleanup released");
                closure.store(true, Ordering::SeqCst);
                Ok(())
            })
        }));
        let observer: Arc<dyn AssistantStreamObserver> = Arc::new(Observer(closed.clone()));
        let consumer = tokio::spawn(async move {
            consume_assistant_stream(&stream, &observer, None, &context).await
        });
        cancel.cancel(None);
        tokio::time::timeout(Duration::from_secs(1), entered_rx)
            .await
            .expect("cleanup starts")
            .expect("entered");
        assert!(!consumer.is_finished());
        release_tx.send(()).expect("release");
        assert!(
            tokio::time::timeout(Duration::from_secs(1), consumer)
                .await
                .expect("consumer exits")
                .expect("join")
                .is_err()
        );
        assert!(closed.load(Ordering::SeqCst));
    }
}

/// Port of senpi packages/agent/test/harness/execution-assistant.test.ts.
#[cfg(test)]
mod execution_assistant_tests {
    use super::*;
    use crate::harness::context::{BACKGROUND_CONTEXT, with_abort_signal};
    use maho_ai::types::{
        ContentBlock, DoneReason, ErrorReason, StopReason, UserContent, UserMessage,
    };
    use maho_ai::utils::abort::AbortController;
    use serde_json::json;
    use std::sync::Mutex;

    fn model() -> Model {
        serde_json::from_value(json!({
            "id": "model", "name": "Model", "api": "test", "provider": "provider",
            "baseUrl": "https://example.invalid", "reasoning": true, "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000, "maxTokens": 16384
        }))
        .expect("model")
    }

    fn user(text: &str) -> AgentMessage {
        AgentMessage::Llm(Message::User(UserMessage {
            content: UserContent::Text(text.into()),
            timestamp: 1,
        }))
    }

    fn assistant(text: &str, stop_reason: StopReason) -> AssistantMessage {
        let mut message: AssistantMessage = serde_json::from_value(json!({
            "role": "assistant", "content": [{ "type": "text", "text": text }],
            "api": "test", "provider": "provider", "model": "model",
            "usage": {
                "input": 1, "output": 2, "cacheRead": 3, "cacheWrite": 4, "totalTokens": 10,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
            },
            "stopReason": "stop", "timestamp": 2
        }))
        .expect("assistant");
        message.stop_reason = stop_reason;
        if stop_reason == StopReason::Error {
            message.error_message = Some(text.into());
        }
        message
    }

    fn to_provider_messages() -> ToProviderMessagesFn {
        Arc::new(|messages, _| {
            Box::pin(async move {
                messages
                    .into_iter()
                    .filter_map(|message| match message {
                        AgentMessage::Llm(message) => Some(message),
                        AgentMessage::Custom(_) => None,
                    })
                    .collect()
            })
        })
    }

    fn scripted(events: Vec<AssistantMessageEvent>) -> AssistantRequestFn {
        Arc::new(move |_, _, _| {
            let events = events.clone();
            Box::pin(async move {
                let stream = AssistantMessageEventStream::assistant();
                for event in events {
                    stream.push(event);
                }
                Ok(stream)
            })
        })
    }

    #[derive(Default)]
    struct Recorder {
        order: Arc<Mutex<Vec<String>>>,
        starts: Mutex<Vec<(AssistantMessage, &'static str)>>,
        updates: Mutex<Vec<AssistantMessage>>,
        ends: Mutex<Vec<SettledAssistantMessage>>,
        forbid_start: bool,
    }

    impl AssistantStreamObserver for Recorder {
        fn start<'a>(
            &'a self,
            message: AssistantMessage,
            event: AssistantMessageEvent,
            _: &'a Context,
        ) -> BoxFuture<'a, ()> {
            assert!(
                !self.forbid_start,
                "pre-generation error must not synthesize start"
            );
            self.order
                .lock()
                .expect("order")
                .push("observer_start".into());
            let kind = if matches!(event, AssistantMessageEvent::Start { .. }) {
                "start"
            } else {
                "other"
            };
            self.starts.lock().expect("starts").push((message, kind));
            Box::pin(async {})
        }
        fn update<'a>(
            &'a self,
            message: AssistantMessage,
            _: AssistantMessageEvent,
            _: &'a Context,
        ) -> BoxFuture<'a, ()> {
            self.order
                .lock()
                .expect("order")
                .push("observer_update".into());
            self.updates.lock().expect("updates").push(message);
            Box::pin(async {})
        }
        fn end<'a>(
            &'a self,
            message: SettledAssistantMessage,
            _: &'a Context,
        ) -> BoxFuture<'a, ()> {
            self.order
                .lock()
                .expect("order")
                .push("observer_end".into());
            self.ends.lock().expect("ends").push(message);
            Box::pin(async {})
        }
    }

    fn config(
        request: AssistantRequestFn,
        observer: Arc<Recorder>,
    ) -> HarnessAssistantStreamConfig {
        HarnessAssistantStreamConfig {
            model: model(),
            system_prompt: "system".into(),
            tools: None,
            thinking_level: None,
            stream_options: AgentHarnessStreamOptions::default(),
            transform_context: None,
            to_provider_messages: to_provider_messages(),
            before_payload: None,
            after_response: None,
            request,
            observer,
        }
    }

    #[tokio::test]
    async fn maps_curated_options_and_runs_the_assistant_lifecycle_without_mutating_input() {
        let input = vec![user("original")];
        let controller = AbortController::new();
        let recorder = Arc::new(Recorder::default());
        let order = recorder.order.clone();
        let converted = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::new(Mutex::new(None::<(AiContext, SimpleStreamOptions)>));
        let payload_seen = Arc::new(Mutex::new(None::<Value>));
        let metadata_seen = Arc::new(Mutex::new(None::<AssistantResponseMetadata>));

        let transform_order = order.clone();
        let transform: TransformContextFn = Arc::new(move |mut request, _| {
            transform_order
                .lock()
                .expect("order")
                .push("transform_context".into());
            request.messages.push(user("injected"));
            request.system_prompt = "transformed system".into();
            Box::pin(async move { request })
        });
        let (convert_order, convert_record) = (order.clone(), converted.clone());
        let to_provider: ToProviderMessagesFn = Arc::new(move |messages, context| {
            convert_order
                .lock()
                .expect("order")
                .push("to_provider_messages".into());
            *convert_record.lock().expect("converted") = messages.clone();
            to_provider_messages()(messages, context)
        });
        let (payload_order, payload_record) = (order.clone(), payload_seen.clone());
        let before_payload: BeforePayloadFn = Arc::new(move |payload, seen_model, _| {
            payload_order
                .lock()
                .expect("order")
                .push("before_payload".into());
            assert_eq!(seen_model.id, "resolved");
            *payload_record.lock().expect("payload") = Some(payload);
            Box::pin(async { Some(json!({ "replaced": true })) })
        });
        let (after_order, after_record) = (order.clone(), metadata_seen.clone());
        let after_response: AfterResponseFn = Arc::new(move |mut message, metadata, _| {
            after_order
                .lock()
                .expect("order")
                .push("after_response".into());
            *after_record.lock().expect("metadata") = Some(metadata);
            message.content = vec![ContentBlock::text("transformed")];
            Box::pin(async move { Ok(message) })
        });
        let (request_order, request_record) = (order.clone(), received.clone());
        let request: AssistantRequestFn = Arc::new(move |context, options, _| {
            let (order, record) = (request_order.clone(), request_record.clone());
            Box::pin(async move {
                order.lock().expect("order").push("request".into());
                let mut resolved = model();
                resolved.id = "resolved".into();
                let on_payload = options
                    .stream
                    .request
                    .async_on_payload
                    .clone()
                    .expect("on_payload");
                assert_eq!(
                    on_payload(json!({ "original": true }), resolved, None).await,
                    Ok(Some(json!({ "replaced": true })))
                );
                let on_response = options
                    .stream
                    .request
                    .on_response
                    .clone()
                    .expect("on_response");
                on_response(
                    &maho_ai::types::ProviderResponse {
                        status: 201,
                        headers: [("request-id".to_owned(), "r1".to_owned())]
                            .into_iter()
                            .collect(),
                    },
                    &model(),
                );
                *record.lock().expect("received") = Some((context, options));
                let mut initial = assistant("", StopReason::Pending);
                initial.content.clear();
                let mut partial = initial.clone();
                partial.content = vec![ContentBlock::text("raw")];
                let stream = AssistantMessageEventStream::assistant();
                stream.push(AssistantMessageEvent::Start { partial: initial });
                stream.push(AssistantMessageEvent::TextDelta {
                    content_index: 0,
                    delta: "raw".into(),
                    partial,
                });
                stream.push(AssistantMessageEvent::Done {
                    reason: DoneReason::Stop,
                    message: assistant("raw", StopReason::Stop),
                });
                Ok(stream)
            })
        });
        let mut config = config(request, recorder.clone());
        config.thinking_level = Some(ThinkingLevel::High);
        config.stream_options = serde_json::from_value(json!({
            "transport": "websocket", "timeoutMs": 123, "maxRetries": 2, "maxRetryDelayMs": 456,
            "headers": { "authorization": "test" }, "metadata": { "tenant": "one" },
            "cacheRetention": "long", "deferred": { "window": "1h" }
        }))
        .expect("stream options");
        config.transform_context = Some(transform);
        config.to_provider_messages = to_provider;
        config.before_payload = Some(before_payload);
        config.after_response = Some(after_response);

        let context = with_abort_signal(controller.signal(), &BACKGROUND_CONTEXT);
        let result = stream_harness_assistant(input.clone(), Arc::new(config), &context)
            .await
            .expect("assistant result");

        // Rust moves the input Vec, so "without mutating input" holds by construction; the caller's
        // copy is untouched.
        assert_eq!(input, vec![user("original")]);
        assert_eq!(
            *converted.lock().expect("converted"),
            vec![user("original"), user("injected")]
        );
        let (received_context, options) =
            received.lock().expect("received").take().expect("request");
        assert_eq!(
            received_context.system_prompt.as_deref(),
            Some("transformed system")
        );
        assert_eq!(
            received_context.messages,
            to_provider_messages_sync(vec![user("original"), user("injected")])
        );
        assert_eq!(
            *payload_seen.lock().expect("payload"),
            Some(json!({ "original": true }))
        );
        assert_eq!(
            options.stream.transport,
            Some(maho_ai::types::Transport::Websocket)
        );
        assert_eq!(options.stream.request.timeout_ms, Some(123));
        assert_eq!(options.stream.request.max_retries, Some(2));
        assert_eq!(options.stream.request.max_retry_delay_ms, Some(456));
        assert_eq!(
            options.stream.request.headers,
            Some(
                [("authorization".to_owned(), Some("test".to_owned()))]
                    .into_iter()
                    .collect()
            )
        );
        assert_eq!(
            options.stream.metadata,
            json!({ "tenant": "one" }).as_object().cloned()
        );
        assert_eq!(
            options.stream.cache_retention,
            Some(maho_ai::types::CacheRetention::Long)
        );
        assert_eq!(
            options.deferred,
            Some(maho_ai::types::DeferredOption::Window {
                window: Some(maho_ai::types::DeferredWindow::OneHour)
            })
        );
        assert_eq!(options.reasoning, Some(ThinkingLevel::High));
        assert!(
            options
                .stream
                .request
                .signal
                .expect("signal")
                .same_signal(&controller.signal())
        );
        assert!(options.stream.request.telemetry_context.is_some());
        assert_eq!(
            *metadata_seen.lock().expect("metadata"),
            Some(AssistantResponseMetadata {
                status: Some(201),
                headers: Some(
                    [("request-id".to_owned(), "r1".to_owned())]
                        .into_iter()
                        .collect()
                ),
            })
        );
        let starts = recorder.starts.lock().expect("starts");
        let updates = recorder.updates.lock().expect("updates");
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].1, "start");
        assert_ne!(starts[0].0, updates[0]);
        assert_eq!(updates.len(), 1);
        assert_eq!(recorder.ends.lock().expect("ends")[0], result);
        assert_eq!(result.content, vec![ContentBlock::text("transformed")]);
        assert_eq!(
            *order.lock().expect("order"),
            vec![
                "transform_context",
                "to_provider_messages",
                "request",
                "before_payload",
                "observer_start",
                "observer_update",
                "after_response",
                "observer_end",
            ]
        );
    }

    fn to_provider_messages_sync(messages: Vec<AgentMessage>) -> Vec<Message> {
        messages
            .into_iter()
            .filter_map(|message| match message {
                AgentMessage::Llm(message) => Some(message),
                AgentMessage::Custom(_) => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn runs_against_the_faux_provider_request_boundary() {
        let faux = maho_ai::providers::faux::faux_provider(
            maho_ai::providers::faux::RegisterFauxProviderOptions {
                token_size: Some(maho_ai::providers::faux::FauxTokenSize {
                    min: Some(1),
                    max: Some(1),
                }),
                ..Default::default()
            },
        )
        .expect("faux provider");
        faux.set_responses(vec![
            maho_ai::providers::faux::faux_assistant_message("hello", Default::default()).into(),
        ]);
        let faux_model = faux.get_model(None).expect("faux model");
        let provider = faux.provider.clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let request_model = faux_model.clone();
        let request: AssistantRequestFn = Arc::new(move |context, options, _| {
            *record.lock().expect("seen") = context.messages.clone();
            let stream = provider.stream_simple(&request_model, &context, Some(options));
            Box::pin(async move { Ok(stream) })
        });
        let recorder = Arc::new(Recorder::default());
        let mut config = config(request, recorder.clone());
        config.model = faux_model;

        let result =
            stream_harness_assistant(vec![user("prompt")], Arc::new(config), &BACKGROUND_CONTEXT)
                .await
                .expect("assistant result");

        assert_eq!(
            *seen.lock().expect("seen"),
            to_provider_messages_sync(vec![user("prompt")])
        );
        assert_eq!(result.content, vec![ContentBlock::text("hello")]);
        let order = recorder.order.lock().expect("order");
        assert_eq!(order.first().map(String::as_str), Some("observer_start"));
        assert_eq!(order.last().map(String::as_str), Some("observer_end"));
        assert!(order.iter().any(|event| event == "observer_update"));
    }

    #[tokio::test]
    async fn rejects_a_successful_terminal_event_before_start() {
        let options_seen = Arc::new(Mutex::new(None::<SimpleStreamOptions>));
        let record = options_seen.clone();
        let request: AssistantRequestFn = Arc::new(move |_, options, _| {
            *record.lock().expect("options") = Some(options);
            Box::pin(async {
                let stream = AssistantMessageEventStream::assistant();
                stream.push(AssistantMessageEvent::Done {
                    reason: DoneReason::Stop,
                    message: assistant("complete", StopReason::Stop),
                });
                Ok(stream)
            })
        });
        let error = stream_harness_assistant(
            vec![user("prompt")],
            Arc::new(config(request, Arc::new(Recorder::default()))),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect_err("done before start");
        assert!(error.contains("done before start"));
        assert_eq!(
            options_seen
                .lock()
                .expect("options")
                .as_ref()
                .expect("options")
                .reasoning,
            None
        );
    }

    #[tokio::test]
    async fn keeps_the_raw_settlement_when_cancellation_interrupts_after_response() {
        let final_message = assistant("raw", StopReason::Stop);
        let mut initial = final_message.clone();
        initial.content.clear();
        initial.stop_reason = StopReason::Pending;
        let recorder = Arc::new(Recorder::default());
        let mut config = config(
            scripted(vec![
                AssistantMessageEvent::Start { partial: initial },
                AssistantMessageEvent::Done {
                    reason: DoneReason::Stop,
                    message: final_message.clone(),
                },
            ]),
            recorder.clone(),
        );
        config.after_response = Some(Arc::new(|_, _, _| {
            let cancellation = Cancellation::new();
            cancellation.resolve();
            Box::pin(async move { Err(AfterResponseError::Abort(cancellation)) })
        }));
        let result =
            stream_harness_assistant(vec![user("prompt")], Arc::new(config), &BACKGROUND_CONTEXT)
                .await
                .expect("raw settlement");
        assert_eq!(result, final_message);
        assert_eq!(*recorder.ends.lock().expect("ends"), vec![final_message]);
    }

    #[tokio::test]
    async fn returns_provider_error_settlements_through_the_same_lifecycle() {
        let final_message = assistant("provider failed", StopReason::Error);
        let recorder = Arc::new(Recorder {
            forbid_start: true,
            ..Default::default()
        });
        let config = config(
            scripted(vec![AssistantMessageEvent::Error {
                reason: ErrorReason::Error,
                error: final_message.clone(),
            }]),
            recorder.clone(),
        );
        let result =
            stream_harness_assistant(vec![user("prompt")], Arc::new(config), &BACKGROUND_CONTEXT)
                .await
                .expect("error settlement");
        assert_eq!(result, final_message);
        assert_eq!(*recorder.order.lock().expect("order"), vec!["observer_end"]);
        assert_eq!(
            recorder.ends.lock().expect("ends")[0].stop_reason,
            StopReason::Error
        );
    }
}
