//! Shared fixtures for the ported senpi `packages/agent/test` suites, mirroring the faux
//! stream-function pattern the TS suites build inline.

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use maho_agent::agent_loop::{AgentEventSink, agent_loop};
use maho_agent::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentStreamOptions, AgentTool, AgentToolResult,
    AgentToolUpdateCallback, StreamFn, identity_convert_to_llm,
};
use maho_ai::model::Model;
use maho_ai::types::{
    Api, AssistantMessage, AssistantMessageEvent, AssistantStopDetails, BoxFuture, ContentBlock, Context, DoneReason,
    ErrorReason, InputModality, Message, ModelCost, ProviderId, StopReason, TextContent, Tool, ToolCall,
    ToolResultMessage, Usage, UserContent, UserMessage,
};
use maho_ai::utils::abort::AbortSignal;
use maho_ai::utils::event_stream::AssistantMessageEventStream;
use serde_json::{Value, json};

pub fn test_model() -> Model {
    Model {
        id: "mock".to_owned(),
        name: "mock".to_owned(),
        api: "openai-responses".to_owned(),
        provider: "openai".to_owned(),
        base_url: "https://example.invalid".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![InputModality::Text],
        cost: ModelCost { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0, tiers: None },
        context_window: 8192,
        max_tokens: 2048,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
    }
}

pub fn assistant(content: Vec<ContentBlock>, stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        content,
        api: "openai-responses".to_owned() as Api,
        provider: "openai".to_owned() as ProviderId,
        model: "mock".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
    }
}

pub fn text_block(text: impl Into<String>) -> ContentBlock {
    ContentBlock::Text(TextContent { text: text.into(), audience: None, text_signature: None })
}

/// An assistant message carrying the extra terminal fields the TS fixtures spread in.
pub fn assistant_with(
    content: Vec<ContentBlock>,
    stop_reason: StopReason,
    error_message: Option<String>,
    stop_details: Option<AssistantStopDetails>,
) -> AssistantMessage {
    AssistantMessage {
        error_message,
        stop_details,
        ..assistant(content, stop_reason)
    }
}

/// A tool that returns `result` verbatim, recording nothing.
pub fn result_tool(name: &str, result: AgentToolResult) -> AgentTool {
    let execute: Arc<
        dyn Fn(String, Value, Option<AbortSignal>, Option<AgentToolUpdateCallback>) -> BoxFuture<'static, AgentToolResult>
            + Send
            + Sync,
    > = Arc::new(move |_id, _args, _signal, _on_update| {
        let result = result.clone();
        Box::pin(async move { result })
    });
    AgentTool {
        label: name.to_owned(),
        prepare_arguments: None,
        execute,
        replay: None,
        execution_mode: None,
        tool: Tool {
            name: name.to_owned(),
            description: name.to_owned(),
            parameters: json!({ "type": "object", "properties": {} }),
            freeform: None,
            constrained_sampling: None,
        },
    }
}

/// A tool whose `execute` returns `result`, counting its invocations.
pub fn counting_result_tool(name: &str, result: AgentToolResult) -> (AgentTool, Arc<std::sync::atomic::AtomicUsize>) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let execute: Arc<
        dyn Fn(String, Value, Option<AbortSignal>, Option<AgentToolUpdateCallback>) -> BoxFuture<'static, AgentToolResult>
            + Send
            + Sync,
    > = Arc::new(move |_id, _args, _signal, _on_update| {
        counter.fetch_add(1, Ordering::SeqCst);
        let result = result.clone();
        Box::pin(async move { result })
    });
    let tool = AgentTool {
        label: name.to_owned(),
        prepare_arguments: None,
        execute,
        replay: None,
        execution_mode: None,
        tool: Tool {
            name: name.to_owned(),
            description: name.to_owned(),
            parameters: json!({ "type": "object", "properties": {} }),
            freeform: None,
            constrained_sampling: None,
        },
    };
    (tool, calls)
}

pub fn tool_call(id: &str, name: &str, arguments: Value) -> ContentBlock {
    ContentBlock::ToolCall(ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.as_object().cloned().unwrap_or_default(),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    })
}

pub fn done_reason(stop_reason: StopReason) -> DoneReason {
    match stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    }
}

/// A stream that resolves to `message` as soon as the loop reads from it. The TS suites push a
/// `done` event in a microtask; here it is queued before the stream is handed over.
pub fn message_stream(message: AssistantMessage) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::assistant();
    stream.push(AssistantMessageEvent::Done { reason: done_reason(message.stop_reason), message });
    stream
}

pub fn error_stream(message: AssistantMessage) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::assistant();
    let reason = match message.stop_reason {
        StopReason::Aborted => ErrorReason::Aborted,
        _ => ErrorReason::Error,
    };
    stream.push(AssistantMessageEvent::Error { reason, error: message });
    stream
}

