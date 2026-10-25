//! Port of senpi packages/agent/src/agent-loop.ts.
//!
//! Agent loop that works with AgentMessage throughout. Transforms to Message[] only at the LLM
//! call boundary.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use maho_ai::api::cursor_agent::types::CursorToolResultHandler;
use maho_ai::types::transcript::{
    create_initial_system_message, get_current_tools, get_tool_state_changes, normalize_context,
};
use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, ContentBlock, Context,
    ErrorReason, Message, StopReason, StreamKind, SystemMessage, Tool, ToolCall, ToolResultMessage,
    Usage, UserContent,
};
use maho_ai::utils::abort::{AbortController, AbortReason, AbortSignal};
use maho_ai::utils::event_stream::{EventStream, StreamError};
use maho_ai::utils::validation::validate_tool_arguments;
use tokio::sync::oneshot;

use crate::assistant_terminal_state::{
    AgentStreamError, PROVIDER_RETRY_WATCHDOG_ABORT_ERROR_NAME, TerminalAssistantMessageEvent,
    create_terminal_failure_assistant_message, demote_tool_use_without_tool_calls,
    is_stream_idle_timeout_error, normalize_terminal_assistant_message, now_ms,
    promote_stop_with_pending_tool_calls, should_finalize_idle_as_stop,
    should_terminate_assistant_turn,
};
use crate::empty_assistant_recovery::with_empty_assistant_recovery;
use crate::response_timing::create_response_timing_recorder;
use crate::stream_fn::get_default_stream_fn;
use crate::tool_arguments::prepare_agent_tool_call_arguments;
use crate::tool_name_alias::{resolve_call_tool, with_tool_name_correction};
use crate::types::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentStreamOptions, AgentTool,
    AgentToolCall, AgentToolResult, AgentToolUpdateCallback, CursorExecHandlersConfig,
    PendingQueue, StreamFn, ToolExecutionMode, model_thinking_level_to_reasoning,
};

/// Event sink the loop writes into. TS uses `(event) => Promise<void> | void`.
pub type AgentEventSink =
    Arc<dyn Fn(AgentEvent) -> maho_ai::types::BoxFuture<'static, ()> + Send + Sync>;

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
    let prompts = declare_tool_changes(&context, prompts, Some(&config.model));
    let mut new_messages: Vec<AgentMessage> = prompts.clone();
    let mut current_context = context;
    current_context.messages.extend(prompts.iter().cloned());

    send(&emit, AgentEvent::AgentStart).await;
    send(&emit, AgentEvent::TurnStart).await;
    for prompt in &prompts {
        send(
            &emit,
            AgentEvent::MessageStart {
                message: prompt.clone(),
            },
        )
        .await;
        send(
            &emit,
            AgentEvent::MessageEnd {
                message: prompt.clone(),
            },
        )
        .await;
    }

    let stream_function = stream_fn.unwrap_or_else(get_default_stream_fn);
    run_loop(
        current_context,
        &mut new_messages,
        config,
        signal,
        emit,
        stream_function,
    )
    .await;
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
    let current_context = context;

    send(&emit, AgentEvent::AgentStart).await;
    send(&emit, AgentEvent::TurnStart).await;

    let stream_function = stream_fn.unwrap_or_else(get_default_stream_fn);
    run_loop(
        current_context,
        &mut new_messages,
        config,
        signal,
        emit,
        stream_function,
    )
    .await;
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
    let Some(queue) = *drained_terminating_queue else {
        return;
    };
    let Some(restore) = config.restore_pending_messages.as_ref() else {
        return;
    };
    restore(queue, pending_messages.clone()).await;
    *pending_messages = get_messages(&config.get_steering_messages).await;
    *drained_terminating_queue = if pending_messages.is_empty() {
        None
    } else {
        Some(PendingQueue::Steering)
    };
    if pending_messages.is_empty() {
        *pending_messages = get_messages(&config.get_follow_up_messages).await;
        *drained_terminating_queue = if pending_messages.is_empty() {
            None
        } else {
            Some(PendingQueue::FollowUp)
        };
    }
}

