//! Port of senpi packages/agent/src/empty-assistant-recovery.ts.

use std::sync::Arc;

use maho_ai::model::Model;
use maho_ai::tool_call_middleware::{
    get_tool_call_format, has_kimi_text_tool_call_recovery, should_recover_text_tool_calls,
};
use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, ErrorReason,
};
use maho_ai::utils::diagnostics::AssistantMessageDiagnostic;
use maho_ai::utils::empty_response_errors::{
    EMPTY_RESPONSE_ERROR, EMPTY_TOOL_USE_ERROR, FORWARDED_EMPTY_RESPONSE_ERROR,
    FORWARDED_EMPTY_TOOL_USE_ERROR,
};
use maho_ai::utils::event_stream::StreamError;
use maho_ai::utils::visible_text::{has_visible_assistant_content, has_visible_text};

use crate::assistant_terminal_state::now_ms;
use crate::types::StreamFn;

fn is_empty_stop(message: &AssistantMessage) -> bool {
    message.stop_reason == maho_ai::types::StopReason::Stop
        && !has_visible_assistant_content(message)
}

fn is_empty_tool_use(message: &AssistantMessage) -> bool {
    message.stop_reason == maho_ai::types::StopReason::ToolUse
        && !message
            .content
            .iter()
            .any(|block| matches!(block, maho_ai::types::ContentBlock::ToolCall(_)))
}

/// Whether reasoning commits the attempt. True for models whose thinking channel is native
/// (Claude, antml/other text formats): a reasoning model shows its work for seconds before the
/// first text or tool call, and holding that back blanks the transcript for the whole phase.
/// False for the Kimi XTML lane: its thinking channel is the documented misrouting vector for
/// text tool calls and \`recoverKimiXtmlThinking\` only rewrites the finished message, so a
/// leaked protocol fragment forwarded live could never be retracted (#759).
#[derive(Debug, Clone, Copy)]
struct CommitPolicy {
    thinking_commits: bool,
}

/// Zero-width or whitespace-only deltas never commit, so format-only noise stays buffered.
fn is_meaningful_content_event(event: &AssistantMessageEvent, policy: CommitPolicy) -> bool {
    match event {
        AssistantMessageEvent::ToolcallStart { .. } => true,
        AssistantMessageEvent::TextDelta { delta, .. } => has_visible_text(delta),
        AssistantMessageEvent::TextEnd { content, .. } => has_visible_text(content),
        AssistantMessageEvent::ThinkingDelta { delta, .. } => {
            policy.thinking_commits && has_visible_text(delta)
        }
        AssistantMessageEvent::ThinkingEnd { content, .. } => {
            policy.thinking_commits && has_visible_text(content)
        }
        _ => false,
    }
}

fn append_retry_diagnostic(
    message: AssistantMessage,
    kind: &str,
    details: serde_json::Map<String, serde_json::Value>,
) -> AssistantMessage {
    let mut diagnostics = message.diagnostics.clone().unwrap_or_default();
    diagnostics.push(AssistantMessageDiagnostic {
        kind: kind.to_owned(),
        timestamp: now_ms(),
        error: None,
        details: Some(details.into_iter().collect()),
    });
    AssistantMessage {
        diagnostics: Some(diagnostics),
        ..message
    }
}

fn retry_details(retries: u64, forwarded: bool) -> serde_json::Map<String, serde_json::Value> {
    let mut details = serde_json::Map::new();
    details.insert("retries".to_owned(), serde_json::Value::from(retries));
    if forwarded {
        details.insert("forwarded".to_owned(), serde_json::Value::Bool(true));
    }
    details
}

fn recovery_diagnostic_type(tool_use: bool) -> &'static str {
    if tool_use {
        "empty_tool_use_response_recovery"
    } else {
        "empty_assistant_response_recovery"
    }
}

fn create_empty_response_failure(message: AssistantMessage, tool_use: bool) -> AssistantMessage {
    let error_message = if tool_use {
        EMPTY_TOOL_USE_ERROR
    } else {
        EMPTY_RESPONSE_ERROR
    };
    let with_diagnostic = append_retry_diagnostic(
        message,
        recovery_diagnostic_type(tool_use),
        retry_details(1, false),
    );
    AssistantMessage {
        content: vec![maho_ai::types::ContentBlock::text(error_message)],
        stop_reason: maho_ai::types::StopReason::Error,
        error_message: Some(error_message.to_owned()),
        ..with_diagnostic
    }
}

