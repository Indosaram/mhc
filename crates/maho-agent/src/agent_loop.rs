//! Port of senpi packages/agent/src/agent-loop.ts.
//!
//! Agent loop that works with AgentMessage throughout. Transforms to Message[] only at the LLM
//! call boundary.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, ContentBlock, Context, ErrorReason, Message,
    StopReason, StreamKind, Tool, ToolCall, ToolResultMessage, Usage,
};
use maho_ai::utils::abort::{AbortController, AbortReason, AbortSignal};
use maho_ai::utils::event_stream::{EventStream, StreamError};
use maho_ai::utils::validation::validate_tool_arguments;
use maho_ai::api::cursor_agent::types::CursorToolResultHandler;
use tokio::sync::oneshot;

use crate::assistant_terminal_state::{
    AgentStreamError, TerminalAssistantMessageEvent, create_terminal_failure_assistant_message,
    demote_tool_use_without_tool_calls, is_stream_idle_timeout_error, normalize_terminal_assistant_message, now_ms,
    promote_stop_with_pending_tool_calls, should_finalize_idle_as_stop, should_terminate_assistant_turn,
};
use crate::empty_assistant_recovery::with_empty_assistant_recovery;
use crate::stream_fn::get_default_stream_fn;
use crate::tool_arguments::prepare_agent_tool_call_arguments;
use crate::tool_name_alias::{resolve_call_tool, with_tool_name_correction};
use crate::types::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentStreamOptions, AgentTool, AgentToolCall,
    AgentToolResult, AgentToolUpdateCallback, CursorExecHandlersConfig, PendingQueue, StreamFn, ToolExecutionMode,
    model_thinking_level_to_reasoning,
};

/// Event sink the loop writes into. TS uses `(event) => Promise<void> | void`.
pub type AgentEventSink = Arc<dyn Fn(AgentEvent) -> maho_ai::types::BoxFuture<'static, ()> + Send + Sync>;

async fn send(emit: &AgentEventSink, event: AgentEvent) {
    emit(event).await;
}

fn assistant_event(message: &AssistantMessage) -> AgentMessage {
    AgentMessage::Llm(Message::Assistant(Box::new(message.clone())))
}

/// Start an agent loop with a new prompt message.
/// The prompt is added to the context and events are emitted for it.
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> EventStream<AgentEvent, Vec<AgentMessage>> {
    let stream = create_agent_stream();
    let sink = stream.clone();
    let emit: AgentEventSink = Arc::new(move |event| {
        sink.push(event);
        Box::pin(async {})
    });
    let end_stream = stream.clone();
    tokio::spawn(async move {
        let messages = run_agent_loop(prompts, context, config, emit, signal, stream_fn).await;
        end_stream.end(Some(messages));
    });
    stream
}

/// Continue an agent loop from the current context without adding a new message.
/// Used for retries - context already has user message or tool results.
///
/// **Important:** The last message in context must convert to a `user` or `toolResult` message
/// via `convertToLlm`. If it doesn't, the LLM provider will reject the request.
/// This cannot be validated here since `convertToLlm` is only called once per turn.
pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> EventStream<AgentEvent, Vec<AgentMessage>> {
    if context.messages.is_empty() {
        panic!("Cannot continue: no messages in context");
    }
    if context.messages[context.messages.len() - 1].role() == "assistant" {
        panic!("Cannot continue from message role: assistant");
    }

    let stream = create_agent_stream();
    let sink = stream.clone();
    let emit: AgentEventSink = Arc::new(move |event| {
        sink.push(event);
        Box::pin(async {})
    });
    let end_stream = stream.clone();
    tokio::spawn(async move {
        let messages = run_agent_loop_continue(context, config, emit, signal, stream_fn).await;
        end_stream.end(Some(messages));
    });
    stream
}

pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Vec<AgentMessage> {
    let mut new_messages: Vec<AgentMessage> = prompts.clone();
    let mut current_context = context.clone();
    current_context.messages.extend(prompts.iter().cloned());

    send(&emit, AgentEvent::AgentStart).await;
    send(&emit, AgentEvent::TurnStart).await;
    for prompt in &prompts {
        send(&emit, AgentEvent::MessageStart { message: prompt.clone() }).await;
        send(&emit, AgentEvent::MessageEnd { message: prompt.clone() }).await;
    }

    let stream_function = stream_fn.unwrap_or_else(get_default_stream_fn);
    run_loop(current_context, &mut new_messages, config, signal, emit, stream_function).await;
    new_messages
}

pub async fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Vec<AgentMessage> {
    if context.messages.is_empty() {
        panic!("Cannot continue: no messages in context");
    }
    if context.messages[context.messages.len() - 1].role() == "assistant" {
        panic!("Cannot continue from message role: assistant");
    }

    let mut new_messages: Vec<AgentMessage> = Vec::new();
    let current_context = context.clone();

    send(&emit, AgentEvent::AgentStart).await;
    send(&emit, AgentEvent::TurnStart).await;

    let stream_function = stream_fn.unwrap_or_else(get_default_stream_fn);
    run_loop(current_context, &mut new_messages, config, signal, emit, stream_function).await;
    new_messages
}

fn create_agent_stream() -> EventStream<AgentEvent, Vec<AgentMessage>> {
    EventStream::new(
        |event: &AgentEvent| matches!(event, AgentEvent::AgentEnd { .. }),
        |event: &AgentEvent| match event {
            AgentEvent::AgentEnd { messages } => messages.clone(),
            _ => Vec::new(),
        },
    )
}

async fn get_messages(callback: &Option<crate::types::GetMessages>) -> Vec<AgentMessage> {
    match callback {
        Some(callback) => callback().await,
        None => Vec::new(),
    }
}

/// `refreshTerminatingQueueDrain`.
async fn refresh_terminating_queue_drain(
    drained_terminating_queue: &mut Option<PendingQueue>,
    pending_messages: &mut Vec<AgentMessage>,
    config: &AgentLoopConfig,
) {
    let Some(queue) = *drained_terminating_queue else { return };
    let Some(restore) = config.restore_pending_messages.as_ref() else { return };
    restore(queue, pending_messages.clone()).await;
    *pending_messages = get_messages(&config.get_steering_messages).await;
    *drained_terminating_queue = if pending_messages.is_empty() { None } else { Some(PendingQueue::Steering) };
    if pending_messages.is_empty() {
        *pending_messages = get_messages(&config.get_follow_up_messages).await;
        *drained_terminating_queue = if pending_messages.is_empty() { None } else { Some(PendingQueue::FollowUp) };
    }
}