/// Hands out `messages` in order; once exhausted it repeats the last one.
pub fn scripted_stream_fn(messages: Vec<AssistantMessage>) -> StreamFn {
    let queue = Arc::new(Mutex::new(messages));
    Arc::new(move |_model: &Model, _context: &Context, _options: Option<AgentStreamOptions>| {
        let mut queue = queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let message = if queue.len() > 1 {
            queue.remove(0)
        } else {
            queue.first().cloned().unwrap_or_else(|| assistant(vec![text_block("")], StopReason::Stop))
        };
        message_stream(message)
    })
}

/// Records every call's `(model, context, options)` and returns the next scripted message.
pub struct RecordingStreamFn {
    calls: Arc<Mutex<Vec<(Model, Context, Option<AgentStreamOptions>)>>>,
    stream: StreamFn,
}

impl RecordingStreamFn {
    pub fn new(messages: Vec<AssistantMessage>) -> Self {
        Self { calls: Arc::new(Mutex::new(Vec::new())), stream: scripted_stream_fn(messages) }
    }

    pub fn stream_fn(&self) -> StreamFn {
        let calls = Arc::clone(&self.calls);
        let inner = Arc::clone(&self.stream);
        Arc::new(move |model: &Model, context: &Context, options: Option<AgentStreamOptions>| {
            calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((model.clone(), context.clone(), options.clone()));
            inner(model, context, options)
        })
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len()
    }

    pub fn calls(&self) -> Vec<(Model, Context, Option<AgentStreamOptions>)> {
        self.calls.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }
}

pub fn user_message(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        content: UserContent::Text(text.to_owned()),
        timestamp: 0,
    }))
}

pub fn assistant_message(message: AssistantMessage) -> AgentMessage {
    AgentMessage::Llm(Message::Assistant(Box::new(message)))
}

pub fn tool_result_message(result: &ToolResultMessage) -> AgentMessage {
    AgentMessage::Llm(Message::ToolResult(result.clone()))
}

pub fn recording_tool(name: &str) -> (AgentTool, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let tool_name = name.to_owned();
    let execute: Arc<
        dyn Fn(String, Value, Option<AbortSignal>, Option<AgentToolUpdateCallback>) -> BoxFuture<'static, AgentToolResult>
            + Send
            + Sync,
    > = Arc::new(move |_id, args, _signal, _on_update| {
        sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(args.clone());
        let text = format!("{tool_name}:{}", args.get("city").and_then(Value::as_str).unwrap_or(""));
        Box::pin(async move { AgentToolResult::text(text) })
    });
    let tool = AgentTool {
        label: name.to_owned(),
        prepare_arguments: None,
        execute,
        replay: None,
        execution_mode: None,
        tool: Tool {
            name: name.to_owned(),
            description: "Weather".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"],
            }),
            freeform: None,
            constrained_sampling: None,
        },
    };
    (tool, seen)
}

pub fn text_of(result: &ToolResultMessage) -> String {
    result
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => text.text.clone(),
            other => format!("[{}]", other.type_name()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn collect_events(stream: &maho_ai::utils::event_stream::EventStream<AgentEvent, Vec<AgentMessage>>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await.expect("stream ok") {
        events.push(event);
    }
    events
}

pub fn event_names(events: &[AgentEvent]) -> Vec<&'static str> {
    events.iter().map(event_name).collect()
}

pub fn event_name(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::AgentStart => "agent_start",
        AgentEvent::AgentEnd { .. } => "agent_end",
        AgentEvent::TurnStart => "turn_start",
        AgentEvent::TurnEnd { .. } => "turn_end",
        AgentEvent::MessageStart { .. } => "message_start",
        AgentEvent::MessageUpdate { .. } => "message_update",
        AgentEvent::MessageEnd { .. } => "message_end",
        AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
        AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
    }
}

pub fn recording_sink() -> (AgentEventSink, Arc<Mutex<Vec<AgentEvent>>>) {
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let emit: AgentEventSink = Arc::new(move |event: AgentEvent| {
        sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(event);
        Box::pin(async {})
    });
    (emit, events)
}

pub async fn run_agent_loop(
    prompt: &str,
    tools: Vec<AgentTool>,
    stream_fn: StreamFn,
    configure: impl FnOnce(&mut AgentLoopConfig),
) -> Vec<AgentEvent> {
    let context = AgentContext { system_prompt: String::new(), messages: Vec::new(), tools: Some(tools) };
    let mut config = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    configure(&mut config);
    let stream = agent_loop(vec![user_message(prompt)], context, config, None, Some(stream_fn));
    collect_events(&stream).await
}

pub struct EventCounter {
    count: AtomicUsize,
}

impl EventCounter {
    pub fn new() -> Self {
        Self { count: AtomicUsize::new(0) }
    }

    pub fn bump(&self) -> usize {
        self.count.fetch_add(1, Ordering::SeqCst)
    }

    pub fn get(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl Default for EventCounter {
    fn default() -> Self {
        Self::new()
    }
}