/// Main loop logic shared by agentLoop and agentLoopContinue.
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
    let mut explicit_continuation = false;
    let mut prepared_messages = Vec::new();
    let mut pending_source = PendingQueue::Steering;

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
                refresh_terminating_queue_drain(
                    &mut drained_terminating_queue,
                    &mut pending_messages,
                    &config,
                )
                .await;
                pending_source = drained_terminating_queue.unwrap_or(pending_source);
                if pending_messages.is_empty() && !explicit_continuation {
                    send(
                        &emit,
                        AgentEvent::AgentEnd {
                            messages: new_messages.clone(),
                        },
                    )
                    .await;
                    return;
                }
                drained_terminating_queue = None;
            }

            // Consumption must be durable before a queued message gets events or enters context.
            if !pending_messages.is_empty()
                && let Some(commit) = &config.commit_queued_consumption
                && let Err(error) = commit(pending_source, pending_messages.clone()).await
            {
                if let Some(restore) = &config.restore_pending_messages {
                    restore(pending_source, pending_messages).await;
                }
                emit_run_failure(&config, error, true, &emit).await;
                return;
            }
            {
                prepared_messages.append(&mut pending_messages);
                for message in declare_tool_changes(
                    &current_context,
                    std::mem::take(&mut prepared_messages),
                    Some(&config.model),
                ) {
                    send(
                        &emit,
                        AgentEvent::MessageStart {
                            message: message.clone(),
                        },
                    )
                    .await;
                    send(
                        &emit,
                        AgentEvent::MessageEnd {
                            message: message.clone(),
                        },
                    )
                    .await;
                    current_context.messages.push(message.clone());
                    new_messages.push(message);
                }
            }

            if let Some(prepare) = &config.prepare_request {
                let update = prepare(
                    crate::types::PrepareRequestContext {
                        context: current_context.clone(),
                        model: config.model.clone(),
                        thinking_level: config.options.reasoning.map_or(
                            maho_ai::types::ModelThinkingLevel::Off,
                            |level| match level {
                                maho_ai::types::ThinkingLevel::Minimal => {
                                    maho_ai::types::ModelThinkingLevel::Minimal
                                }
                                maho_ai::types::ThinkingLevel::Low => {
                                    maho_ai::types::ModelThinkingLevel::Low
                                }
                                maho_ai::types::ThinkingLevel::Medium => {
                                    maho_ai::types::ModelThinkingLevel::Medium
                                }
                                maho_ai::types::ThinkingLevel::High => {
                                    maho_ai::types::ModelThinkingLevel::High
                                }
                                maho_ai::types::ThinkingLevel::Xhigh => {
                                    maho_ai::types::ModelThinkingLevel::Xhigh
                                }
                                maho_ai::types::ThinkingLevel::Max => {
                                    maho_ai::types::ModelThinkingLevel::Max
                                }
                            },
                        ),
                    },
                    signal.clone(),
                )
                .await;
                if let Some(update) = update {
                    if let Some(error) = update.failure {
                        emit_run_failure(&config, error, false, &emit).await;
                        return;
                    }
                    current_context = update.context.unwrap_or(current_context);
                    apply_loop_update(
                        &mut config,
                        update.model,
                        update.thinking_level,
                        update.thinking_selection,
                        update.abort_server_side_fallback,
                    );
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
            let message = demote_tool_use_without_tool_calls(promote_stop_with_pending_tool_calls(
                streamed.message,
            ));
            let provider_tool_results = streamed.provider_tool_results;
            new_messages.push(assistant_event(&message));
            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            for result in &provider_tool_results {
                let message = AgentMessage::Llm(Message::ToolResult(result.clone()));
                send(
                    &emit,
                    AgentEvent::MessageStart {
                        message: message.clone(),
                    },
                )
                .await;
                send(
                    &emit,
                    AgentEvent::MessageEnd {
                        message: message.clone(),
                    },
                )
                .await;
                current_context.messages.push(message);
                new_messages.push(AgentMessage::Llm(Message::ToolResult(result.clone())));
                tool_results.push(result.clone());
            }

            if should_terminate_assistant_turn(&message) {
                if let Some(finish) = &config.finish_turn {
                    finish(
                        crate::types::AgentTurnContext {
                            message: message.clone(),
                            tool_results: tool_results.clone(),
                            context: current_context.clone(),
                            new_messages: new_messages.clone(),
                        },
                        signal.clone(),
                    )
                    .await;
                }
                send(
                    &emit,
                    AgentEvent::TurnEnd {
                        message: assistant_event(&message),
                        tool_results: tool_results.clone(),
                    },
                )
                .await;
                send(
                    &emit,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await;
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
                    execute_tool_calls(
                        &current_context,
                        &message,
                        &provider_tool_results,
                        &config,
                        signal.clone(),
                        &emit,
                    )
                    .await
                };
                tool_results.extend(executed_tool_batch.messages.iter().cloned());
                tool_batch_terminated = executed_tool_batch.terminate;
                has_more_tool_calls = !executed_tool_batch.terminate;

                for result in &executed_tool_batch.messages {
                    current_context
                        .messages
                        .push(AgentMessage::Llm(Message::ToolResult(result.clone())));
                    new_messages.push(AgentMessage::Llm(Message::ToolResult(result.clone())));
                }
            }

            let decision = match &config.finish_turn {
                Some(finish) => {
                    let next_turn_context = crate::types::ShouldStopAfterTurnContext {
                        message: message.clone(),
                        tool_results: tool_results.clone(),
                        context: current_context.clone(),
                        new_messages: new_messages.clone(),
                    };
                    finish(next_turn_context, signal.clone()).await
                }
                None => None,
            };
            let turn_end_tool_results =
                if config.should_stop_after_turn.is_some() || config.prepare_next_turn.is_some() {
                    tool_results.clone()
                } else {
                    std::mem::take(&mut tool_results)
                };
            send(
                &emit,
                AgentEvent::TurnEnd {
                    message: assistant_event(&message),
                    tool_results: turn_end_tool_results,
                },
            )
            .await;
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                send(
                    &emit,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await;
                return;
            }

            if decision == Some(crate::types::AgentTurnDecision::End) {
                send(
                    &emit,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await;
                return;
            }
            explicit_continuation = decision == Some(crate::types::AgentTurnDecision::Continue);
            if let Some(should_stop) = config.should_stop_after_turn.clone()
                && should_stop(crate::types::ShouldStopAfterTurnContext {
                    message: message.clone(),
                    tool_results: tool_results.clone(),
                    context: current_context.clone(),
                    new_messages: new_messages.clone(),
                })
                .await
            {
                send(
                    &emit,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await;
                return;
            }
            if tool_batch_terminated {
                pending_source = PendingQueue::Steering;
                pending_messages = get_messages(&config.get_steering_messages).await;
                if !pending_messages.is_empty() {
                    drained_terminating_queue = Some(PendingQueue::Steering);
                }
                if pending_messages.is_empty() {
                    pending_source = PendingQueue::FollowUp;
                    pending_messages = get_messages(&config.get_follow_up_messages).await;
                    if !pending_messages.is_empty() {
                        drained_terminating_queue = Some(PendingQueue::FollowUp);
                    }
                }
                if pending_messages.is_empty() && !explicit_continuation {
                    send(
                        &emit,
                        AgentEvent::AgentEnd {
                            messages: new_messages.clone(),
                        },
                    )
                    .await;
                    return;
                }
                // Give queue owners a boundary before preparation refreshes the drained
                // snapshot, so a clear or replacement wins before admission.
                if !pending_messages.is_empty() {
                    send(&emit, AgentEvent::TurnStart).await;
                    turn_start_already_emitted = true;
                }
            }

            let next_turn_snapshot = match config.prepare_next_turn.clone() {
                Some(prepare) => {
                    prepare(crate::types::ShouldStopAfterTurnContext {
                        message: message.clone(),
                        tool_results,
                        context: current_context.clone(),
                        new_messages: new_messages.clone(),
                    })
                    .await
                }
                None => None,
            };
            if let Some(error_message) = next_turn_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.failure.clone())
            {
                if let Some(queue) = drained_terminating_queue
                    && let Some(restore) = config.restore_pending_messages.as_ref()
                {
                    restore(queue, pending_messages.clone()).await;
                }
                // senpi `handleRunFailure(error, aborted=false)`.
                let failure = assistant_event(&create_terminal_failure_assistant_message(
                    &config.model,
                    StopReason::Error,
                    &AgentStreamError::Other {
                        message: error_message,
                    },
                    None,
                ));
                send(
                    &emit,
                    AgentEvent::MessageStart {
                        message: failure.clone(),
                    },
                )
                .await;
                send(
                    &emit,
                    AgentEvent::MessageEnd {
                        message: failure.clone(),
                    },
                )
                .await;
                send(
                    &emit,
                    AgentEvent::TurnEnd {
                        message: failure.clone(),
                        tool_results: Vec::new(),
                    },
                )
                .await;
                send(
                    &emit,
                    AgentEvent::AgentEnd {
                        messages: vec![failure],
                    },
                )
                .await;
                return;
            }
            if let Some(snapshot) = next_turn_snapshot {
                current_context = snapshot.context.unwrap_or(current_context);
                prepared_messages = snapshot.messages.unwrap_or_default();
                apply_loop_update(
                    &mut config,
                    snapshot.model,
                    snapshot.thinking_level,
                    snapshot.thinking_selection,
                    snapshot.abort_server_side_fallback,
                );
            }
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                if let Some(queue) = drained_terminating_queue
                    && let Some(restore) = config.restore_pending_messages.as_ref()
                {
                    restore(queue, pending_messages.clone()).await;
                }
                send(
                    &emit,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await;
                return;
            }
            if drained_terminating_queue.is_some() {
                refresh_terminating_queue_drain(
                    &mut drained_terminating_queue,
                    &mut pending_messages,
                    &config,
                )
                .await;
                pending_source = drained_terminating_queue.unwrap_or(pending_source);
                if pending_messages.is_empty() && !explicit_continuation {
                    send(
                        &emit,
                        AgentEvent::AgentEnd {
                            messages: new_messages.clone(),
                        },
                    )
                    .await;
                    return;
                }
                drained_terminating_queue = None;
            }
            if !tool_batch_terminated {
                pending_source = PendingQueue::Steering;
                pending_messages = get_messages(&config.get_steering_messages).await;
            }
            if has_more_tool_calls || !pending_messages.is_empty() {
                explicit_continuation = false;
            }
        }

        // Agent would stop here. Check for follow-up messages.
        let follow_up_messages = get_messages(&config.get_follow_up_messages).await;
        if !follow_up_messages.is_empty() {
            // Set as pending so inner loop processes them
            explicit_continuation = false;
            pending_source = PendingQueue::FollowUp;
            pending_messages = follow_up_messages;
            continue;
        }

        if explicit_continuation {
            explicit_continuation = false;
            continue;
        }

        // No more messages, exit
        break;
    }

    send(
        &emit,
        AgentEvent::AgentEnd {
            messages: new_messages.clone(),
        },
    )
    .await;
}

fn apply_loop_update(
    config: &mut AgentLoopConfig,
    model: Option<maho_ai::model::Model>,
    thinking_level: Option<maho_ai::types::ModelThinkingLevel>,
    thinking_selection: Option<Option<maho_ai::types::ThinkingSelection>>,
    abort_server_side_fallback: Option<bool>,
) {
    if let Some(model) = model {
        config.model = model;
    }
    if let Some(level) = thinking_level {
        config.options.reasoning = model_thinking_level_to_reasoning(level);
    }
    if let Some(selection) = thinking_selection {
        config.options.thinking_selection = selection;
    }
    if let Some(fallback) = abort_server_side_fallback {
        config.options.stream.request.abort_server_side_fallback = Some(fallback);
    }
}

pub(crate) async fn emit_run_failure(
    config: &AgentLoopConfig,
    error: String,
    local: bool,
    emit: &AgentEventSink,
) {
    let mut failure_message = create_terminal_failure_assistant_message(
        &config.model,
        StopReason::Error,
        &AgentStreamError::Other { message: error },
        None,
    );
    if local {
        failure_message.failure_source = Some(maho_ai::types::AssistantFailureSource::Local);
    }
    let failure = assistant_event(&failure_message);
    send(
        emit,
        AgentEvent::MessageStart {
            message: failure.clone(),
        },
    )
    .await;
    send(
        emit,
        AgentEvent::MessageEnd {
            message: failure.clone(),
        },
    )
    .await;
    send(
        emit,
        AgentEvent::TurnEnd {
            message: failure.clone(),
            tool_results: Vec::new(),
        },
    )
    .await;
    send(
        emit,
        AgentEvent::AgentEnd {
            messages: vec![failure],
        },
    )
    .await;
}

/// Provider declarations retain freeform schemas but never executable/output-only tool fields.
fn to_provider_tool_declaration(tool: &Tool) -> Tool {
    Tool {
        output_schema: None,
        ..tool.clone()
    }
}

fn provider_tools(
    context: &AgentContext,
    model: Option<&maho_ai::types::Model>,
) -> (Option<Vec<Tool>>, Option<Vec<String>>) {
    let active = context.tools.as_deref().unwrap_or_default();
    let Some(declared) = context.declared_tools.as_ref().filter(|_| {
        model.is_some_and(maho_ai::openai_responses_compat::supports_allowed_tool_choice)
    }) else {
        return (
            context.tools.as_ref().map(|tools| {
                tools
                    .iter()
                    .map(|tool| to_provider_tool_declaration(&tool.tool))
                    .collect()
            }),
            None,
        );
    };
    let mut tools: Vec<Tool> = declared
        .iter()
        .map(|tool| to_provider_tool_declaration(&tool.tool))
        .collect();
    for tool in active {
        if !declared
            .iter()
            .any(|declared| declared.tool.name == tool.tool.name)
        {
            tools.push(to_provider_tool_declaration(&tool.tool));
        }
    }
    (
        Some(tools),
        Some(active.iter().map(|tool| tool.tool.name.clone()).collect()),
    )
}