/// Main loop logic shared by agentLoop and agentLoopContinue.
///
/// TS wraps `prepareNextTurn` in a try/catch that restores a drained terminating queue before
/// rethrowing. Callbacks in this port are infallible (a failing host callback unwinds the
/// process instead of returning a value), so there is no Rust equivalent of that catch.
async fn run_loop(
    initial_context: AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    initial_config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: AgentEventSink,
    stream_function: StreamFn,
) {
    let mut current_context = initial_context;
    let mut config = initial_config;
    let mut first_turn = true;
    let mut first_provider_request = true;
    let mut drained_terminating_queue: Option<PendingQueue> = None;
    let mut turn_start_already_emitted = false;

    // Check for steering messages at start (user may have typed while waiting)
    let mut pending_messages: Vec<AgentMessage> = get_messages(&config.get_steering_messages).await;

    // Outer loop: continues when queued follow-up messages arrive after agent would stop
    loop {
        let mut has_more_tool_calls = true;

        // Inner loop: process tool calls and steering messages
        while has_more_tool_calls || !pending_messages.is_empty() {
            if turn_start_already_emitted {
                turn_start_already_emitted = false;
            } else if !first_turn {
                send(&emit, AgentEvent::TurnStart).await;
            } else {
                first_turn = false;
            }
            if drained_terminating_queue.is_some() {
                refresh_terminating_queue_drain(&mut drained_terminating_queue, &mut pending_messages, &config).await;
                if pending_messages.is_empty() {
                    send(&emit, AgentEvent::AgentEnd { messages: new_messages.clone() }).await;
                    return;
                }
                drained_terminating_queue = None;
            }

            // Process pending messages (inject before next assistant response)
            if !pending_messages.is_empty() {
                for message in std::mem::take(&mut pending_messages) {
                    send(&emit, AgentEvent::MessageStart { message: message.clone() }).await;
                    send(&emit, AgentEvent::MessageEnd { message: message.clone() }).await;
                    current_context.messages.push(message.clone());
                    new_messages.push(message);
                }
            }

            // Stream assistant response
            let is_initial_provider_request = first_provider_request;
            first_provider_request = false;
            let request_config = if is_initial_provider_request {
                let mut request_config = config.clone();
                request_config.options.stream.request.timeout_ms =
                    config.initial_request_timeout_ms.or(config.timeout_ms());
                request_config.stream_start_timeout_ms = config
                    .initial_request_stream_start_timeout_ms
                    .or(config.stream_start_timeout_ms);
                request_config
            } else {
                config.clone()
            };
            let idle_timeout_ms = if is_initial_provider_request {
                config.timeout_ms()
            } else {
                request_config.timeout_ms()
            };
            let streamed = stream_assistant_response(
                &mut current_context,
                &request_config,
                signal.clone(),
                &emit,
                with_empty_assistant_recovery(&request_config.model, stream_function.clone()),
                idle_timeout_ms,
            )
            .await;
            let message = demote_tool_use_without_tool_calls(promote_stop_with_pending_tool_calls(streamed.message));
            let provider_tool_results = streamed.provider_tool_results;
            new_messages.push(assistant_event(&message));
            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            for result in &provider_tool_results {
                let message = AgentMessage::Llm(Message::ToolResult(result.clone()));
                send(&emit, AgentEvent::MessageStart { message: message.clone() }).await;
                send(&emit, AgentEvent::MessageEnd { message: message.clone() }).await;
                current_context.messages.push(message);
                new_messages.push(AgentMessage::Llm(Message::ToolResult(result.clone())));
                tool_results.push(result.clone());
            }

            if should_terminate_assistant_turn(&message) {
                send(
                    &emit,
                    AgentEvent::TurnEnd {
                        message: assistant_event(&message),
                        tool_results: tool_results.clone(),
                    },
                )
                .await;
                send(&emit, AgentEvent::AgentEnd { messages: new_messages.clone() }).await;
                return;
            }

            // Check for tool calls
            let tool_calls: Vec<AgentToolCall> = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolCall(call)
                        if !is_cursor_exec_resolved(call, &provider_tool_results) =>
                    {
                        Some(call.clone())
                    }
                    _ => None,
                })
                .collect();

            has_more_tool_calls = false;
            let mut tool_batch_terminated = false;
            if !tool_calls.is_empty() {
                // A "length" stop means the output was cut off by the token limit, so
                // every tool call in the message may carry truncated arguments. Fail
                // them all instead of executing potentially borked calls.
                let executed_tool_batch = if message.stop_reason == StopReason::Length {
                    fail_tool_calls_from_truncated_message(&tool_calls, &emit).await
                } else {
                    execute_tool_calls(&current_context, &message, &config, signal.clone(), &emit).await
                };
                tool_results.extend(executed_tool_batch.messages.iter().cloned());
                tool_batch_terminated = executed_tool_batch.terminate;
                has_more_tool_calls = !executed_tool_batch.terminate;

                for result in &executed_tool_batch.messages {
                    current_context.messages.push(AgentMessage::Llm(Message::ToolResult(result.clone())));
                    new_messages.push(AgentMessage::Llm(Message::ToolResult(result.clone())));
                }
            }

            send(
                &emit,
                AgentEvent::TurnEnd {
                    message: assistant_event(&message),
                    tool_results: tool_results.clone(),
                },
            )
            .await;
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                send(&emit, AgentEvent::AgentEnd { messages: new_messages.clone() }).await;
                return;
            }

            let next_turn_context = crate::types::ShouldStopAfterTurnContext {
                message: message.clone(),
                tool_results: tool_results.clone(),
                context: current_context.clone(),
                new_messages: new_messages.clone(),
            };
            if let Some(should_stop) = config.should_stop_after_turn.clone()
                && should_stop(next_turn_context.clone()).await
            {
                send(&emit, AgentEvent::AgentEnd { messages: new_messages.clone() }).await;
                return;
            }
            if tool_batch_terminated {
                pending_messages = get_messages(&config.get_steering_messages).await;
                if !pending_messages.is_empty() {
                    drained_terminating_queue = Some(PendingQueue::Steering);
                }
                if pending_messages.is_empty() {
                    pending_messages = get_messages(&config.get_follow_up_messages).await;
                    if !pending_messages.is_empty() {
                        drained_terminating_queue = Some(PendingQueue::FollowUp);
                    }
                }
                if pending_messages.is_empty() {
                    send(&emit, AgentEvent::AgentEnd { messages: new_messages.clone() }).await;
                    return;
                }
                // Give queue owners a boundary before preparation refreshes the drained
                // snapshot, so a clear or replacement wins before admission.
                send(&emit, AgentEvent::TurnStart).await;
                turn_start_already_emitted = true;
            }

            let next_turn_snapshot = match config.prepare_next_turn.clone() {
                Some(prepare) => prepare(next_turn_context).await,
                None => None,
            };
            if let Some(snapshot) = next_turn_snapshot {
                current_context = snapshot.context.unwrap_or(current_context);
                config.model = snapshot.model.unwrap_or(config.model.clone());
                if let Some(level) = snapshot.thinking_level {
                    config.options.reasoning = model_thinking_level_to_reasoning(level);
                }
                if let Some(selection) = snapshot.thinking_selection {
                    config.options.thinking_selection = selection;
                }
                if let Some(abort_server_side_fallback) = snapshot.abort_server_side_fallback {
                    config.options.stream.request.abort_server_side_fallback = Some(abort_server_side_fallback);
                }
            }
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                if let Some(queue) = drained_terminating_queue
                    && let Some(restore) = config.restore_pending_messages.as_ref()
                {
                    restore(queue, pending_messages.clone()).await;
                }
                send(&emit, AgentEvent::AgentEnd { messages: new_messages.clone() }).await;
                return;
            }
            if drained_terminating_queue.is_some() {
                refresh_terminating_queue_drain(&mut drained_terminating_queue, &mut pending_messages, &config).await;
                if pending_messages.is_empty() {
                    send(&emit, AgentEvent::AgentEnd { messages: new_messages.clone() }).await;
                    return;
                }
                drained_terminating_queue = None;
            }
            if !tool_batch_terminated {
                pending_messages = get_messages(&config.get_steering_messages).await;
            }
        }

        // Agent would stop here. Check for follow-up messages.
        let follow_up_messages = get_messages(&config.get_follow_up_messages).await;
        if !follow_up_messages.is_empty() {
            // Set as pending so inner loop processes them
            pending_messages = follow_up_messages;
            continue;
        }

        // No more messages, exit
        break;
    }

    send(&emit, AgentEvent::AgentEnd { messages: new_messages.clone() }).await;
}