/// An attempt whose reasoning already streamed cannot be replayed here: a second \`start\` would
/// duplicate the partial message, and stitching this attempt's thinking onto a retry's content
/// would break provider replay of signed thinking blocks. The turn ends as a retryable error
/// (see the classifier in pi-ai's retry.ts) and keeps the content the user already saw, so the
/// session's turn retry re-requests it with the failed attempt dropped from the provider context.
fn create_forwarded_empty_failure(message: AssistantMessage, tool_use: bool) -> AssistantMessage {
    let error_message = if tool_use {
        FORWARDED_EMPTY_TOOL_USE_ERROR
    } else {
        FORWARDED_EMPTY_RESPONSE_ERROR
    };
    let with_diagnostic = append_retry_diagnostic(
        message,
        recovery_diagnostic_type(tool_use),
        retry_details(0, true),
    );
    AssistantMessage {
        stop_reason: maho_ai::types::StopReason::Error,
        error_message: Some(error_message.to_owned()),
        ..with_diagnostic
    }
}

async fn run_retrying_stream(
    outer_stream: AssistantMessageEventStream,
    first_stream: AssistantMessageEventStream,
    create_stream: Arc<dyn Fn() -> AssistantMessageEventStream + Send + Sync>,
    policy: CommitPolicy,
    active: Arc<std::sync::Mutex<AssistantMessageEventStream>>,
    signal: maho_ai::utils::abort::AbortSignal,
) {
    let mut stream = first_stream;
    let mut retrying = false;
    loop {
        let mut buffered: Vec<AssistantMessageEvent> = Vec::new();
        let mut forwarding = false;
        let mut retry = false;
        loop {
            let event = match stream.next().await {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    outer_stream.fail(error);
                    return;
                }
            };
            match &event {
                AssistantMessageEvent::Done {
                    message, reason, ..
                } => {
                    let done_reason = *reason;
                    let empty_tool_use = is_empty_tool_use(message);
                    if is_empty_stop(message) || empty_tool_use {
                        if forwarding {
                            outer_stream.push(AssistantMessageEvent::Error {
                                reason: ErrorReason::Error,
                                error: create_forwarded_empty_failure(
                                    message.clone(),
                                    empty_tool_use,
                                ),
                            });
                            outer_stream.end(None);
                            return;
                        }
                        if !retrying {
                            retry = true;
                            break;
                        }
                        let error = create_empty_response_failure(message.clone(), empty_tool_use);
                        outer_stream.push(AssistantMessageEvent::Error {
                            reason: ErrorReason::Error,
                            error,
                        });
                        outer_stream.end(None);
                        return;
                    }
                    let terminal = if retrying {
                        AssistantMessageEvent::Done {
                            reason: done_reason,
                            message: append_retry_diagnostic(
                                message.clone(),
                                recovery_diagnostic_type(is_empty_tool_use(message)),
                                retry_details(1, false),
                            ),
                        }
                    } else {
                        event.clone()
                    };
                    if !forwarding {
                        for pending in buffered.drain(..) {
                            outer_stream.push(pending);
                        }
                    }
                    outer_stream.push(terminal);
                    outer_stream.end(None);
                    return;
                }
                AssistantMessageEvent::Error { .. } => {
                    if !forwarding {
                        for pending in buffered.drain(..) {
                            outer_stream.push(pending);
                        }
                    }
                    outer_stream.push(event.clone());
                    outer_stream.end(None);
                    return;
                }
                _ => {}
            }
            if forwarding {
                outer_stream.push(event);
                continue;
            }
            buffered.push(event.clone());
            if is_meaningful_content_event(&event, policy) {
                for pending in buffered.drain(..) {
                    outer_stream.push(pending);
                }
                forwarding = true;
            }
        }
        if !retry {
            match stream.result().await {
                Ok(mut result) => {
                    if retrying {
                        result = append_retry_diagnostic(
                            result.clone(),
                            recovery_diagnostic_type(is_empty_tool_use(&result)),
                            retry_details(1, false),
                        );
                    }
                    outer_stream.end(Some(result));
                }
                Err(error) => outer_stream.fail(StreamError::new(error.to_string())),
            }
            return;
        }
        if let Err(error) = stream.cancel().await {
            outer_stream.fail(error);
            return;
        }
        if signal.aborted() { return; }
        retrying = true;
        stream = create_stream();
        *active.lock().unwrap_or_else(|error| error.into_inner()) = stream.clone();
    }
}

