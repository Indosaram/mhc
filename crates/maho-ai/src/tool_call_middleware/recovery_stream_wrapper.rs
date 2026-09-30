//! Port of senpi packages/ai/src/tool-call-middleware/recovery-stream-wrapper.ts.
//! This Rust `EventStream` has no async-iterator `return()` hook for an outer consumer
//! to signal cancellation, so unlike the TS orchestrator this loop only ever terminates
//! from the inner stream's own terminal events (`done`/`error`) or exhaustion/failure;
//! `RecoveryAssistantMessageEventStream`'s cancellation plumbing has no caller here.

use std::sync::Arc;

use crate::tool_call_middleware::protocols::anthropic_xml::recovery_stream::RecoveryStreamParser;
use crate::tool_call_middleware::protocols::antml::recovery_stream::create_antml_invoke_recovery_stream_parser;
use crate::types::{AssistantMessage, AssistantMessageEvent, DoneReason, ErrorReason, Tool};
use crate::utils::event_stream::AssistantMessageEventStream;

use super::recovery_content_lifecycle::{RecoveryContentKind, RecoveryContentLifecycle};
use super::recovery_native_projection::{ProjectNativeStartResult, RecoveryNativeProjection};
use super::recovery_stream_failure::{terminate_recovery_stream_for_failure, RecoveryStreamFailure, TerminateRecoveryStreamOptions};
use super::recovery_stream_terminal::{AbortedOrError, DoneOrLengthOrToolUse, RecoveryStreamTerminal};
use super::recovery_text_projection::{RecoveryTextProjection, RecoveryTextProjectionOptions};
use super::stream_wrapper_shared::{StreamMessageProjection, StreamMessageProjectionOptions};
use super::types::ToolCallFormat;

type CreateParserFn = Arc<dyn Fn(Vec<Tool>) -> Box<dyn RecoveryStreamParser + Send> + Send + Sync>;

pub struct InvokeRecoveryOptions {
    pub create_parser: Option<CreateParserFn>,
    pub protocol: ToolCallFormat,
}

impl Default for InvokeRecoveryOptions {
    fn default() -> Self {
        Self { create_parser: None, protocol: ToolCallFormat::Antml }
    }
}

fn done_reason_to_stop_family(reason: DoneReason) -> Option<DoneOrLengthOrToolUse> {
    match reason {
        DoneReason::Stop => Some(DoneOrLengthOrToolUse::Stop),
        DoneReason::Length => Some(DoneOrLengthOrToolUse::Length),
        DoneReason::ToolUse => Some(DoneOrLengthOrToolUse::ToolUse),
        DoneReason::Deferred => None,
    }
}

fn error_reason_to_aborted_family(reason: ErrorReason) -> AbortedOrError {
    match reason {
        ErrorReason::Aborted => AbortedOrError::Aborted,
        ErrorReason::Error => AbortedOrError::Error,
    }
}