/// Build the provider context using the same transform and conversion pipeline as an agent request.
pub async fn build_provider_context(
    context: &AgentContext,
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
) -> Context {
    let mut messages = context.messages.clone();
    if let Some(transform) = config.transform_context.as_ref() {
        messages = transform(messages, signal.cloned()).await;
    }
    let tools: Option<Vec<Tool>> = context.tools.as_ref().map(|tools| tools.iter().map(|tool| tool.tool.clone()).collect());
    Context {
        system_prompt: Some(context.system_prompt.clone()),
        messages: (config.convert_to_llm)(messages).await,
        tools,
    }
}

struct StreamedAssistantResponse {
    message: AssistantMessage,
    provider_tool_results: Vec<ToolResultMessage>,
}

#[derive(Debug, Clone, Copy)]
struct ThinkingTiming {
    started_at: i64,
    ended_at: Option<i64>,
}

/// Stream an assistant response from the LLM.
/// This is where AgentMessage[] gets transformed to Message[] for the LLM.
async fn stream_assistant_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
    stream_function: StreamFn,
    stream_idle_timeout_ms: Option<u64>,
) -> StreamedAssistantResponse {
    let mut partial_message: Option<AssistantMessage> = None;
    let mut added_partial = false;
    // Tool results delivered by a provider that executes tools mid-stream
    // (Cursor's exec channel). Buffered here and appended by the caller right
    // after the assistant message so pairs stay adjacent in the transcript.
    let provider_tool_results: Arc<Mutex<Vec<ToolResultMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let mut thinking_timing: BTreeMap<usize, ThinkingTiming> = BTreeMap::new();

    // Dedicated controller for the provider request so the loop can tear the
    // request down itself (idle timeout), not only when the caller aborts.
    let request_abort_controller = AbortController::new();
    let mut detach_caller_abort: Option<maho_ai::utils::abort::ListenerId> = None;
    if let Some(signal) = signal.as_ref() {
        if signal.aborted() {
            request_abort_controller.abort(signal.reason());
        } else {
            let controller = request_abort_controller.clone();
            let reason = signal.clone();
            let listener = signal.add_abort_listener(move |_| controller.abort(reason.reason()));
            detach_caller_abort = Some(listener);
        }
    }

    let outcome: Result<StreamedAssistantResponse, AgentStreamError> = async {
        let llm_context = build_provider_context(context, config, signal.as_ref()).await;

        // Resolve API key (important for expiring tokens)
        let resolved_api_key = match config.get_api_key.as_ref() {
            Some(get_api_key) => get_api_key(config.model.provider.clone()).await,
            None => None,
        }
        .or_else(|| config.options.stream.request.api_key.clone());

        let mut request_options: AgentStreamOptions = AgentStreamOptions {
            simple: config.options.clone(),
            exec_handlers: None,
            on_tool_result: None,
        };
        request_options.simple.stream.request.stream_kind = Some(StreamKind::Main);
        request_options.simple.stream.request.api_key = resolved_api_key;
        request_options.simple.stream.request.signal = Some(request_abort_controller.signal());
        // Cursor exec bridging (ignored by every other provider): handlers
        // execute mid-stream; their paired results buffer here.
        if let Some(cursor_exec_handlers) = config.cursor_exec_handlers.as_ref() {
            let handlers = match cursor_exec_handlers {
                CursorExecHandlersConfig::Handlers(handlers) => Arc::clone(handlers),
                CursorExecHandlersConfig::Factory(factory) => factory(signal.clone()),
            };
            let buffer = provider_tool_results.clone();
            let sink: CursorToolResultHandler = Arc::new(move |result: ToolResultMessage| {
                buffer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(result.clone());
                Box::pin(async { None })
            });
            request_options.exec_handlers = Some(handlers);
            request_options.on_tool_result = Some(sink);
        }

        let response = stream_function(&config.model, &llm_context, Some(request_options));

        let idle_error: Arc<dyn Fn(AgentStreamError) + Send + Sync> = {
            let controller = request_abort_controller.clone();
            Arc::new(move |error: AgentStreamError| {
                controller.abort(Some(AbortReason::new("StreamIdleTimeoutError", error.message())));
            })
        };
        let mut event_reader = AssistantEventReader::new(
            response.clone(),
            stream_idle_timeout_ms,
            request_abort_controller.signal(),
            Some(idle_error),
            config.stream_start_timeout_ms,
            signal.clone(),
        );

        let mut finalized: Option<AssistantMessage> = None;
        loop {
            let next = event_reader.next().await?;
            let Some(event) = next else { break };
            let terminal = match &event {
                AssistantMessageEvent::Done { .. } => Some(TerminalAssistantMessageEvent::Done),
                AssistantMessageEvent::Error { reason, .. } => Some(TerminalAssistantMessageEvent::Error {
                    reason: match reason {
                        ErrorReason::Aborted => StopReason::Aborted,
                        ErrorReason::Error => StopReason::Error,
                    },
                }),
                _ => None,
            };
            if let Some(terminal) = terminal {
                let final_message =
                    normalize_terminal_assistant_message(response.result().await.map_err(stream_error)?, terminal);
                finalized = Some(final_message);
                break;
            }
            match &event {
                AssistantMessageEvent::Start { partial } => {
                    partial_message = Some(partial.clone());
                    context.messages.push(assistant_event(partial));
                    added_partial = true;
                    send(emit, AgentEvent::MessageStart { message: assistant_event(partial) }).await;
                }
                AssistantMessageEvent::ThinkingStart { content_index, partial } => {
                    if partial_message.is_some() {
                        let timing = thinking_timing
                            .entry(*content_index)
                            .or_insert(ThinkingTiming { started_at: now_ms(), ended_at: None });
                        let (started_at, ended_at) = (timing.started_at, timing.ended_at);
                        let mut updated = partial.clone();
                        set_thinking_block_timing(&mut updated, *content_index, started_at, ended_at);
                        emit_message_update(emit, context, &mut partial_message, updated, &event).await;
                    }
                }
                AssistantMessageEvent::ThinkingDelta { content_index, partial, .. } => {
                    if partial_message.is_some() {
                        let mut updated = partial.clone();
                        if let Some(timing) = thinking_timing.get(content_index) {
                            set_thinking_block_timing(&mut updated, *content_index, timing.started_at, timing.ended_at);
                        }
                        emit_message_update(emit, context, &mut partial_message, updated, &event).await;
                    }
                }
                AssistantMessageEvent::ThinkingEnd { content_index, partial, .. } => {
                    if partial_message.is_some() {
                        let mut updated = partial.clone();
                        if let Some(timing) = thinking_timing.get_mut(content_index) {
                            timing.ended_at = Some(now_ms());
                            let (started_at, ended_at) = (timing.started_at, timing.ended_at);
                            set_thinking_block_timing(&mut updated, *content_index, started_at, ended_at);
                        }
                        emit_message_update(emit, context, &mut partial_message, updated, &event).await;
                    }
                }
                AssistantMessageEvent::TextStart { partial, .. }
                | AssistantMessageEvent::TextDelta { partial, .. }
                | AssistantMessageEvent::TextEnd { partial, .. }
                | AssistantMessageEvent::ToolcallStart { partial, .. }
                | AssistantMessageEvent::ToolcallDelta { partial, .. }
                | AssistantMessageEvent::ToolcallEnd { partial, .. } => {
                    if partial_message.is_some() {
                        emit_message_update(emit, context, &mut partial_message, partial.clone(), &event).await;
                    }
                }
                AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => {}
            }
        }

        let final_message = match finalized {
            Some(final_message) => final_message,
            None => response.result().await.map_err(stream_error)?,
        };
        let mut final_message = final_message;
        propagate_thinking_timing(&mut final_message, &thinking_timing);
        if added_partial {
            if let Some(last) = context.messages.last_mut() {
                *last = assistant_event(&final_message);
            }
        } else {
            context.messages.push(assistant_event(&final_message));
            send(emit, AgentEvent::MessageStart { message: assistant_event(&final_message) }).await;
        }
        send(emit, AgentEvent::MessageEnd { message: assistant_event(&final_message) }).await;
        Ok(StreamedAssistantResponse {
            message: final_message,
            provider_tool_results: provider_tool_results
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
        })
    }
    .await;

    let result = match outcome {
        Ok(response) => Ok(response),
        Err(error) => {
            let buffered = provider_tool_results
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if is_stream_idle_timeout_error(&error)
                && should_finalize_idle_as_stop(
                    partial_message.as_ref(),
                    &buffered,
                    |call| is_cursor_exec_resolved(call, &buffered),
                )
            {
                let partial = partial_message.clone();
                let mut final_message = AssistantMessage {
                    content: partial
                        .as_ref()
                        .map(|message| message.content.clone())
                        .unwrap_or_else(|| vec![ContentBlock::text("")]),
                    api: partial.as_ref().map(|m| m.api.clone()).unwrap_or_else(|| config.model.api.clone()),
                    provider: partial
                        .as_ref()
                        .map(|m| m.provider.clone())
                        .unwrap_or_else(|| config.model.provider.clone()),
                    model: partial.as_ref().map(|m| m.model.clone()).unwrap_or_else(|| config.model.id.clone()),
                    response_model: partial.as_ref().and_then(|m| m.response_model.clone()),
                    response_id: partial.as_ref().and_then(|m| m.response_id.clone()),
                    provider_thinking_level: partial.as_ref().and_then(|m| m.provider_thinking_level.clone()),
                    diagnostics: partial.as_ref().and_then(|m| m.diagnostics.clone()),
                    usage: partial.as_ref().map(|m| m.usage).unwrap_or_else(empty_usage),
                    stop_reason: StopReason::Stop,
                    stop_details: None,
                    deferred: None,
                    error_message: None,
                    abort_source: None,
                    raw_stop_reason: None,
                    end_turn: None,
                    timestamp: partial.as_ref().map(|m| m.timestamp).unwrap_or_else(now_ms),
                };
                propagate_thinking_timing(&mut final_message, &thinking_timing);
                if added_partial {
                    if let Some(last) = context.messages.last_mut() {
                        *last = assistant_event(&final_message);
                    }
                } else {
                    context.messages.push(assistant_event(&final_message));
                    send(emit, AgentEvent::MessageStart { message: assistant_event(&final_message) }).await;
                }
                send(emit, AgentEvent::MessageEnd { message: assistant_event(&final_message) }).await;
                Ok(StreamedAssistantResponse { message: final_message, provider_tool_results: buffered })
            } else {
                let aborted = signal.as_ref().is_some_and(AbortSignal::aborted);
                let reason = if aborted { StopReason::Aborted } else { StopReason::Error };
                let mut final_message = create_terminal_failure_assistant_message(
                    &config.model,
                    reason,
                    &error,
                    partial_message.as_ref(),
                );
                propagate_thinking_timing(&mut final_message, &thinking_timing);
                if added_partial {
                    if let Some(last) = context.messages.last_mut() {
                        *last = assistant_event(&final_message);
                    }
                } else {
                    context.messages.push(assistant_event(&final_message));
                    send(emit, AgentEvent::MessageStart { message: assistant_event(&final_message) }).await;
                }
                send(emit, AgentEvent::MessageEnd { message: assistant_event(&final_message) }).await;
                Ok(StreamedAssistantResponse { message: final_message, provider_tool_results: buffered })
            }
        }
    };

    request_abort_controller.abort(None);
    if let Some(signal) = signal.as_ref()
        && let Some(listener) = detach_caller_abort
    {
        signal.remove_abort_listener(listener);
    }

    match result {
        Ok(response) => response,
        Err(error) => {
            // The failure path above always converts to a terminal message; this arm only
            // exists so the function stays total.
            let mut final_message =
                create_terminal_failure_assistant_message(&config.model, StopReason::Error, &error, None);
            propagate_thinking_timing(&mut final_message, &thinking_timing);
            context.messages.push(assistant_event(&final_message));
            StreamedAssistantResponse { message: final_message, provider_tool_results: Vec::new() }
        }
    }
}