// Wrapping replaces the provider stream with a buffering proxy, which does not carry the
// underlying stream's liveness surface (trackLocalWork/hasPendingLocalWork) that the loop's
// idle watchdog reads. Only wrap models that actually need stream-level recovery; the
// empty-tool_use contradiction is normalized for every model by the agent loop instead.
pub fn with_empty_assistant_recovery(model: &Model, stream_function: StreamFn) -> StreamFn {
    if !should_recover_text_tool_calls(model) && get_tool_call_format(model).is_none() {
        return stream_function;
    }
    let policy = CommitPolicy {
        thinking_commits: !has_kimi_text_tool_call_recovery(model),
    };
    Arc::new(move |requested_model, context, options| {
        let stream_function = stream_function.clone();
        let create_stream = {
            let stream_function = stream_function.clone();
            let requested_model = requested_model.clone();
            let context = context.clone();
            let options = options.clone();
            Arc::new(move || stream_function(&requested_model, &context, options.clone()))
        };
        let first_stream = create_stream();
        let outer_stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
        let controller = maho_ai::utils::abort::AbortController::new();
        let signal = controller.signal();
        outer_stream.set_cancellation_handler(Box::new(move || Box::pin(async move {
            controller.abort(None);
            Ok(())
        })));
        let active = Arc::new(std::sync::Mutex::new(first_stream.clone()));
        let task_outer = outer_stream.clone();
        let create_stream_for_task = create_stream.clone();
        let task = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = signal.cancelled() => {},
                _ = run_retrying_stream(task_outer.clone(), first_stream, create_stream_for_task, policy, active.clone(), signal.clone()) => {},
            }
            let stream = active.lock().unwrap_or_else(|error| error.into_inner()).clone();
            if let Err(error) = stream.cancel().await {
                task_outer.fail(error);
            } else if let Some(result) = stream.result_if_ready() {
                match result {
                    Ok(message) => task_outer.end(Some(message)),
                    Err(error) => task_outer.fail(error),
                }
            }
        });
        outer_stream.set_cancellation_task(task);
        outer_stream
    })
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};
    use std::time::Duration;

    #[tokio::test]
    async fn cancelling_during_attempt_cleanup_awaits_exit() {
        let mut model = crate::agent::default_model();
        model.recover_text_tool_calls = Some(true);
        let first = AssistantMessageEventStream::assistant();
        let message = crate::assistant_terminal_state::create_terminal_failure_assistant_message(
            &model, maho_ai::types::StopReason::Stop,
            &crate::assistant_terminal_state::AgentStreamError::OAuthRefreshUnavailable { message: String::new() }, None,
        );
        let mut message = message;
        message.content.clear();
        first.push(AssistantMessageEvent::Done { reason: maho_ai::types::DoneReason::Stop, message });
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let closed = Arc::new(AtomicUsize::new(0));
        let task_closed = closed.clone();
        first.set_cancellation_handler(Box::new(move || Box::pin(async move {
            entered_tx.send(()).expect("cleanup entered");
            release_rx.await.expect("release cleanup");
            task_closed.store(1, Ordering::SeqCst);
            Ok(())
        })));
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let source: StreamFn = Arc::new(move |_, _, _| {
            count.fetch_add(1, Ordering::SeqCst);
            first.clone()
        });
        let context = maho_ai::types::Context { system_prompt: None, messages: Vec::new(), tools: None, active_tool_names: None };
        let first = source(&model, &context, None);
        let active = Arc::new(Mutex::new(first.clone()));
        let outer = AssistantMessageEventStream::assistant();
        let controller = maho_ai::utils::abort::AbortController::new();
        let signal = controller.signal();
        let create: Arc<dyn Fn() -> AssistantMessageEventStream + Send + Sync> = Arc::new(move || source(&model, &context, None));
        let task = tokio::spawn(run_retrying_stream(outer, first, create, CommitPolicy { thinking_commits: true }, active, signal));
        tokio::time::timeout(Duration::from_secs(1), entered_rx).await.expect("cleanup starts").expect("entered");
        controller.abort(None);
        release_tx.send(()).expect("release");
        tokio::time::timeout(Duration::from_secs(1), task).await.expect("acknowledged").expect("relay exits");
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancellation_after_retry_creation_closes_replacement() {
        let mut model = crate::agent::default_model();
        model.recover_text_tool_calls = Some(true);
        let first = AssistantMessageEventStream::assistant();
        let mut message = crate::assistant_terminal_state::create_terminal_failure_assistant_message(
            &model, maho_ai::types::StopReason::Stop,
            &crate::assistant_terminal_state::AgentStreamError::OAuthRefreshUnavailable { message: String::new() }, None,
        );
        message.content.clear();
        first.push(AssistantMessageEvent::Done { reason: maho_ai::types::DoneReason::Stop, message });
        let second = AssistantMessageEventStream::assistant();
        let closed = Arc::new(AtomicUsize::new(0));
        let task_closed = closed.clone();
        second.set_cancellation_handler(Box::new(move || Box::pin(async move {
            task_closed.store(1, Ordering::SeqCst);
            Ok(())
        })));
        let (created_tx, created_rx) = tokio::sync::oneshot::channel();
        let created_tx = Mutex::new(Some(created_tx));
        let calls = AtomicUsize::new(0);
        let source: StreamFn = Arc::new(move |_, _, _| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 { return first.clone(); }
            created_tx.lock().expect("sender").take().expect("one retry").send(()).expect("created");
            second.clone()
        });
        let context = maho_ai::types::Context { system_prompt: None, messages: Vec::new(), tools: None, active_tool_names: None };
        let outer = with_empty_assistant_recovery(&model, source)(&model, &context, None);
        tokio::time::timeout(Duration::from_secs(1), created_rx).await.expect("retry created").expect("created");
        tokio::time::timeout(Duration::from_secs(1), outer.cancel()).await.expect("acknowledged").expect("cancel");
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