pub fn wrap_stream_with_invoke_recovery(
    inner_stream: AssistantMessageEventStream,
    tools: Vec<Tool>,
    options: InvokeRecoveryOptions,
) -> AssistantMessageEventStream {
    let outer_stream = AssistantMessageEventStream::assistant();
    let outer_for_task = outer_stream.clone();
    let protocol = options.protocol;
    let create_parser: CreateParserFn = options.create_parser.unwrap_or_else(|| Arc::new(|tools| create_antml_invoke_recovery_stream_parser(tools, None)));

    tokio::spawn(async move {
        let mut projection: Option<StreamMessageProjection> = None;
        let mut native_projection: Option<RecoveryNativeProjection> = None;
        let mut text_projection: Option<RecoveryTextProjection> = None;
        let mut saw_tool_call = false;
        let mut content_lifecycle = RecoveryContentLifecycle::new();
        let mut terminal = RecoveryStreamTerminal::new(outer_for_task.clone());

        macro_rules! finish_text {
            () => {{
                let mut finished_with_tool_call = false;
                if let Some(mut current_text) = text_projection.take() {
                    if let (Some(current_projection), Some(current_native)) = (&mut projection, &mut native_projection) {
                        finished_with_tool_call = current_text.finish(current_projection, current_native);
                    }
                }
                finished_with_tool_call
            }};
        }

        macro_rules! terminate_for_failure {
            ($source:expr, $failure:expr) => {{
                let _ = finish_text!();
                let Some(current_projection) = &mut projection else { return };
                terminate_recovery_stream_for_failure(&outer_for_task, current_projection, TerminateRecoveryStreamOptions { source: $source, failure: $failure, protocol });
                return;
            }};
        }

        macro_rules! prepare_content_event {
            ($source:expr, $content_index:expr) => {{
                if let (Some(current_projection), Some(current_native)) = (&mut projection, &mut native_projection) {
                    if $content_index >= $source.content.len() {
                        false
                    } else {
                        current_projection.sync($source.clone());
                        current_native.reserve_visible_ids($source);
                        current_native.synchronize_lower($source, $content_index)
                    }
                } else {
                    false
                }
            }};
        }

        loop {
            match inner_stream.next().await {
                Ok(Some(event)) => match event {
                    AssistantMessageEvent::Start { partial } => {
                        let new_projection = StreamMessageProjection::new(outer_for_task.clone(), partial.clone(), StreamMessageProjectionOptions { preserve_source_metadata: true });
                        let mut new_native = RecoveryNativeProjection::new(outer_for_task.clone(), new_projection.message.clone(), protocol);
                        new_native.reserve_visible_ids(&partial);
                        outer_for_task.push(AssistantMessageEvent::Start { partial: new_projection.message.clone() });
                        projection = Some(new_projection);
                        native_projection = Some(new_native);
                    }
                    AssistantMessageEvent::TextStart { content_index, partial } => {
                        if content_index >= partial.content.len() || !matches!(partial.content.get(content_index), Some(crate::types::ContentBlock::Text(_))) || !content_lifecycle.can_start(content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidContentEventOrder);
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let (Some(current_projection), Some(current_native)) = (&mut projection, &mut native_projection) else { return };
                        let next_text = RecoveryTextProjection::new(tools.clone(), content_index, RecoveryTextProjectionOptions { create_parser: Some(Arc::clone(&create_parser)), protocol });
                        if !next_text.start(current_projection, current_native, &partial) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidNativeEventOrder);
                        }
                        text_projection = Some(next_text);
                        content_lifecycle.start(content_index, RecoveryContentKind::Text);
                    }
                    AssistantMessageEvent::TextDelta { content_index, delta, partial } => {
                        if !content_lifecycle.is_active(content_index, RecoveryContentKind::Text) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidContentEventOrder);
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let (Some(current_projection), Some(current_native), Some(current_text)) = (&mut projection, &mut native_projection, &mut text_projection) else { continue };
                        saw_tool_call = current_text.feed(current_projection, current_native, &delta) || saw_tool_call;
                    }
                    AssistantMessageEvent::TextEnd { content_index, content: _, partial } => {
                        if !content_lifecycle.is_active(content_index, RecoveryContentKind::Text) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidContentEventOrder);
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        saw_tool_call = finish_text!() || saw_tool_call;
                        content_lifecycle.end(content_index, RecoveryContentKind::Text);
                    }
                    AssistantMessageEvent::ThinkingStart { content_index, partial } => {
                        if content_index >= partial.content.len() || !matches!(partial.content.get(content_index), Some(crate::types::ContentBlock::Thinking(_))) || !content_lifecycle.can_start(content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidContentEventOrder);
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let (Some(current_projection), Some(current_native)) = (&mut projection, &mut native_projection) else { return };
                        let outer_index = current_projection.start_thinking(content_index, &partial);
                        if !current_native.record_projected_block(content_index, outer_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidNativeEventOrder);
                        }
                        content_lifecycle.start(content_index, RecoveryContentKind::Thinking);
                    }
                    AssistantMessageEvent::ThinkingDelta { content_index, delta, partial } => {
                        if !content_lifecycle.is_active(content_index, RecoveryContentKind::Thinking) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidContentEventOrder);
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let Some(current_projection) = &mut projection else { continue };
                        current_projection.project_thinking_delta(content_index, &delta, &partial);
                    }
                    AssistantMessageEvent::ThinkingEnd { content_index, content, partial } => {
                        if !content_lifecycle.is_active(content_index, RecoveryContentKind::Thinking) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidContentEventOrder);
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let Some(current_projection) = &mut projection else { continue };
                        current_projection.finish_thinking(content_index, &content, &partial);
                        content_lifecycle.end(content_index, RecoveryContentKind::Thinking);
                    }
                    AssistantMessageEvent::ToolcallStart { content_index, partial } => {
                        if content_index >= partial.content.len() || !matches!(partial.content.get(content_index), Some(crate::types::ContentBlock::ToolCall(_))) || !content_lifecycle.can_start(content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidNativeEventOrder);
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let Some(current_native) = &mut native_projection else { return };
                        match current_native.project_native_start(&partial, content_index) {
                            ProjectNativeStartResult::Projected => {}
                            ProjectNativeStartResult::Collision => terminate_for_failure!(&partial, RecoveryStreamFailure::Collision),
                            ProjectNativeStartResult::Invalid => terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidNativeEventOrder),
                        }
                        content_lifecycle.start(content_index, RecoveryContentKind::ToolCall);
                        saw_tool_call = true;
                    }
                    AssistantMessageEvent::ToolcallDelta { content_index, delta, partial } => {
                        if !content_lifecycle.is_active(content_index, RecoveryContentKind::ToolCall) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidNativeEventOrder);
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let Some(current_native) = &mut native_projection else { continue };
                        if !current_native.project_native_delta(&partial, content_index, &delta) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidNativeEventOrder);
                        }
                    }
                    AssistantMessageEvent::ToolcallEnd { content_index, tool_call, partial } => {
                        if !content_lifecycle.is_active(content_index, RecoveryContentKind::ToolCall) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidNativeEventOrder);
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let Some(current_native) = &mut native_projection else { continue };
                        if !current_native.project_native_end(content_index, tool_call) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::InvalidNativeEventOrder);
                        }
                        content_lifecycle.end(content_index, RecoveryContentKind::ToolCall);
                        saw_tool_call = true;
                    }
                    AssistantMessageEvent::Done { reason, message } => {
                        if !synchronize_terminal(&mut projection, &mut native_projection, &outer_for_task, &mut text_projection, &mut saw_tool_call, &message, protocol) {
                            return;
                        }
                        let Some(current_projection) = &mut projection else { return };
                        match done_reason_to_stop_family(reason) {
                            Some(family) => terminal.done(current_projection, &message, saw_tool_call, family),
                            None => {
                                outer_for_task.push(AssistantMessageEvent::Done { reason, message: message.clone() });
                                outer_for_task.end(Some(message));
                            }
                        }
                        return;
                    }
                    AssistantMessageEvent::Error { reason, error } => {
                        if projection.is_none() {
                            outer_for_task.push(AssistantMessageEvent::Error { reason, error: error.clone() });
                            outer_for_task.end(Some(error));
                            return;
                        }
                        if !synchronize_terminal(&mut projection, &mut native_projection, &outer_for_task, &mut text_projection, &mut saw_tool_call, &error, protocol) {
                            return;
                        }
                        let Some(current_projection) = &mut projection else { return };
                        terminal.source_error(current_projection, &error, saw_tool_call, error_reason_to_aborted_family(reason));
                        return;
                    }
                },
                Ok(None) => {
                    let _ = finish_text!();
                    terminal.exhausted(projection.as_mut());
                    return;
                }
                Err(error) => {
                    let _ = finish_text!();
                    terminal.iterator_failure(projection.as_mut(), error);
                    return;
                }
            }
        }
    });

    outer_stream
}