async fn emit_message_update(
    emit: &AgentEventSink,
    context: &mut AgentContext,
    partial_message: &mut Option<AssistantMessage>,
    updated: AssistantMessage,
    event: &AssistantMessageEvent,
) {
    *partial_message = Some(updated.clone());
    if let Some(last) = context.messages.last_mut() {
        *last = assistant_event(&updated);
    }
    send(
        emit,
        AgentEvent::MessageUpdate {
            message: assistant_event(&updated),
            assistant_message_event: event.clone(),
        },
    )
    .await;
}

fn set_thinking_block_timing(
    message: &mut AssistantMessage,
    content_index: usize,
    started_at: i64,
    ended_at: Option<i64>,
) {
    if let Some(ContentBlock::Thinking(block)) = message.content.get_mut(content_index) {
        block.started_at = Some(started_at);
        if ended_at.is_some() {
            block.ended_at = ended_at;
        }
    }
}

fn propagate_thinking_timing(message: &mut AssistantMessage, thinking_timing: &BTreeMap<usize, ThinkingTiming>) {
    if thinking_timing.is_empty() {
        return;
    }
    for (content_index, timing) in thinking_timing {
        let ended_at = timing.ended_at.unwrap_or_else(now_ms);
        if let Some(ContentBlock::Thinking(block)) = message.content.get_mut(*content_index) {
            block.started_at = Some(timing.started_at);
            block.ended_at = Some(ended_at);
        }
    }
}