/// Replace the last pending system message's tool intent with the actual provider loadout delta.
fn declare_tool_changes(
    context: &AgentContext,
    mut pending_messages: Vec<AgentMessage>,
    model: Option<&maho_ai::types::Model>,
) -> Vec<AgentMessage> {
    let system_index = pending_messages
        .iter()
        .rposition(|message| matches!(message, AgentMessage::Llm(Message::System(_))));
    let (declared, _) = provider_tools(context, model);
    let declared = declared.unwrap_or_default();
    let mut committed: Vec<Message> =
        create_initial_system_message(Some(&context.system_prompt), Some(&declared))
            .map(Message::System)
            .into_iter()
            .chain(
                context
                    .messages
                    .iter()
                    .filter_map(AgentMessage::try_as_llm)
                    .cloned(),
            )
            .collect();
    for (index, message) in pending_messages.iter().enumerate() {
        if let Some(message) = message.try_as_llm() {
            let mut message = message.clone();
            if Some(index) == system_index
                && let Message::System(system) = &mut message
            {
                system.tools_added.clear();
                system.tools_removed.clear();
            }
            committed.push(message);
        }
    }
    let mut changes = get_tool_state_changes(&get_current_tools(&committed), &declared);
    // The shared comparator drops freeform; announce the complete provider declaration instead.
    for added in &mut changes.tools_added {
        if let Some(tool) = declared.iter().find(|tool| tool.name == added.name) {
            *added = tool.clone();
        }
    }
    if let Some(index) = system_index {
        if let AgentMessage::Llm(Message::System(system)) = &mut pending_messages[index] {
            system.tools_added = changes.tools_added;
            system.tools_removed = changes.tools_removed;
        }
        return pending_messages;
    }
    if changes.tools_added.is_empty() && changes.tools_removed.is_empty() {
        return pending_messages;
    }
    let index = pending_messages
        .iter()
        .position(|message| message.role() != "system")
        .unwrap_or(pending_messages.len());
    pending_messages.insert(
        index,
        Message::System(SystemMessage {
            content: UserContent::Text(String::new()),
            sections: Default::default(),
            tools_added: changes.tools_added,
            tools_removed: changes.tools_removed,
            timestamp: now_ms(),
        })
        .into(),
    );
    pending_messages
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
    let (tools, active_tool_names) = provider_tools(context, Some(&config.model));
    let normalized = normalize_context(&Context {
        system_prompt: Some(context.system_prompt.clone()),
        messages: (config.convert_to_llm)(messages).await,
        tools,
        active_tool_names,
    });
    // Keep the existing native stream/caller contract while carrying only normalized messages.
    Context {
        messages: normalized.messages,
        active_tool_names: normalized.active_tool_names,
        ..Context::default()
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
    let mut added_partial = false;
    // Tool results delivered by a provider that executes tools mid-stream
    // (Cursor's exec channel). Buffered here and appended by the caller right
    // after the assistant message so pairs stay adjacent in the transcript.
    let provider_tool_results: Arc<Mutex<Vec<ToolResultMessage>>> =
        Arc::new(Mutex::new(Vec::new()));
    let mut thinking_timing: BTreeMap<usize, ThinkingTiming> = BTreeMap::new();
    let mut response_timing = create_response_timing_recorder();

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
        let pre_stream =
            PreStreamBound::new(config.stream_start_timeout_ms, stream_idle_timeout_ms);
        let request_signal = request_abort_controller.signal();
        let llm_context = pre_stream
            .run(
                build_provider_context(context, config, Some(&request_signal)),
                &request_abort_controller,
            )
            .await?;

        // Resolve API key (important for expiring tokens)
        let resolved_api_key = match config.get_api_key.as_ref() {
            Some(get_api_key) => pre_stream
                .run(
                    get_api_key(config.model.provider.clone()),
                    &request_abort_controller,
                )
                .await?
                .map_err(stream_error)?,
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

        response_timing.requested();
        let response = pre_stream
            .run(
                async { stream_function(&config.model, &llm_context, Some(request_options)) },
                &request_abort_controller,
            )
            .await?;
        let consumed = async {
            let result = || async {
                let mut message = response.result().await.map_err(stream_error)?;
                message.thinking_level = Some(
                    config
                        .options
                        .reasoning
                        .map(Into::into)
                        .unwrap_or(maho_ai::types::ModelThinkingLevel::Off),
                );
                Ok::<_, AgentStreamError>(message)
            };

            let idle_error: Arc<dyn Fn(AgentStreamError) + Send + Sync> = {
                let controller = request_abort_controller.clone();
                Arc::new(move |error: AgentStreamError| {
                    controller.abort(Some(AbortReason::new(
                        "StreamIdleTimeoutError",
                        error.message(),
                    )));
                })
            };
            let mut event_reader = AssistantEventReader::new(
                response.clone(),
                stream_idle_timeout_ms,
                request_abort_controller.signal(),
                Some(idle_error),
                pre_stream.remaining_start_ms(),
                signal.clone(),
            );
            event_reader.configured_start_timeout_ms =
                normalize_timeout_ms(config.stream_start_timeout_ms);

            let mut finalized: Option<AssistantMessage> = None;
            loop {
                let next = match event_reader.next().await {
                    Ok(next) => next,
                    Err(error) => return Err(error),
                };
                let Some(event) = next else { break };
                let terminal = match &event {
                    AssistantMessageEvent::Done { .. } => Some(TerminalAssistantMessageEvent::Done),
                    AssistantMessageEvent::Error { reason, .. } => {
                        Some(TerminalAssistantMessageEvent::Error {
                            reason: match reason {
                                ErrorReason::Aborted => StopReason::Aborted,
                                ErrorReason::Error => StopReason::Error,
                            },
                        })
                    }
                    _ => None,
                };
                if let Some(terminal) = terminal {
                    let final_message =
                        normalize_terminal_assistant_message(result().await?, terminal);
                    finalized = Some(final_message);
                    break;
                }
                match &event {
                    AssistantMessageEvent::Start { partial } => {
                        response_timing.started();
                        context.messages.push(assistant_event(partial));
                        added_partial = true;
                        send(
                            emit,
                            AgentEvent::MessageStart {
                                message: assistant_event(partial),
                            },
                        )
                        .await;
                    }
                    AssistantMessageEvent::ThinkingStart {
                        content_index,
                        partial,
                    } => {
                        if added_partial {
                            let timing =
                                thinking_timing
                                    .entry(*content_index)
                                    .or_insert(ThinkingTiming {
                                        started_at: now_ms(),
                                        ended_at: None,
                                    });
                            let (started_at, ended_at) = (timing.started_at, timing.ended_at);
                            let mut updated = partial.clone();
                            set_thinking_block_timing(
                                &mut updated,
                                *content_index,
                                started_at,
                                ended_at,
                            );
                            emit_message_update(emit, context, updated, event).await;
                        }
                    }
                    AssistantMessageEvent::ThinkingDelta {
                        content_index,
                        partial,
                        ..
                    } => {
                        if added_partial {
                            response_timing.observe(&event);
                            let mut updated = partial.clone();
                            if let Some(timing) = thinking_timing.get(content_index) {
                                set_thinking_block_timing(
                                    &mut updated,
                                    *content_index,
                                    timing.started_at,
                                    timing.ended_at,
                                );
                            }
                            emit_message_update(emit, context, updated, event).await;
                        }
                    }
                    AssistantMessageEvent::ThinkingEnd {
                        content_index,
                        partial,
                        ..
                    } => {
                        if added_partial {
                            let mut updated = partial.clone();
                            if let Some(timing) = thinking_timing.get_mut(content_index) {
                                timing.ended_at = Some(now_ms());
                                let (started_at, ended_at) = (timing.started_at, timing.ended_at);
                                set_thinking_block_timing(
                                    &mut updated,
                                    *content_index,
                                    started_at,
                                    ended_at,
                                );
                            }
                            emit_message_update(emit, context, updated, event).await;
                        }
                    }
                    AssistantMessageEvent::TextStart { partial, .. }
                    | AssistantMessageEvent::TextDelta { partial, .. }
                    | AssistantMessageEvent::TextEnd { partial, .. }
                    | AssistantMessageEvent::ToolcallStart { partial, .. }
                    | AssistantMessageEvent::ToolcallDelta { partial, .. }
                    | AssistantMessageEvent::ToolcallEnd { partial, .. } => {
                        if added_partial {
                            response_timing.observe(&event);
                            emit_message_update(emit, context, partial.clone(), event).await;
                        }
                    }
                    AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => {}
                }
            }

            let final_message = match finalized {
                Some(final_message) => final_message,
                None => result().await?,
            };
            let mut final_message = final_message;
            propagate_thinking_timing(&mut final_message, &thinking_timing, &response_timing);
            if added_partial {
                if let Some(last) = context.messages.last_mut() {
                    *last = assistant_event(&final_message);
                }
            } else {
                context.messages.push(assistant_event(&final_message));
                send(
                    emit,
                    AgentEvent::MessageStart {
                        message: assistant_event(&final_message),
                    },
                )
                .await;
            }
            send(
                emit,
                AgentEvent::MessageEnd {
                    message: assistant_event(&final_message),
                },
            )
            .await;
            Ok(StreamedAssistantResponse {
                message: final_message,
                provider_tool_results: provider_tool_results
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone(),
            })
        }
        .await;
        let cancelled = response.cancel().await.map_err(stream_error);
        match consumed {
            Err(error) => Err(error),
            Ok(result) => {
                cancelled?;
                Ok(result)
            }
        }
    }
    .await;

    let result = match outcome {
        Ok(response) => Ok(response),
        Err(error) => {
            let buffered = provider_tool_results
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            // The streamed partial lives in the context slot added at `Start`.
            let partial_message = if added_partial {
                context.messages.last().and_then(AgentMessage::as_assistant)
            } else {
                None
            };
            if is_stream_idle_timeout_error(&error)
                && should_finalize_idle_as_stop(partial_message, &buffered, |call| {
                    is_cursor_exec_resolved(call, &buffered)
                })
            {
                let partial = partial_message.cloned();
                let mut final_message = AssistantMessage {
                    content: partial
                        .as_ref()
                        .map(|message| message.content.clone())
                        .unwrap_or_else(|| vec![ContentBlock::text("")]),
                    api: partial
                        .as_ref()
                        .map(|m| m.api.clone())
                        .unwrap_or_else(|| config.model.api.clone()),
                    provider: partial
                        .as_ref()
                        .map(|m| m.provider.clone())
                        .unwrap_or_else(|| config.model.provider.clone()),
                    model: partial
                        .as_ref()
                        .map(|m| m.model.clone())
                        .unwrap_or_else(|| config.model.id.clone()),
                    response_model: partial.as_ref().and_then(|m| m.response_model.clone()),
                    response_id: partial.as_ref().and_then(|m| m.response_id.clone()),
                    thinking_level: None,
                    provider_thinking_level: partial
                        .as_ref()
                        .and_then(|m| m.provider_thinking_level.clone()),
                    diagnostics: partial.as_ref().and_then(|m| m.diagnostics.clone()),
                    prompt_cache_breakpoint: None,
                    prompt_cache_diagnostics: partial
                        .as_ref()
                        .and_then(|m| m.prompt_cache_diagnostics.clone()),
                    usage: partial
                        .as_ref()
                        .map(|m| m.usage)
                        .unwrap_or_else(empty_usage),
                    stop_reason: StopReason::Stop,
                    stop_details: None,
                    deferred: None,
                    error_message: None,
                    error_cause: partial.as_ref().and_then(|message| message.error_cause),
                    provider_diagnostic: None,
                    abort_source: None,
                    failure_source: None,
                    raw_stop_reason: None,
                    end_turn: None,
                    timing: None,
                    timestamp: partial.as_ref().map(|m| m.timestamp).unwrap_or_else(now_ms),
                };
                propagate_thinking_timing(&mut final_message, &thinking_timing, &response_timing);
                if added_partial {
                    if let Some(last) = context.messages.last_mut() {
                        *last = assistant_event(&final_message);
                    }
                } else {
                    context.messages.push(assistant_event(&final_message));
                    send(
                        emit,
                        AgentEvent::MessageStart {
                            message: assistant_event(&final_message),
                        },
                    )
                    .await;
                }
                send(
                    emit,
                    AgentEvent::MessageEnd {
                        message: assistant_event(&final_message),
                    },
                )
                .await;
                Ok(StreamedAssistantResponse {
                    message: final_message,
                    provider_tool_results: buffered,
                })
            } else {
                let aborted = signal.as_ref().is_some_and(AbortSignal::aborted);
                let reason = if aborted {
                    StopReason::Aborted
                } else {
                    StopReason::Error
                };
                let error = if aborted
                    && matches!(
                        error,
                        AgentStreamError::StartTimeout { .. }
                            | AgentStreamError::IdleTimeout { .. }
                    ) {
                    abort_error(signal.as_ref())
                } else {
                    error
                };
                let mut final_message = create_terminal_failure_assistant_message(
                    &config.model,
                    reason,
                    &error,
                    partial_message,
                );
                propagate_thinking_timing(&mut final_message, &thinking_timing, &response_timing);
                if added_partial {
                    if let Some(last) = context.messages.last_mut() {
                        *last = assistant_event(&final_message);
                    }
                } else {
                    context.messages.push(assistant_event(&final_message));
                    send(
                        emit,
                        AgentEvent::MessageStart {
                            message: assistant_event(&final_message),
                        },
                    )
                    .await;
                }
                send(
                    emit,
                    AgentEvent::MessageEnd {
                        message: assistant_event(&final_message),
                    },
                )
                .await;
                Ok(StreamedAssistantResponse {
                    message: final_message,
                    provider_tool_results: buffered,
                })
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
            let mut final_message = create_terminal_failure_assistant_message(
                &config.model,
                StopReason::Error,
                &error,
                None,
            );
            propagate_thinking_timing(&mut final_message, &thinking_timing, &response_timing);
            context.messages.push(assistant_event(&final_message));
            StreamedAssistantResponse {
                message: final_message,
                provider_tool_results: Vec::new(),
            }
        }
    }
}

async fn emit_message_update(
    emit: &AgentEventSink,
    context: &mut AgentContext,
    updated: AssistantMessage,
    event: AssistantMessageEvent,
) {
    let message = AgentMessage::Llm(Message::Assistant(Box::new(updated)));
    if let Some(last) = context.messages.last_mut() {
        *last = message.clone();
    }
    send(
        emit,
        AgentEvent::MessageUpdate {
            message,
            assistant_message_event: event,
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

fn propagate_thinking_timing(
    message: &mut AssistantMessage,
    thinking_timing: &BTreeMap<usize, ThinkingTiming>,
    response_timing: &crate::response_timing::ResponseTimingRecorder,
) {
    response_timing.stamp(message);
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
    use maho_ai::utils::event_stream::StreamErrorKind;
    match error.kind {
        StreamErrorKind::Other => AgentStreamError::Other {
            message: error.message,
        },
        StreamErrorKind::ProviderFailure(diagnostic) => AgentStreamError::ProviderFailure {
            message: error.message,
            diagnostic,
        },
        StreamErrorKind::OAuthRefreshUnavailable => AgentStreamError::OAuthRefreshUnavailable {
            message: error.message,
        },
    }
}

/// `AbortError`: the reason the signal carries, else the DOM default. A
/// `ProviderRetryWatchdogAbortError` reason keeps its class so the terminal message gets
/// `abortSource: "provider"`.
fn abort_error(signal: Option<&AbortSignal>) -> AgentStreamError {
    match signal.and_then(AbortSignal::reason) {
        Some(reason) if reason.name == PROVIDER_RETRY_WATCHDOG_ABORT_ERROR_NAME => {
            AgentStreamError::ProviderRetryWatchdogAbort {
                provider_cause: reason.message,
            }
        }
        Some(reason) => AgentStreamError::Other {
            message: reason.message,
        },
        None => AgentStreamError::Other {
            message: "Request was aborted".to_owned(),
        },
    }
}

fn normalize_timeout_ms(timeout_ms: Option<u64>) -> Option<u64> {
    timeout_ms.filter(|timeout_ms| *timeout_ms > 0)
}

struct PreStreamBound {
    started_at: tokio::time::Instant,
    start_timeout_ms: Option<u64>,
    idle_timeout_ms: Option<u64>,
}

impl PreStreamBound {
    fn new(start_timeout_ms: Option<u64>, idle_timeout_ms: Option<u64>) -> Self {
        Self {
            started_at: tokio::time::Instant::now(),
            start_timeout_ms: normalize_timeout_ms(start_timeout_ms),
            idle_timeout_ms: normalize_timeout_ms(idle_timeout_ms),
        }
    }

    fn remaining_start_ms(&self) -> Option<u64> {
        self.start_timeout_ms.map(|ms| {
            let elapsed = u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
            ms.saturating_sub(elapsed).max(1)
        })
    }

    async fn run<T>(
        &self,
        work: impl std::future::Future<Output = T>,
        request: &AbortController,
    ) -> Result<T, AgentStreamError> {
        let deadline = match self.start_timeout_ms {
            Some(ms) => self.started_at + Duration::from_millis(ms),
            None => match self.idle_timeout_ms {
                Some(ms) => tokio::time::Instant::now() + Duration::from_millis(ms),
                None => return Ok(work.await),
            },
        };
        match tokio::time::timeout_at(deadline, work).await {
            Ok(value) => Ok(value),
            Err(_) => {
                let error = match self.start_timeout_ms {
                    Some(timeout_ms) => AgentStreamError::StartTimeout { timeout_ms },
                    None => AgentStreamError::IdleTimeout {
                        timeout_ms: self.idle_timeout_ms.unwrap_or(0),
                    },
                };
                request.abort(Some(AbortReason::new(
                    "PreStreamTimeoutError",
                    error.message(),
                )));
                Err(error)
            }
        }
    }
}

struct AssistantEventReader {
    stream: AssistantMessageEventStream,
    idle_timeout_ms: Option<u64>,
    start_timeout_ms: Option<u64>,
    configured_start_timeout_ms: Option<u64>,
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
            configured_start_timeout_ms: normalize_timeout_ms(stream_start_timeout_ms),
            saw_first_event: false,
            signal: Some(signal),
            on_idle_timeout,
        }
    }

    /// The start bound applies only until the provider proves the request is alive with its
    /// first event; afterwards the idle bound governs as before.
    fn make_timeout_error(&self, timeout_ms: u64) -> AgentStreamError {
        if !self.saw_first_event && self.start_timeout_ms.is_some() {
            AgentStreamError::StartTimeout {
                timeout_ms: self.configured_start_timeout_ms.unwrap_or(timeout_ms),
            }
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
        let read_timeout_ms = if use_start_bound {
            self.start_timeout_ms
        } else {
            self.idle_timeout_ms
        };
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
    async fn read_without_timeout(
        &mut self,
    ) -> Result<Option<AssistantMessageEvent>, AgentStreamError> {
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

fn create_incomplete_tool_call_error_message(
    tool_name: &str,
    error_message: Option<&str>,
) -> String {
    match error_message {
        Some(error_message) => {
            let separator = if error_message.ends_with('.') {
                ""
            } else {
                "."
            };
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
fn is_cursor_exec_resolved(
    tool_call: &ToolCall,
    provider_tool_results: &[ToolResultMessage],
) -> bool {
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
                parent_tool_call_id: None,
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                args: serde_json::Value::Object(tool_call.arguments.clone()),
            },
        )
        .await;
        let finalized = FinalizedToolCallOutcome {
            tool_call: tool_call.clone(),
            result: create_error_tool_result(&create_incomplete_tool_call_error_message(
                &tool_call.name,
                None,
            )),
            is_error: true,
        };
        emit_tool_execution_end(&finalized, emit).await;
        let tool_result_message = create_tool_result_message(&finalized);
        emit_tool_result_message(&tool_result_message, emit).await;
        messages.push(tool_result_message);
    }
    ExecutedToolCallBatch {
        messages,
        terminate: false,
    }
}

/// Execute tool calls from an assistant message.
async fn execute_tool_calls(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    provider_tool_results: &[ToolResultMessage],
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    // Same filter as the loop's collection site (defense in depth): a block
    // stamped `kCursorExecResolved` was already executed by Cursor's exec
    // channel and its result buffered; running it again would duplicate a
    // side-effecting tool.
    let tool_calls: Vec<AgentToolCall> = assistant_message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call)
                if !is_cursor_exec_resolved(call, provider_tool_results) =>
            {
                Some(call.clone())
            }
            _ => None,
        })
        .collect();
    // One snapshot per batch, shared by every spawned call and hook context.
    let current_context = &Arc::new(current_context.clone());
    if config.tool_execution_mode() == ToolExecutionMode::Sequential {
        return execute_tool_calls_sequential(
            current_context,
            assistant_message,
            &tool_calls,
            config,
            signal,
            emit,
        )
        .await;
    }
    execute_tool_calls_parallel(
        current_context,
        assistant_message,
        &tool_calls,
        config,
        signal,
        emit,
    )
    .await
}

async fn execute_tool_calls_sequential(
    current_context: &Arc<AgentContext>,
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
                parent_tool_call_id: None,
                tool_call_id: tool_call.id.clone(),
                tool_name: tool
                    .as_ref()
                    .map(|tool| tool.name().to_owned())
                    .unwrap_or_else(|| tool_call.name.clone()),
                args: serde_json::Value::Object(tool_call.arguments.clone()),
            },
        )
        .await;

        let preparation = prepare_tool_call(
            current_context,
            assistant_message,
            tool_call,
            tool.as_ref(),
            config,
            signal.clone(),
        )
        .await;
        let finalized = match &preparation {
            ToolCallPreparation::Immediate {
                tool_call,
                result,
                is_error,
            } => FinalizedToolCallOutcome {
                tool_call: tool_call.clone(),
                result: (**result).clone(),
                is_error: *is_error,
            },
            ToolCallPreparation::Prepared { .. } => {
                let executed = execute_prepared_tool_call(&preparation, signal.clone(), emit).await;
                finalize_executed_tool_call(
                    current_context,
                    assistant_message,
                    &preparation,
                    executed,
                    config,
                    signal.clone(),
                )
                .await
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
    ExecutedToolCallBatch {
        messages,
        terminate,
    }
}

/// A single finalized tool call, awaited by both its dependents and the collector.
///
/// TS hands out one promise per call and lets several `await`s share it; a `oneshot` receiver is
/// single-use and not `Clone`, so the port carries the outcome on a `watch` channel, which every
/// holder can clone and await.
type FinalizedReceiver = tokio::sync::watch::Receiver<Option<FinalizedToolCallOutcome>>;

fn finalized_channel() -> (
    tokio::sync::watch::Sender<Option<FinalizedToolCallOutcome>>,
    FinalizedReceiver,
) {
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
    current_context: &Arc<AgentContext>,
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
                parent_tool_call_id: None,
                tool_call_id: tool_call.id.clone(),
                tool_name: tool
                    .as_ref()
                    .map(|tool| tool.name().to_owned())
                    .unwrap_or_else(|| tool_call.name.clone()),
                args: serde_json::Value::Object(tool_call.arguments.clone()),
            },
        )
        .await;

        let preparation = prepare_tool_call(
            current_context,
            assistant_message,
            tool_call,
            tool.as_ref(),
            config,
            signal.clone(),
        )
        .await;
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
        let context = Arc::clone(current_context);
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
                run_prepared_tool_call(
                    &context,
                    &assistant,
                    &preparation,
                    &config,
                    signal.clone(),
                    &emit,
                )
                .await
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
    ExecutedToolCallBatch {
        messages,
        terminate,
    }
}

async fn run_prepared_tool_call(
    current_context: &Arc<AgentContext>,
    assistant_message: &AssistantMessage,
    preparation: &ToolCallPreparation,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
) -> FinalizedToolCallOutcome {
    if let ToolCallPreparation::Immediate {
        tool_call,
        result,
        is_error,
    } = preparation
    {
        return FinalizedToolCallOutcome {
            tool_call: tool_call.clone(),
            result: (**result).clone(),
            is_error: *is_error,
        };
    }

    let executed = execute_prepared_tool_call(preparation, signal.clone(), emit).await;
    finalize_executed_tool_call(
        current_context,
        assistant_message,
        preparation,
        executed,
        config,
        signal,
    )
    .await
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
        tool: Box<AgentTool>,
        args: serde_json::Value,
        requested_name: Option<String>,
    },
    Immediate {
        tool_call: AgentToolCall,
        result: Box<AgentToolResult>,
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

pub use crate::types::AgentToolCallOutcome;

type FinalizedToolCallOutcome = AgentToolCallOutcome;

pub struct RunToolCallOptions {
    pub assistant_message: AssistantMessage,
    pub context: Arc<AgentContext>,
    pub tools: Vec<AgentTool>,
    pub before_tool_call: Option<crate::types::BeforeToolCall>,
    pub after_tool_call: Option<crate::types::AfterToolCall>,
    pub signal: Option<AbortSignal>,
    pub on_update: Option<AgentToolUpdateCallback>,
}

/// Executes a nested call through the model-call pipeline without lifecycle events or transcript mutation.
pub async fn run_tool_call(
    tool_call: AgentToolCall,
    options: RunToolCallOptions,
) -> AgentToolCallOutcome {
    let mut config = AgentLoopConfig::new(
        crate::agent::default_model(),
        crate::types::identity_convert_to_llm(),
    );
    config.before_tool_call = options.before_tool_call;
    config.after_tool_call = options.after_tool_call;
    let tool = options
        .tools
        .iter()
        .find(|tool| tool.name() == tool_call.name);
    let preparation = prepare_tool_call(
        &options.context,
        &options.assistant_message,
        &tool_call,
        tool,
        &config,
        options.signal.clone(),
    )
    .await;
    if let ToolCallPreparation::Immediate {
        result, is_error, ..
    } = &preparation
    {
        return AgentToolCallOutcome {
            tool_call,
            result: (**result).clone(),
            is_error: *is_error,
        };
    }
    let emit: AgentEventSink = Arc::new(|_| Box::pin(async {}));
    let executed = execute_prepared_tool_call_with_update(
        &preparation,
        options.signal.clone(),
        &emit,
        options.on_update,
    )
    .await;
    finalize_executed_tool_call(
        &options.context,
        &options.assistant_message,
        &preparation,
        executed,
        &config,
        options.signal,
    )
    .await
}

fn should_terminate_tool_batch(finalized_calls: &[FinalizedToolCallOutcome]) -> bool {
    !finalized_calls.is_empty()
        && finalized_calls
            .iter()
            .all(|finalized| finalized.result.terminate == Some(true))
}

/// `PreparedAgentToolCall`.
pub struct PreparedAgentToolCall {
    pub tool_call: AgentToolCall,
    pub tool: AgentTool,
    pub args: serde_json::Value,
}

pub fn prepare_agent_tool_call(
    tool: &AgentTool,
    tool_call: &AgentToolCall,
) -> Result<PreparedAgentToolCall, String> {
    let prepared_tool_call = prepare_agent_tool_call_arguments(tool, tool_call)?;
    let args = validate_tool_arguments(&tool.tool, &prepared_tool_call)
        .map_err(|error| error.to_string())?;
    Ok(PreparedAgentToolCall {
        tool_call: prepared_tool_call,
        tool: tool.clone(),
        args,
    })
}

async fn prepare_tool_call(
    current_context: &Arc<AgentContext>,
    assistant_message: &AssistantMessage,
    tool_call: &AgentToolCall,
    tool: Option<&AgentTool>,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
) -> ToolCallPreparation {
    if tool_call.incomplete == Some(true) {
        return ToolCallPreparation::Immediate {
            tool_call: tool_call.clone(),
            result: Box::new(create_error_tool_result(
                &create_incomplete_tool_call_error_message(
                    &tool_call.name,
                    tool_call.error_message.as_deref(),
                ),
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
            result: Box::new(create_error_tool_result(&message)),
            is_error: true,
        };
    };
    if tool.name() != tool_call.name {
        let requested_name = tool_call.name.clone();
        let mut renamed = tool_call.clone();
        renamed.name = tool.name().to_owned();
        let outcome = prepare_resolved_tool_call(
            current_context,
            assistant_message,
            &renamed,
            tool,
            config,
            signal,
        )
        .await;
        return match outcome {
            ToolCallPreparation::Prepared {
                tool_call,
                tool,
                args,
                ..
            } => ToolCallPreparation::Prepared {
                tool_call,
                tool,
                args,
                requested_name: Some(requested_name),
            },
            ToolCallPreparation::Immediate {
                tool_call,
                result,
                is_error,
            } => ToolCallPreparation::Immediate {
                tool_call,
                result: Box::new(with_tool_name_correction(
                    *result,
                    &requested_name,
                    tool.name(),
                )),
                is_error,
            },
        };
    }
    prepare_resolved_tool_call(
        current_context,
        assistant_message,
        tool_call,
        tool,
        config,
        signal,
    )
    .await
}

async fn prepare_resolved_tool_call(
    current_context: &Arc<AgentContext>,
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
                result: Box::new(create_error_tool_result(&message)),
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
                context: Arc::clone(current_context),
            },
            signal.clone(),
        )
        .await;
        if signal.as_ref().is_some_and(AbortSignal::aborted) {
            return ToolCallPreparation::Immediate {
                tool_call: tool_call.clone(),
                result: Box::new(create_error_tool_result("Operation aborted")),
                is_error: true,
            };
        }
        match before_result {
            Some(before_result) if before_result.block == Some(true) => {
                let mut result = create_error_tool_result(
                    before_result
                        .reason
                        .as_deref()
                        .unwrap_or("Tool execution was blocked"),
                );
                if before_result.terminate == Some(true) {
                    result.terminate = Some(true);
                }
                return ToolCallPreparation::Immediate {
                    tool_call: tool_call.clone(),
                    result: Box::new(result),
                    is_error: true,
                };
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
            result: Box::new(create_error_tool_result("Operation aborted")),
            is_error: true,
        };
    }
    ToolCallPreparation::Prepared {
        tool_call: prepared_tool_call.tool_call,
        tool: Box::new(tool.clone()),
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
    execute_prepared_tool_call_with_update(prepared, signal, emit, None).await
}

async fn execute_prepared_tool_call_with_update(
    prepared: &ToolCallPreparation,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
    nested_update: Option<AgentToolUpdateCallback>,
) -> ExecutedToolCallOutcome {
    let ToolCallPreparation::Prepared {
        tool_call,
        tool,
        args,
        ..
    } = prepared
    else {
        return ExecutedToolCallOutcome {
            result: create_error_tool_result("Tool call was not prepared"),
            is_error: true,
        };
    };
    // Updates are delivered in order as they arrive by one drainer task; dropping the
    // sender (taken out of the shared slot at settle/abort) stops accepting updates and
    // lets the drainer finish, so every accepted update lands before the outcome.
    let (update_tx, mut update_rx) =
        tokio::sync::mpsc::unbounded_channel::<maho_ai::types::BoxFuture<'static, ()>>();
    let update_sender = Arc::new(Mutex::new(Some(update_tx)));
    let update_drainer = tokio::spawn(async move {
        while let Some(update) = update_rx.recv().await {
            update.await;
        }
    });

    let on_update: AgentToolUpdateCallback = {
        let emit = emit.clone();
        let update_sender = update_sender.clone();
        let tool_call_id = tool_call.id.clone();
        let tool_name = tool_call.name.clone();
        let args = serde_json::Value::Object(tool_call.arguments.clone());
        Arc::new(move |partial_result: AgentToolResult| {
            let sender = update_sender
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(sender) = sender.as_ref() else {
                return;
            };
            if let Some(update) = &nested_update {
                let update = update.clone();
                let _ = sender.send(Box::pin(async move {
                    update(partial_result);
                }));
                return;
            }
            let event = AgentEvent::ToolExecutionUpdate {
                parent_tool_call_id: None,
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                args: args.clone(),
                partial_result: serde_json::to_value(&partial_result)
                    .unwrap_or(serde_json::Value::Null),
            };
            let _ = sender.send(emit(event));
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

    update_sender
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    let _ = update_drainer.await;
    match settled {
        None => ExecutedToolCallOutcome {
            result: create_error_tool_result("Tool execution aborted"),
            is_error: true,
        },
        Some(Ok(result)) => ExecutedToolCallOutcome {
            is_error: result.is_error == Some(true),
            result,
        },
        Some(Err(_)) => ExecutedToolCallOutcome {
            result: create_error_tool_result("Tool execution was cancelled"),
            is_error: true,
        },
    }
}

async fn finalize_executed_tool_call(
    current_context: &Arc<AgentContext>,
    assistant_message: &AssistantMessage,
    prepared: &ToolCallPreparation,
    executed: ExecutedToolCallOutcome,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
) -> FinalizedToolCallOutcome {
    let ToolCallPreparation::Prepared {
        tool_call,
        requested_name,
        args,
        ..
    } = prepared
    else {
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
                context: Arc::clone(current_context),
            },
            signal.clone(),
        )
        .await;
        if let Some(after_result) = after_result {
            // Structured content not replaced along with the content may no longer match it.
            let structured_content =
                after_result
                    .structured_content
                    .or(if after_result.content.is_some() {
                        None
                    } else {
                        result.structured_content
                    });
            result = AgentToolResult {
                content: after_result.content.unwrap_or(result.content),
                details: after_result.details.unwrap_or(result.details),
                structured_content,
                usage: after_result.usage.or(result.usage),
                nested_calls: result.nested_calls,
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

    FinalizedToolCallOutcome {
        tool_call: tool_call.clone(),
        result,
        is_error,
    }
}

fn create_error_tool_result(message: &str) -> AgentToolResult {
    AgentToolResult {
        content: vec![ContentBlock::text(message)],
        details: serde_json::Value::Object(serde_json::Map::new()),
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: None,
    }
}

async fn emit_tool_execution_end(finalized: &FinalizedToolCallOutcome, emit: &AgentEventSink) {
    send(
        emit,
        AgentEvent::ToolExecutionEnd {
            parent_tool_call_id: None,
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
        details: Some(
            serde_json::to_value(&finalized.result.details).unwrap_or(serde_json::Value::Null),
        ),
        usage: finalized.result.usage,
        nested_calls: finalized.result.nested_calls.clone(),
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
    send(
        emit,
        AgentEvent::MessageStart {
            message: message.clone(),
        },
    )
    .await;
    send(emit, AgentEvent::MessageEnd { message }).await;
}

#[cfg(test)]
mod provider_context_tests {
    use super::*;

    #[test]
    fn typed_stream_failures_project_without_losing_classification() {
        let diagnostic = StreamError::diagnostic_from_status(Some(401)).expect("diagnostic");
        assert!(
            matches!(stream_error(StreamError::provider_failure("denied", diagnostic.clone())), AgentStreamError::ProviderFailure { diagnostic: projected, .. } if projected == diagnostic)
        );
        assert!(matches!(
            stream_error(StreamError::oauth_refresh_unavailable("refresh")),
            AgentStreamError::OAuthRefreshUnavailable { .. }
        ));
    }

    #[test]
    fn native_allowed_choice_keeps_declarations_separate_from_execution() {
        let mut context = context();
        let mut declared = context.tools.clone().expect("active tools");
        let mut inactive = declared[0].clone();
        inactive.tool.name = "inactive".into();
        declared.push(inactive);
        context.declared_tools = Some(declared);
        let mut model = crate::agent::default_model();
        model.base_url = "https://api.openai.com/v1".into();
        model.compat = Some(
            serde_json::from_value(serde_json::json!({"supportsAllowedTools":true}))
                .expect("compat"),
        );
        let (tools, active) = provider_tools(&context, Some(&model));
        assert_eq!(tools.expect("declarations").len(), 2);
        assert_eq!(active, Some(vec!["current".into()]));
        assert_eq!(context.tools.as_ref().expect("execution tools").len(), 1);
        model.base_url = "https://gateway.invalid/v1".into();
        let (tools, active) = provider_tools(&context, Some(&model));
        assert_eq!(tools.expect("active only").len(), 1);
        assert!(active.is_none());
    }

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.into(),
            description: String::new(),
            parameters: serde_json::json!({"type": "object"}),
            output_schema: Some(serde_json::json!({"type": "string"})),
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn context() -> AgentContext {
        AgentContext {
            tools: Some(vec![AgentTool {
                tool: tool("current"),
                label: String::new(),
                prepare_arguments: None,
                execute: Arc::new(|_, _, _, _| Box::pin(async { AgentToolResult::text("") })),
                replay: None,
                execution_mode: None,
            }]),
            ..AgentContext::default()
        }
    }

    fn system(added: Vec<Tool>, removed: &[&str]) -> AgentMessage {
        Message::System(SystemMessage {
            content: UserContent::Text(String::new()),
            sections: Default::default(),
            tools_added: added,
            tools_removed: removed
                .iter()
                .map(|name| maho_ai::types::ToolReference {
                    name: (*name).into(),
                })
                .collect(),
            timestamp: 17,
        })
        .into()
    }

    #[test]
    fn shorthand_loadout_does_not_gain_duplicate_declarations() {
        // Given: tools supplied only through the shorthand context.
        let context = context();
        // When: a request has no pending system message.
        let pending = declare_tool_changes(&context, Vec::new(), None);
        // Then: normalization's initial declaration needs no extra transcript delta.
        assert!(pending.is_empty());
    }

    #[test]
    fn pending_system_intent_is_replaced_without_losing_sections() {
        // Given: committed history removed a current tool and added an obsolete tool.
        let mut context = context();
        context.messages = vec![system(vec![tool("obsolete")], &["current"])];
        let mut pending = system(vec![tool("unrequested")], &["obsolete"]);
        if let AgentMessage::Llm(Message::System(message)) = &mut pending {
            message
                .sections
                .insert("policy".into(), Some("value".into()));
        }
        // When: the caller's pending declaration is reconciled with the actual tools.
        let pending = declare_tool_changes(&context, vec![pending], None);
        // Then: intent is replaced by the replay delta, while non-tool fields survive.
        let AgentMessage::Llm(Message::System(message)) = &pending[0] else {
            panic!("system message preserved");
        };
        assert_eq!(
            message.tools_added,
            vec![to_provider_tool_declaration(&tool("current"))]
        );
        assert_eq!(
            message.tools_removed,
            vec![maho_ai::types::ToolReference {
                name: "obsolete".into()
            }]
        );
        assert_eq!(message.sections.get("policy"), Some(&Some("value".into())));
        assert_eq!(message.timestamp, 17);
    }

    #[tokio::test]
    async fn provider_context_normalizes_after_transform_and_conversion() {
        // Given: transform and conversion each add an ordered system update.
        let context = context();
        let mut config = AgentLoopConfig::new(
            crate::agent::default_model(),
            Arc::new(|mut messages| {
                Box::pin(async move {
                    messages.push(system(vec![tool("converted")], &["transformed"]));
                    messages.into_iter().map(AgentMessage::into_llm).collect()
                })
            }),
        );
        config.transform_context = Some(Arc::new(|mut messages, _| {
            Box::pin(async move {
                messages.push(system(vec![tool("transformed")], &["current"]));
                messages
            })
        }));
        // When: the public builder prepares a provider request.
        let built = tokio::time::timeout(
            Duration::from_secs(1),
            build_provider_context(&context, &config, None),
        )
        .await
        .expect("bounded context preparation");
        // Then: shorthand leads the converted transcript and replay sees the final tool set.
        assert!(built.system_prompt.is_none());
        assert!(built.tools.is_none());
        assert_eq!(built.messages.len(), 3);
        assert_eq!(get_current_tools(&built.messages), vec![tool("converted")]);
        let Message::System(initial) = &built.messages[0] else {
            panic!("normalized shorthand leads");
        };
        assert!(initial.tools_added[0].output_schema.is_none());
    }

    #[tokio::test]
    async fn inserted_declaration_has_lifecycle_before_the_prompt() {
        // Given: history disagrees with the next request's callable tool loadout.
        let mut context = context();
        context.messages = vec![system(vec![tool("obsolete")], &["current"])];
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let emit: AgentEventSink = Arc::new(move |event| {
            observed.lock().expect("events").push(event);
            Box::pin(async {})
        });
        let config = AgentLoopConfig::new(
            crate::agent::default_model(),
            crate::types::identity_convert_to_llm(),
        );
        let stream: StreamFn = Arc::new(|_, context, _| {
            assert_eq!(
                get_current_tools(&context.messages),
                vec![to_provider_tool_declaration(&tool("current"))]
            );
            let stream = AssistantMessageEventStream::assistant();
            let mut response = create_terminal_failure_assistant_message(
                &crate::agent::default_model(),
                StopReason::Stop,
                &AgentStreamError::Other {
                    message: String::new(),
                },
                None,
            );
            response.error_message = None;
            response.content = vec![ContentBlock::text("answer")];
            stream.end(Some(response));
            stream
        });
        let prompt = Message::User(maho_ai::types::UserMessage {
            content: UserContent::Text("input".into()),
            timestamp: 0,
        })
        .into();
        // When: a new prompt starts the loop.
        let messages = tokio::time::timeout(
            Duration::from_secs(1),
            run_agent_loop(vec![prompt], context, config, emit, None, Some(stream)),
        )
        .await
        .expect("bounded agent loop");
        // Then: the inserted declaration is persisted and announced before the user prompt.
        assert_eq!(messages[0].role(), "system");
        assert_eq!(messages[1].role(), "user");
        let lifecycle: Vec<_> = events
            .lock()
            .expect("events")
            .iter()
            .filter_map(|event| match event {
                AgentEvent::MessageStart { message } => Some(("start", message.role())),
                AgentEvent::MessageEnd { message } => Some(("end", message.role())),
                _ => None,
            })
            .collect();
        assert_eq!(
            &lifecycle[..4],
            &[
                ("start", "system"),
                ("end", "system"),
                ("start", "user"),
                ("end", "user")
            ]
        );
    }
}

#[cfg(test)]
mod audit_gap_tests {
    use super::*;
    use crate::types::{AgentLoopTurnUpdate, AgentRequestUpdate, AgentTurnDecision};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn terminal_response_awaits_provider_resource_release() {
        let closed = Arc::new(AtomicUsize::new(0));
        let released = closed.clone();
        let stream: StreamFn = Arc::new(move |_, _, _| {
            let stream = AssistantMessageEventStream::assistant();
            let released = released.clone();
            stream.set_cancellation_handler(Box::new(move || {
                Box::pin(async move {
                    released.store(1, Ordering::SeqCst);
                    Ok(())
                })
            }));
            stream.push(AssistantMessageEvent::Done {
                reason: maho_ai::types::DoneReason::Stop,
                message: response(StopReason::Stop),
            });
            stream
        });
        let config = AgentLoopConfig::new(
            crate::agent::default_model(),
            crate::types::identity_convert_to_llm(),
        );
        tokio::time::timeout(
            Duration::from_secs(1),
            run_agent_loop(
                vec![user()],
                AgentContext::default(),
                config,
                sink(Arc::new(Mutex::new(Vec::new()))),
                None,
                Some(stream),
            ),
        )
        .await
        .expect("loop acknowledges closure");
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn oauth_key_failure_terminates_before_provider_dispatch() {
        let mut config = AgentLoopConfig::new(
            crate::agent::default_model(),
            crate::types::identity_convert_to_llm(),
        );
        config.get_api_key = Some(Arc::new(|_| {
            Box::pin(async {
                Err(StreamError::oauth_refresh_unavailable(
                    "refresh unavailable",
                ))
            })
        }));
        let stream: StreamFn =
            Arc::new(|_, _, _| panic!("provider must not run after key failure"));
        let emit: AgentEventSink = Arc::new(|_| Box::pin(async {}));
        let messages = tokio::time::timeout(
            Duration::from_secs(1),
            run_agent_loop(
                vec![user()],
                AgentContext::default(),
                config,
                emit,
                None,
                Some(stream),
            ),
        )
        .await
        .expect("bounded agent loop");
        let message = messages
            .last()
            .and_then(AgentMessage::as_assistant)
            .expect("terminal assistant");
        assert_eq!(message.stop_reason, StopReason::Error);
        assert_eq!(
            message.error_message.as_deref(),
            Some("refresh unavailable")
        );
        assert!(
            message
                .diagnostics
                .as_ref()
                .expect("diagnostics")
                .iter()
                .any(|diagnostic| diagnostic.kind
                    == maho_ai::utils::retry::OAUTH_REFRESH_UNAVAILABLE_DIAGNOSTIC)
        );
    }

    #[tokio::test]
    async fn settled_responses_record_requested_effort_on_both_result_paths() {
        // Given: an injected stream reporting a different effort than the agent requested.
        for terminal_event in [true, false] {
            for level in [None, Some(maho_ai::types::ThinkingLevel::High)] {
                let mut config = AgentLoopConfig::new(
                    crate::agent::default_model(),
                    crate::types::identity_convert_to_llm(),
                );
                config.options.reasoning = level;
                let stream: StreamFn = Arc::new(move |_, _, _| {
                    let stream = AssistantMessageEventStream::assistant();
                    let mut message = response(StopReason::Stop);
                    message.thinking_level = Some(maho_ai::types::ModelThinkingLevel::Low);
                    message.provider_thinking_level = Some("native-effort".into());
                    if terminal_event {
                        stream.push(AssistantMessageEvent::Done {
                            reason: maho_ai::types::DoneReason::Stop,
                            message,
                        });
                    } else {
                        stream.end(Some(message));
                    }
                    stream
                });
                // When: the real agent loop settles the injected response.
                let messages = tokio::time::timeout(
                    Duration::from_secs(1),
                    run_agent_loop(
                        vec![user()],
                        AgentContext::default(),
                        config,
                        sink(Arc::new(Mutex::new(Vec::new()))),
                        None,
                        Some(stream),
                    ),
                )
                .await
                .expect("bounded response");
                // Then: serialization exposes requested effort separately from native effort.
                let message = messages
                    .iter()
                    .find_map(AgentMessage::as_assistant)
                    .expect("assistant response");
                let wire = serde_json::to_value(message).expect("message wire");
                assert_eq!(
                    wire["thinkingLevel"],
                    if level.is_some() { "high" } else { "off" }
                );
                assert_eq!(wire["providerThinkingLevel"], "native-effort");
            }
        }
    }

    fn user() -> AgentMessage {
        Message::User(maho_ai::types::UserMessage {
            content: maho_ai::types::UserContent::Text("input".into()),
            timestamp: 0,
        })
        .into()
    }

    fn response(reason: StopReason) -> AssistantMessage {
        let mut message = create_terminal_failure_assistant_message(
            &crate::agent::default_model(),
            reason,
            &AgentStreamError::Other {
                message: "fixture".into(),
            },
            None,
        );
        message.stop_reason = reason;
        message.error_message = None;
        message.content = vec![ContentBlock::text("answer")];
        message
    }

    fn stream(calls: Arc<AtomicUsize>) -> StreamFn {
        Arc::new(move |_, _, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            let stream = AssistantMessageEventStream::assistant();
            stream.end(Some(response(StopReason::Stop)));
            stream
        })
    }

    fn sink(events: Arc<Mutex<Vec<AgentEvent>>>) -> AgentEventSink {
        Arc::new(move |event| {
            events.lock().expect("events").push(event);
            Box::pin(async {})
        })
    }

    #[tokio::test]
    async fn continuation_prepares_first_and_next_requests_before_turn_end() {
        // Given: one explicit continuation and a prepared message for that request.
        let calls = Arc::new(AtomicUsize::new(0));
        let turns = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut config = AgentLoopConfig::new(
            crate::agent::default_model(),
            crate::types::identity_convert_to_llm(),
        );
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = requests.clone();
        config.prepare_request = Some(Arc::new(move |request, _| {
            let index = seen.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                request.context.messages.len(),
                if index == 0 { 1 } else { 3 }
            );
            Box::pin(async { Some(AgentRequestUpdate::default()) })
        }));
        let finished = turns.clone();
        let observed = events.clone();
        config.finish_turn = Some(Arc::new(move |_, _| {
            let index = finished.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                observed
                    .lock()
                    .expect("events")
                    .iter()
                    .filter(|event| matches!(event, AgentEvent::TurnEnd { .. }))
                    .count(),
                index
            );
            Box::pin(async move {
                Some(if index == 0 {
                    AgentTurnDecision::Continue
                } else {
                    AgentTurnDecision::End
                })
            })
        }));
        config.prepare_next_turn = Some(Arc::new(|_| {
            Box::pin(async {
                Some(AgentLoopTurnUpdate {
                    messages: Some(vec![user()]),
                    ..Default::default()
                })
            })
        }));
        // When: the first stop requests one additional provider call.
        let messages = tokio::time::timeout(
            Duration::from_secs(1),
            run_agent_loop(
                vec![user()],
                AgentContext::default(),
                config,
                sink(events.clone()),
                None,
                Some(stream(calls.clone())),
            ),
        )
        .await
        .expect("bounded loop");
        // Then: preparation covered both requests and the added message had a lifecycle.
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(messages.len(), 4);
        assert_eq!(events.lock().expect("events").iter().filter(|event| matches!(event, AgentEvent::MessageEnd { message } if message.role() == "user")).count(), 2);
    }

    #[tokio::test]
    async fn failed_queue_commit_restores_without_admission() {
        // Given: a claim whose durable commit fails.
        let calls = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let restored = Arc::new(AtomicUsize::new(0));
        let mut config = AgentLoopConfig::new(
            crate::agent::default_model(),
            crate::types::identity_convert_to_llm(),
        );
        config.get_steering_messages = Some(Arc::new(|| Box::pin(async { vec![user()] })));
        config.commit_queued_consumption = Some(Arc::new(|queue, messages| {
            assert_eq!(queue, PendingQueue::Steering);
            assert_eq!(messages.len(), 1);
            Box::pin(async { Err("commit failed".into()) })
        }));
        let returned = restored.clone();
        config.restore_pending_messages = Some(Arc::new(move |_, messages| {
            returned.fetch_add(messages.len(), Ordering::SeqCst);
            Box::pin(async {})
        }));
        // When: admission is attempted.
        tokio::time::timeout(
            Duration::from_secs(1),
            run_agent_loop(
                Vec::new(),
                AgentContext::default(),
                config,
                sink(events.clone()),
                None,
                Some(stream(calls.clone())),
            ),
        )
        .await
        .expect("bounded loop");
        // Then: neither provider nor transcript saw the claimed input.
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(restored.load(Ordering::SeqCst), 1);
        assert!(!events.lock().expect("events").iter().any(|event| matches!(event, AgentEvent::MessageStart { message } if message.role() == "user")));
    }

    #[tokio::test(start_paused = true)]
    async fn start_bound_includes_stalled_context_transform() {
        // Given: context preparation that never resolves.
        let calls = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut config = AgentLoopConfig::new(
            crate::agent::default_model(),
            crate::types::identity_convert_to_llm(),
        );
        config.stream_start_timeout_ms = Some(25);
        config.transform_context = Some(Arc::new(|_, _| Box::pin(std::future::pending())));
        // When: the virtual start deadline expires.
        let messages = run_agent_loop(
            vec![user()],
            AgentContext::default(),
            config,
            sink(events),
            None,
            Some(stream(calls.clone())),
        )
        .await;
        // Then: preparation failed without starting the provider.
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            messages
                .last()
                .and_then(AgentMessage::as_assistant)
                .map(|message| message.stop_reason),
            Some(StopReason::Error)
        );
    }

    #[tokio::test]
    async fn nested_call_uses_validation_and_both_hooks_without_messages() {
        // Given: a valid nested tool with a shim and both hooks.
        let executed = Arc::new(AtomicUsize::new(0));
        let execution_count = executed.clone();
        let tool = AgentTool {
            label: "nested".into(),
            prepare_arguments: Some(Arc::new(|_| Ok(serde_json::json!({"value": 7})))),
            execute: Arc::new(move |_, args, _, _| {
                execution_count.fetch_add(1, Ordering::SeqCst);
                assert_eq!(args["value"], 7);
                Box::pin(async { AgentToolResult::text("raw") })
            }),
            replay: None,
            execution_mode: None,
            tool: Tool {
                name: "nested".into(),
                description: String::new(),
                parameters: serde_json::json!({"type":"object","properties":{"value":{"type":"integer"}},"required":["value"]}),
                output_schema: None,
                freeform: None,
                constrained_sampling: None,
            },
        };
        let context = Arc::new(AgentContext::default());
        let options = RunToolCallOptions {
            assistant_message: response(StopReason::ToolUse),
            context: context.clone(),
            tools: vec![tool],
            before_tool_call: Some(Arc::new(|context, _| {
                assert_eq!(context.args["value"], 7);
                Box::pin(async { None })
            })),
            after_tool_call: Some(Arc::new(|_, _| {
                Box::pin(async {
                    Some(crate::types::AfterToolCallResult {
                        is_error: Some(true),
                        ..Default::default()
                    })
                })
            })),
            signal: None,
            on_update: None,
        };
        // When: a tool invokes another tool through the reusable pipeline.
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            run_tool_call(
                ToolCall {
                    id: "call".into(),
                    name: "nested".into(),
                    arguments: Default::default(),
                    incomplete: None,
                    error_message: None,
                    thought_signature: None,
                    namespace: None,
                },
                options,
            ),
        )
        .await
        .expect("bounded nested call");
        // Then: the final hook result is returned without transcript mutation.
        assert_eq!(executed.load(Ordering::SeqCst), 1);
        assert!(outcome.is_error);
        assert!(context.messages.is_empty());
        assert!(outcome.result.nested_calls.is_none());
    }

    #[test]
    fn tool_messages_copy_only_producer_supplied_nested_records() {
        let nested_calls = Some(maho_ai::types::NestedToolCalls {
            calls: Vec::new(),
            complete: false,
        });
        let mut result = AgentToolResult::text("done");
        result.nested_calls = nested_calls.clone();
        let finalized = FinalizedToolCallOutcome {
            tool_call: ToolCall {
                id: "root".into(),
                name: "read".into(),
                arguments: Default::default(),
                incomplete: None,
                error_message: None,
                thought_signature: None,
                namespace: None,
            },
            result,
            is_error: false,
        };
        assert_eq!(
            create_tool_result_message(&finalized).nested_calls,
            nested_calls
        );
    }
}