#[allow(clippy::too_many_arguments)]
fn synchronize_terminal(
    projection: &mut Option<StreamMessageProjection>,
    native_projection: &mut Option<RecoveryNativeProjection>,
    outer_stream: &AssistantMessageEventStream,
    text_projection: &mut Option<RecoveryTextProjection>,
    saw_tool_call: &mut bool,
    source: &AssistantMessage,
    protocol: ToolCallFormat,
) -> bool {
    if projection.is_none() {
        *projection = Some(StreamMessageProjection::new(outer_stream.clone(), source.clone(), StreamMessageProjectionOptions { preserve_source_metadata: true }));
    }
    if native_projection.is_none() {
        let message = projection.as_ref().expect("projection just initialized").message.clone();
        *native_projection = Some(RecoveryNativeProjection::new(outer_stream.clone(), message, protocol));
    }
    let current_projection = projection.as_mut().expect("projection just initialized");
    let current_native = native_projection.as_mut().expect("native_projection just initialized");
    current_projection.sync(source.clone());
    current_native.reserve_visible_ids(source);
    if let Some(mut current_text) = text_projection.take() {
        *saw_tool_call = current_text.finish(current_projection, current_native) || *saw_tool_call;
    }
    if current_native.synchronize_remaining(source) {
        true
    } else {
        terminate_recovery_stream_for_failure(outer_stream, current_projection, TerminateRecoveryStreamOptions { source, failure: RecoveryStreamFailure::Collision, protocol });
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentBlock, StopReason, TextContent, Tool, Usage};
    use serde_json::json;

    fn tool(name: &str) -> Tool {
        Tool { name: name.into(), description: "d".into(), parameters: json!({"type": "object"}), freeform: None, constrained_sampling: None }
    }

    fn message(content: Vec<ContentBlock>, stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            content,
            api: "openai-completions".into(),
            provider: "openai".into(),
            model: "m".into(),
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

    #[tokio::test]
    async fn recovers_a_leaked_invoke_from_streamed_text_and_promotes_stop_reason() {
        let inner = AssistantMessageEventStream::assistant();
        let outer = wrap_stream_with_invoke_recovery(inner.clone(), vec![tool("get_weather")], InvokeRecoveryOptions::default());

        let start_source = message(vec![ContentBlock::Text(TextContent::default())], StopReason::Pending);
        inner.push(AssistantMessageEvent::Start { partial: start_source.clone() });
        inner.push(AssistantMessageEvent::TextStart { content_index: 0, partial: start_source.clone() });
        let invoke_text = r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#;
        let mut delta_source = start_source.clone();
        delta_source.content[0] = ContentBlock::Text(TextContent { text: invoke_text.into(), ..TextContent::default() });
        inner.push(AssistantMessageEvent::TextDelta { content_index: 0, delta: invoke_text.into(), partial: delta_source.clone() });
        inner.push(AssistantMessageEvent::TextEnd { content_index: 0, content: invoke_text.into(), partial: delta_source.clone() });
        let mut done_source = delta_source.clone();
        done_source.stop_reason = StopReason::Stop;
        inner.push(AssistantMessageEvent::Done { reason: DoneReason::Stop, message: done_source });
        inner.end(None);

        let mut done_event = None;
        loop {
            match outer.next().await {
                Ok(Some(event @ AssistantMessageEvent::Done { .. })) => {
                    done_event = Some(event);
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => break,
            }
        }
        let Some(AssistantMessageEvent::Done { reason, message }) = done_event else { panic!("expected a done event") };
        assert_eq!(reason, DoneReason::ToolUse);
        assert!(message.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(tool_call) if tool_call.name == "get_weather")));
    }

    #[tokio::test]
    async fn passes_through_plain_text_with_no_tool_call_recovered() {
        let inner = AssistantMessageEventStream::assistant();
        let outer = wrap_stream_with_invoke_recovery(inner.clone(), vec![tool("get_weather")], InvokeRecoveryOptions::default());

        let start_source = message(vec![ContentBlock::Text(TextContent::default())], StopReason::Pending);
        inner.push(AssistantMessageEvent::Start { partial: start_source.clone() });
        inner.push(AssistantMessageEvent::TextStart { content_index: 0, partial: start_source.clone() });
        let mut delta_source = start_source.clone();
        delta_source.content[0] = ContentBlock::Text(TextContent { text: "hello there".into(), ..TextContent::default() });
        inner.push(AssistantMessageEvent::TextDelta { content_index: 0, delta: "hello there".into(), partial: delta_source.clone() });
        inner.push(AssistantMessageEvent::TextEnd { content_index: 0, content: "hello there".into(), partial: delta_source.clone() });
        let mut done_source = delta_source.clone();
        done_source.stop_reason = StopReason::Stop;
        inner.push(AssistantMessageEvent::Done { reason: DoneReason::Stop, message: done_source });
        inner.end(None);

        let mut done_event = None;
        loop {
            match outer.next().await {
                Ok(Some(event @ AssistantMessageEvent::Done { .. })) => {
                    done_event = Some(event);
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => break,
            }
        }
        let Some(AssistantMessageEvent::Done { reason, message }) = done_event else { panic!("expected a done event") };
        assert_eq!(reason, DoneReason::Stop);
        assert!(!message.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_))));
    }
}