fn empty_usage() -> Usage {
    Usage::default()
}

fn stream_error(error: StreamError) -> AgentStreamError {
    AgentStreamError::Other { message: error.message }
}

/// `AbortError`: the reason the signal carries, else the DOM default.
fn abort_error(signal: Option<&AbortSignal>) -> AgentStreamError {
    match signal.and_then(AbortSignal::reason) {
        Some(reason) => AgentStreamError::Other { message: reason.message },
        None => AgentStreamError::Other { message: "Request was aborted".to_owned() },
    }
}

fn normalize_timeout_ms(timeout_ms: Option<u64>) -> Option<u64> {
    timeout_ms.filter(|timeout_ms| *timeout_ms > 0)
}

struct AssistantEventReader {
    stream: AssistantMessageEventStream,
    idle_timeout_ms: Option<u64>,
    start_timeout_ms: Option<u64>,
    saw_first_event: bool,
    signal: Option<AbortSignal>,
    on_idle_timeout: Option<Arc<dyn Fn(AgentStreamError) + Send + Sync>>,
}

impl AssistantEventReader {
    fn new(
        stream: AssistantMessageEventStream,
        timeout_ms: Option<u64>,
        signal: AbortSignal,
        on_idle_timeout: Option<Arc<dyn Fn(AgentStreamError) + Send + Sync>>,
        stream_start_timeout_ms: Option<u64>,
        caller_signal: Option<AbortSignal>,
    ) -> Self {
        let _ = caller_signal;
        Self {
            stream,
            idle_timeout_ms: normalize_timeout_ms(timeout_ms),
            start_timeout_ms: normalize_timeout_ms(stream_start_timeout_ms),
            saw_first_event: false,
            signal: Some(signal),
            on_idle_timeout,
        }
    }

    /// The start bound applies only until the provider proves the request is alive with its
    /// first event; afterwards the idle bound governs as before.
    fn make_timeout_error(&self, timeout_ms: u64) -> AgentStreamError {
        if !self.saw_first_event && self.start_timeout_ms.is_some() {
            AgentStreamError::StartTimeout { timeout_ms }
        } else {
            AgentStreamError::IdleTimeout { timeout_ms }
        }
    }

    async fn next(&mut self) -> Result<Option<AssistantMessageEvent>, AgentStreamError> {
        if let Some(signal) = self.signal.as_ref()
            && signal.aborted()
        {
            return Err(abort_error(Some(signal)));
        }
        let use_start_bound = !self.saw_first_event && self.start_timeout_ms.is_some();
        let read_timeout_ms = if use_start_bound { self.start_timeout_ms } else { self.idle_timeout_ms };
        let result = match read_timeout_ms {
            Some(timeout_ms) => self.read_with_timeout(timeout_ms).await,
            None => self.read_without_timeout().await,
        }?;
        if result.is_some() {
            self.saw_first_event = true;
        }
        Ok(result)
    }

    /// With no idle bound the read still races the abort signal, exactly as
    /// `readNextAssistantEvent` does whenever an abort promise exists: aborting mid-stream must
    /// tear the request down instead of waiting forever for a provider event that never comes.
    async fn read_without_timeout(&mut self) -> Result<Option<AssistantMessageEvent>, AgentStreamError> {
        let signal = self.signal.clone();
        let abort_future = async move {
            match signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            biased;
            _ = abort_future => Err(abort_error(self.signal.as_ref())),
            result = self.stream.next() => result.map_err(stream_error),
        }
    }

    async fn read_with_timeout(
        &mut self,
        timeout_ms: u64,
    ) -> Result<Option<AssistantMessageEvent>, AgentStreamError> {
        let mut deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let signal = self.signal.clone();
            let abort_future = async move {
                match signal {
                    Some(signal) => signal.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                biased;
                _ = abort_future => {
                    return Err(abort_error(self.signal.as_ref()));
                }
                _ = tokio::time::sleep(remaining) => {
                    // A provider executing a server-requested tool locally (Cursor's
                    // exec channel) legitimately emits no events while the tool runs.
                    // That silence is tracked as local work on the stream; re-arm the
                    // idle bound instead of killing a healthy request.
                    if self.stream.has_pending_local_work() {
                        deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
                        continue;
                    }
                    let error = self.make_timeout_error(timeout_ms);
                    if let Some(on_idle_timeout) = self.on_idle_timeout.as_ref() {
                        on_idle_timeout(error.clone());
                    }
                    return Err(error);
                }
                result = self.stream.next() => {
                    return result.map_err(stream_error);
                }
            }
        }
    }
}

fn create_incomplete_tool_call_error_message(tool_name: &str, error_message: Option<&str>) -> String {
    match error_message {
        Some(error_message) => {
            let separator = if error_message.ends_with('.') { "" } else { "." };
            format!("{error_message}{separator} Re-issue the tool call with complete arguments.")
        }
        None => format!(
            "Tool call \"{tool_name}\" was not executed: the response ended before the tool call was complete because it hit the output token limit. Re-issue the tool call with complete arguments."
        ),
    }
}

/// Whether a tool call was already executed by a provider's mid-stream exec channel.
///
/// TS stamps the marker on the streamed block (`kCursorExecResolved`); this port keeps that
/// marker as side-car state inside the provider, and every exec-resolved call's paired
/// `ToolResultMessage` is delivered to the loop through `onToolResult` before the assistant
/// message settles. A call is therefore resolved exactly when a buffered provider tool result
/// carries its id.
fn is_cursor_exec_resolved(tool_call: &ToolCall, provider_tool_results: &[ToolResultMessage]) -> bool {
    provider_tool_results
        .iter()
        .any(|result| result.tool_call_id == tool_call.id)
}

struct ExecutedToolCallBatch {
    messages: Vec<ToolResultMessage>,
    terminate: bool,
}

/// Fail all tool calls from an assistant message that was truncated by the
/// output token limit. Streamed tool-call arguments are finalized with a
/// best-effort JSON salvage parser, so a truncated message can yield tool calls
/// whose arguments parse and validate but are silently incomplete. None of them
/// are safe to execute; report each as an error so the model can re-issue them.
async fn fail_tool_calls_from_truncated_message(
    tool_calls: &[AgentToolCall],
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for tool_call in tool_calls {
        send(
            emit,
            AgentEvent::ToolExecutionStart {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                args: serde_json::Value::Object(tool_call.arguments.clone()),
            },
        )
        .await;
        let finalized = FinalizedToolCallOutcome {
            tool_call: tool_call.clone(),
            result: create_error_tool_result(&create_incomplete_tool_call_error_message(&tool_call.name, None)),
            is_error: true,
        };
        emit_tool_execution_end(&finalized, emit).await;
        let tool_result_message = create_tool_result_message(&finalized);
        emit_tool_result_message(&tool_result_message, emit).await;
        messages.push(tool_result_message);
    }
    ExecutedToolCallBatch { messages, terminate: false }
}

/// Execute tool calls from an assistant message.
async fn execute_tool_calls(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    // Same filter as the loop's collection site (defense in depth): a block
    // stamped `kCursorExecResolved` was already executed by Cursor's exec
    // channel and its result buffered; running it again would duplicate a
    // side-effecting tool.
    let provider_tool_results: Vec<ToolResultMessage> = Vec::new();
    let tool_calls: Vec<AgentToolCall> = assistant_message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) if !is_cursor_exec_resolved(call, &provider_tool_results) => Some(call.clone()),
            _ => None,
        })
        .collect();
    if config.tool_execution_mode() == ToolExecutionMode::Sequential {
        return execute_tool_calls_sequential(current_context, assistant_message, &tool_calls, config, signal, emit).await;
    }
    execute_tool_calls_parallel(current_context, assistant_message, &tool_calls, config, signal, emit).await
}

async fn execute_tool_calls_sequential(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut finalized_calls: Vec<FinalizedToolCallOutcome> = Vec::new();
    let mut messages: Vec<ToolResultMessage> = Vec::new();

    for tool_call in tool_calls {
        let tool = resolve_call_tool(current_context, tool_call, config).await;
        send(
            emit,
            AgentEvent::ToolExecutionStart {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool.as_ref().map(|tool| tool.name().to_owned()).unwrap_or_else(|| tool_call.name.clone()),
                args: serde_json::Value::Object(tool_call.arguments.clone()),
            },
        )
        .await;

        let preparation = prepare_tool_call(current_context, assistant_message, tool_call, tool.as_ref(), config, signal.clone()).await;
        let finalized = match &preparation {
            ToolCallPreparation::Immediate { tool_call, result, is_error } => FinalizedToolCallOutcome {
                tool_call: tool_call.clone(),
                result: result.clone(),
                is_error: *is_error,
            },
            ToolCallPreparation::Prepared { .. } => {
                let executed = execute_prepared_tool_call(&preparation, signal.clone(), emit).await;
                finalize_executed_tool_call(current_context, assistant_message, &preparation, executed, config, signal.clone()).await
            }
        };

        emit_tool_execution_end(&finalized, emit).await;
        let tool_result_message = create_tool_result_message(&finalized);
        emit_tool_result_message(&tool_result_message, emit).await;
        finalized_calls.push(finalized);
        messages.push(tool_result_message);

        if signal.as_ref().is_some_and(AbortSignal::aborted) {
            break;
        }
    }

    let terminate = should_terminate_tool_batch(&finalized_calls);
    ExecutedToolCallBatch { messages, terminate }
}

/// A single finalized tool call, awaited by both its dependents and the collector.
///
/// TS hands out one promise per call and lets several `await`s share it; a `oneshot` receiver is
/// single-use and not `Clone`, so the port carries the outcome on a `watch` channel, which every
/// holder can clone and await.
type FinalizedReceiver = tokio::sync::watch::Receiver<Option<FinalizedToolCallOutcome>>;

fn finalized_channel() -> (tokio::sync::watch::Sender<Option<FinalizedToolCallOutcome>>, FinalizedReceiver) {
    tokio::sync::watch::channel(None)
}

/// `await finalizedCall`; `None` means the producing task died without a value (TS would reject).
async fn await_finalized(receiver: &FinalizedReceiver) -> Option<FinalizedToolCallOutcome> {
    let mut receiver = receiver.clone();
    loop {
        {
            let current = receiver.borrow();
            if let Some(outcome) = current.as_ref() {
                return Some(outcome.clone());
            }
        }
        if receiver.changed().await.is_err() {
            return None;
        }
    }
}

async fn execute_tool_calls_parallel(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut prepared_calls: Vec<ToolCallPreparation> = Vec::new();

    for tool_call in tool_calls {
        let tool = resolve_call_tool(current_context, tool_call, config).await;
        send(
            emit,
            AgentEvent::ToolExecutionStart {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool.as_ref().map(|tool| tool.name().to_owned()).unwrap_or_else(|| tool_call.name.clone()),
                args: serde_json::Value::Object(tool_call.arguments.clone()),
            },
        )
        .await;

        let preparation =
            prepare_tool_call(current_context, assistant_message, tool_call, tool.as_ref(), config, signal.clone()).await;
        prepared_calls.push(preparation);

        if signal.as_ref().is_some_and(AbortSignal::aborted) {
            break;
        }
    }

    // Dependencies are assigned in the second phase after all preflight hooks have completed,
    // so a later preflight abort vetoes every execution (the TS first-phase dependency
    // computation is likewise discarded there).
    let mut finalized_calls: Vec<FinalizedReceiver> = Vec::new();
    let mut previous_sequential: Option<FinalizedReceiver> = None;
    let mut previous_wave: Vec<FinalizedReceiver> = Vec::new();

    for preparation in &prepared_calls {
        let is_sequential = is_sequential_tool_call(current_context, preparation.tool_call());
        let dependencies: Vec<FinalizedReceiver> = if is_sequential {
            let mut dependencies: Vec<FinalizedReceiver> = Vec::new();
            if let Some(previous) = previous_sequential.as_ref() {
                dependencies.push(previous.clone());
            }
            dependencies.extend(previous_wave.iter().cloned());
            dependencies
        } else if let Some(previous) = previous_sequential.as_ref() {
            vec![previous.clone()]
        } else {
            Vec::new()
        };

        let (sender, receiver) = finalized_channel();
        let context = current_context.clone();
        let assistant = assistant_message.clone();
        let preparation = preparation.clone();
        let config = config.clone();
        let signal = signal.clone();
        let emit = emit.clone();
        tokio::spawn(async move {
            for dependency in &dependencies {
                let _ = await_finalized(dependency).await;
            }
            let finalized = if signal.as_ref().is_some_and(AbortSignal::aborted) {
                FinalizedToolCallOutcome {
                    tool_call: preparation.tool_call().clone(),
                    result: create_error_tool_result("Operation aborted"),
                    is_error: true,
                }
            } else {
                run_prepared_tool_call(&context, &assistant, &preparation, &config, signal.clone(), &emit).await
            };
            emit_tool_execution_end(&finalized, &emit).await;
            let _ = sender.send(Some(finalized));
        });
        finalized_calls.push(receiver.clone());
        if is_sequential {
            previous_sequential = Some(receiver);
            previous_wave = Vec::new();
        } else {
            previous_wave.push(receiver);
        }
    }

    let mut ordered_finalized_calls: Vec<FinalizedToolCallOutcome> = Vec::new();
    for receiver in &finalized_calls {
        if let Some(finalized) = await_finalized(receiver).await {
            ordered_finalized_calls.push(finalized);
        }
    }
    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for finalized in &ordered_finalized_calls {
        let tool_result_message = create_tool_result_message(finalized);
        emit_tool_result_message(&tool_result_message, emit).await;
        messages.push(tool_result_message);
    }

    let terminate = should_terminate_tool_batch(&ordered_finalized_calls);
    ExecutedToolCallBatch { messages, terminate }
}

async fn run_prepared_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    preparation: &ToolCallPreparation,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
) -> FinalizedToolCallOutcome {
    if let ToolCallPreparation::Immediate { tool_call, result, is_error } = preparation {
        return FinalizedToolCallOutcome {
            tool_call: tool_call.clone(),
            result: result.clone(),
            is_error: *is_error,
        };
    }

    let executed = execute_prepared_tool_call(preparation, signal.clone(), emit).await;
    finalize_executed_tool_call(current_context, assistant_message, preparation, executed, config, signal).await
}

fn is_sequential_tool_call(current_context: &AgentContext, tool_call: &AgentToolCall) -> bool {
    current_context
        .tools
        .as_ref()
        .and_then(|tools| tools.iter().find(|tool| tool.name() == tool_call.name))
        .and_then(|tool| tool.execution_mode)
        == Some(ToolExecutionMode::Sequential)
}

/// `PreparedToolCall | ImmediateToolCallOutcome`.
#[derive(Clone)]
enum ToolCallPreparation {
    Prepared {
        tool_call: AgentToolCall,
        tool: AgentTool,
        args: serde_json::Value,
        requested_name: Option<String>,
    },
    Immediate {
        tool_call: AgentToolCall,
        result: AgentToolResult,
        is_error: bool,
    },
}

impl ToolCallPreparation {
    fn tool_call(&self) -> &AgentToolCall {
        match self {
            ToolCallPreparation::Prepared { tool_call, .. } => tool_call,
            ToolCallPreparation::Immediate { tool_call, .. } => tool_call,
        }
    }
}

struct ExecutedToolCallOutcome {
    result: AgentToolResult,
    is_error: bool,
}

#[derive(Clone)]
struct FinalizedToolCallOutcome {
    tool_call: AgentToolCall,
    result: AgentToolResult,
    is_error: bool,
}

fn should_terminate_tool_batch(finalized_calls: &[FinalizedToolCallOutcome]) -> bool {
    !finalized_calls.is_empty() && finalized_calls.iter().all(|finalized| finalized.result.terminate == Some(true))
}

/// `PreparedAgentToolCall`.
pub struct PreparedAgentToolCall {
    pub tool_call: AgentToolCall,
    pub tool: AgentTool,
    pub args: serde_json::Value,
}

pub fn prepare_agent_tool_call(tool: &AgentTool, tool_call: &AgentToolCall) -> Result<PreparedAgentToolCall, String> {
    let prepared_tool_call = prepare_agent_tool_call_arguments(tool, tool_call);
    let args = validate_tool_arguments(&tool.tool, &prepared_tool_call).map_err(|error| error.to_string())?;
    Ok(PreparedAgentToolCall { tool_call: prepared_tool_call, tool: tool.clone(), args })
}

async fn prepare_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &AgentToolCall,
    tool: Option<&AgentTool>,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
) -> ToolCallPreparation {
    if tool_call.incomplete == Some(true) {
        return ToolCallPreparation::Immediate {
            tool_call: tool_call.clone(),
            result: create_error_tool_result(&create_incomplete_tool_call_error_message(
                &tool_call.name,
                tool_call.error_message.as_deref(),
            )),
            is_error: true,
        };
    }

    let Some(tool) = tool else {
        let hint = config
            .removed_tool_hints
            .as_ref()
            .and_then(|hints| hints.get(&tool_call.name).cloned());
        let message = match hint {
            None => format!("Tool {} not found", tool_call.name),
            Some(hint) => format!("Tool {} not found. {}", tool_call.name, hint),
        };
        return ToolCallPreparation::Immediate {
            tool_call: tool_call.clone(),
            result: create_error_tool_result(&message),
            is_error: true,
        };
    };
    if tool.name() != tool_call.name {
        let requested_name = tool_call.name.clone();
        let mut renamed = tool_call.clone();
        renamed.name = tool.name().to_owned();
        let outcome = prepare_resolved_tool_call(current_context, assistant_message, &renamed, tool, config, signal).await;
        return match outcome {
            ToolCallPreparation::Prepared { tool_call, tool, args, .. } => ToolCallPreparation::Prepared {
                tool_call,
                tool,
                args,
                requested_name: Some(requested_name),
            },
            ToolCallPreparation::Immediate { tool_call, result, is_error } => ToolCallPreparation::Immediate {
                tool_call,
                result: with_tool_name_correction(result, &requested_name, tool.name()),
                is_error,
            },
        };
    }
    prepare_resolved_tool_call(current_context, assistant_message, tool_call, tool, config, signal).await
}

async fn prepare_resolved_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &AgentToolCall,
    tool: &AgentTool,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
) -> ToolCallPreparation {
    let prepared_tool_call = match prepare_agent_tool_call(tool, tool_call) {
        Ok(prepared_tool_call) => prepared_tool_call,
        Err(message) => {
            return ToolCallPreparation::Immediate {
                tool_call: tool_call.clone(),
                result: create_error_tool_result(&message),
                is_error: true,
            };
        }
    };
    let validated_args = prepared_tool_call.args.clone();
    let mut effective_args = validated_args.clone();
    if let Some(before_tool_call) = config.before_tool_call.clone() {
        let before_result = before_tool_call(
            crate::types::BeforeToolCallContext {
                assistant_message: assistant_message.clone(),
                tool_call: prepared_tool_call.tool_call.clone(),
                args: validated_args.clone(),
                context: current_context.clone(),
            },
            signal.clone(),
        )
        .await;
        if signal.as_ref().is_some_and(AbortSignal::aborted) {
            return ToolCallPreparation::Immediate {
                tool_call: tool_call.clone(),
                result: create_error_tool_result("Operation aborted"),
                is_error: true,
            };
        }
        match before_result {
            Some(before_result) if before_result.block == Some(true) => {
                let mut result = create_error_tool_result(
                    before_result.reason.as_deref().unwrap_or("Tool execution was blocked"),
                );
                if before_result.terminate == Some(true) {
                    result.terminate = Some(true);
                }
                return ToolCallPreparation::Immediate { tool_call: tool_call.clone(), result, is_error: true };
            }
            Some(before_result) => {
                if let Some(replacement) = before_result.args {
                    effective_args = replacement;
                }
            }
            None => {}
        }
    }
    if signal.as_ref().is_some_and(AbortSignal::aborted) {
        return ToolCallPreparation::Immediate {
            tool_call: tool_call.clone(),
            result: create_error_tool_result("Operation aborted"),
            is_error: true,
        };
    }
    ToolCallPreparation::Prepared {
        tool_call: prepared_tool_call.tool_call,
        tool: tool.clone(),
        args: effective_args,
        requested_name: None,
    }
}

/// Resolves as soon as `signal` aborts, so a tool that never settles and never
/// observes its signal cannot pin the run forever. Without this the abort has no
/// wakeup once `execute()` is entered: no `agent_end`, the session never goes
/// idle, and every queued prompt parks behind the session work barrier while the
/// TUI shows "Running <tool>" with a dead ESC.
async fn abort_release_promise(signal: Option<AbortSignal>) {
    match signal {
        None => std::future::pending::<()>().await,
        Some(signal) => signal.cancelled().await,
    }
}

async fn execute_prepared_tool_call(
    prepared: &ToolCallPreparation,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCallOutcome {
    let ToolCallPreparation::Prepared { tool_call, tool, args, .. } = prepared else {
        return ExecutedToolCallOutcome {
            result: create_error_tool_result("Tool call was not prepared"),
            is_error: true,
        };
    };
    let update_events: Arc<Mutex<Vec<maho_ai::types::BoxFuture<'static, ()>>>> = Arc::new(Mutex::new(Vec::new()));
    let accepting_updates = Arc::new(AtomicBool::new(true));

    let on_update: AgentToolUpdateCallback = {
        let emit = emit.clone();
        let updates = update_events.clone();
        let accepting_updates = accepting_updates.clone();
        let tool_call_id = tool_call.id.clone();
        let tool_name = tool_call.name.clone();
        let args = serde_json::Value::Object(tool_call.arguments.clone());
        Arc::new(move |partial_result: AgentToolResult| {
            if !accepting_updates.load(Ordering::SeqCst) {
                return;
            }
            let event = AgentEvent::ToolExecutionUpdate {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                args: args.clone(),
                partial_result: serde_json::to_value(&partial_result).unwrap_or(serde_json::Value::Null),
            };
            updates.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(emit(event));
        })
    };

    let execution = (tool.execute)(
        tool_call.id.clone(),
        args.clone(),
        signal.clone(),
        Some(on_update.clone()),
    );
    // The execution is detached so a losing abort race leaves it running, as the TS
    // `Promise.race` does (the abandoned promise keeps executing there).
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let result = execution.await;
        let _ = sender.send(result);
    });

    let settled = if signal.is_some() {
        let abort_release = abort_release_promise(signal.clone());
        tokio::select! {
            result = receiver => Some(result),
            _ = abort_release => None,
        }
    } else {
        Some(receiver.await)
    };

    let outcome = match settled {
        None => {
            accepting_updates.store(false, Ordering::SeqCst);
            let pending: Vec<_> = update_events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .drain(..)
                .collect();
            for event in pending {
                event.await;
            }
            return ExecutedToolCallOutcome {
                result: create_error_tool_result("Tool execution aborted"),
                is_error: true,
            };
        }
        Some(Ok(result)) => {
            accepting_updates.store(false, Ordering::SeqCst);
            let pending: Vec<_> = update_events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .drain(..)
                .collect();
            for event in pending {
                event.await;
            }
            ExecutedToolCallOutcome { is_error: result.is_error == Some(true), result }
        }
        Some(Err(_)) => ExecutedToolCallOutcome {
            result: create_error_tool_result("Tool execution was cancelled"),
            is_error: true,
        },
    };
    accepting_updates.store(false, Ordering::SeqCst);
    outcome
}

async fn finalize_executed_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    prepared: &ToolCallPreparation,
    executed: ExecutedToolCallOutcome,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
) -> FinalizedToolCallOutcome {
    let ToolCallPreparation::Prepared { tool_call, requested_name, args, .. } = prepared else {
        return FinalizedToolCallOutcome {
            tool_call: prepared.tool_call().clone(),
            result: executed.result,
            is_error: executed.is_error,
        };
    };
    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(after_tool_call) = config.after_tool_call.clone() {
        let after_result = after_tool_call(
            crate::types::AfterToolCallContext {
                assistant_message: assistant_message.clone(),
                tool_call: tool_call.clone(),
                args: args.clone(),
                result: result.clone(),
                is_error,
                context: current_context.clone(),
            },
            signal.clone(),
        )
        .await;
        if let Some(after_result) = after_result {
            result = AgentToolResult {
                content: after_result.content.unwrap_or(result.content),
                details: after_result.details.unwrap_or(result.details),
                usage: after_result.usage.or(result.usage),
                added_tool_names: result.added_tool_names,
                terminate: after_result.terminate.or(result.terminate),
                is_error: result.is_error,
            };
            is_error = after_result.is_error.unwrap_or(is_error);
        }
    }
    if let Some(requested_name) = requested_name {
        result = with_tool_name_correction(result, requested_name, &tool_call.name);
    }

    FinalizedToolCallOutcome { tool_call: tool_call.clone(), result, is_error }
}

fn create_error_tool_result(message: &str) -> AgentToolResult {
    AgentToolResult {
        content: vec![ContentBlock::text(message)],
        details: serde_json::Value::Object(serde_json::Map::new()),
        usage: None,
        added_tool_names: None,
        terminate: None,
        is_error: None,
    }
}

async fn emit_tool_execution_end(finalized: &FinalizedToolCallOutcome, emit: &AgentEventSink) {
    send(
        emit,
        AgentEvent::ToolExecutionEnd {
            tool_call_id: finalized.tool_call.id.clone(),
            tool_name: finalized.tool_call.name.clone(),
            result: serde_json::to_value(&finalized.result).unwrap_or(serde_json::Value::Null),
            is_error: finalized.is_error,
        },
    )
    .await;
}

fn create_tool_result_message(finalized: &FinalizedToolCallOutcome) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        // Untyped tools (JS extensions) can return results without content; normalize
        // so the null never enters session history or provider payloads.
        content: finalized.result.content.clone(),
        details: Some(serde_json::to_value(&finalized.result.details).unwrap_or(serde_json::Value::Null)),
        usage: finalized.result.usage,
        added_tool_names: match finalized.result.added_tool_names.as_ref() {
            Some(names) if !names.is_empty() => Some(names.clone()),
            _ => None,
        },
        is_error: finalized.is_error,
        timestamp: now_ms(),
    }
}

async fn emit_tool_result_message(tool_result_message: &ToolResultMessage, emit: &AgentEventSink) {
    let message = AgentMessage::Llm(Message::ToolResult(tool_result_message.clone()));
    send(emit, AgentEvent::MessageStart { message: message.clone() }).await;
    send(emit, AgentEvent::MessageEnd { message }).await;
}
