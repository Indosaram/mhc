//! Port of senpi packages/ai/src/tool-call-middleware/recovery-stream-wrapper.ts.

use std::sync::Arc;

use crate::tool_call_middleware::protocols::anthropic_xml::recovery_stream::RecoveryStreamParser;
use crate::tool_call_middleware::protocols::antml::recovery_stream::create_antml_invoke_recovery_stream_parser;
use crate::types::{AssistantMessage, AssistantMessageEvent, DoneReason, ErrorReason, Tool};
use crate::utils::event_stream::AssistantMessageEventStream;

use super::recovery_content_lifecycle::{RecoveryContentKind, RecoveryContentLifecycle};
use super::recovery_native_projection::{ProjectNativeStartResult, RecoveryNativeProjection};
use super::recovery_stream_failure::{
    RecoveryStreamFailure, TerminateRecoveryStreamOptions, terminate_recovery_stream_for_failure,
};
use super::recovery_stream_terminal::{
    AbortedOrError, DoneOrLengthOrToolUse, RecoveryStreamTerminal,
};
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
        Self {
            create_parser: None,
            protocol: ToolCallFormat::Antml,
        }
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
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();
    let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel();
    let cancellation_source = inner_stream.clone();
    outer_stream.set_cancellation_handler(Box::new(move || {
        Box::pin(async move {
            if cancel_tx.send(()).is_err() {
                return cancellation_source.cancel().await;
            }
            match cancelled_rx.await {
                Ok(result) => result,
                Err(_) => cancellation_source.cancel().await,
            }
        })
    }));
    let protocol = options.protocol;
    let create_parser: CreateParserFn = options.create_parser.unwrap_or_else(|| {
        Arc::new(|tools| create_antml_invoke_recovery_stream_parser(tools, None))
    });

    let task = tokio::spawn(async move {
        let mut projection: Option<StreamMessageProjection> = None;
        let mut native_projection: Option<RecoveryNativeProjection> = None;
        let mut text_projection: Option<RecoveryTextProjection> = None;
        let mut saw_tool_call = false;
        let mut content_lifecycle = RecoveryContentLifecycle::new();
        let mut terminal = RecoveryStreamTerminal::new(outer_for_task.clone());

        macro_rules! finish_text {
            () => {{
                let mut finished_with_tool_call = false;
                if let Some(mut current_text) = text_projection.take()
                    && let (Some(current_projection), Some(current_native)) =
                        (&mut projection, &mut native_projection)
                {
                    finished_with_tool_call =
                        current_text.finish(current_projection, current_native);
                }
                finished_with_tool_call
            }};
        }

        macro_rules! terminate_for_failure {
            ($source:expr, $failure:expr) => {{
                if let Err(error) = inner_stream.cancel().await {
                    let _ = finish_text!();
                    terminal.iterator_failure(projection.as_mut(), error);
                    return;
                }
                let _ = finish_text!();
                let Some(current_projection) = &mut projection else {
                    return;
                };
                terminate_recovery_stream_for_failure(
                    &outer_for_task,
                    current_projection,
                    TerminateRecoveryStreamOptions {
                        source: $source,
                        failure: $failure,
                        protocol,
                    },
                );
                return;
            }};
        }

        macro_rules! prepare_content_event {
            ($source:expr, $content_index:expr) => {{
                if $content_index >= $source.content.len() {
                    terminate_for_failure!($source, RecoveryStreamFailure::InvalidNativeEventOrder);
                }
                if let (Some(current_projection), Some(current_native)) =
                    (&mut projection, &mut native_projection)
                {
                    if $content_index >= $source.content.len() {
                        false
                    } else {
                        current_projection.sync($source);
                        current_native.reserve_visible_ids($source);
                        current_native.synchronize_lower(
                            &mut current_projection.message,
                            $source,
                            $content_index,
                        )
                    }
                } else {
                    false
                }
            }};
        }

        loop {
            let next = tokio::select! {
                biased;
                request = &mut cancel_rx => {
                    if request.is_ok() {
                        let result = inner_stream.cancel().await;
                        let _ = finish_text!();
                        match &result {
                            Ok(()) => terminal.cancelled(projection.as_mut()),
                            Err(error) => terminal.iterator_failure(projection.as_mut(), error.clone()),
                        }
                        if cancelled_tx.send(result).is_err() {
                            return;
                        }
                        return;
                    }
                    return;
                }
                event = inner_stream.next() => event,
            };
            match next {
                Ok(Some(event)) => match event {
                    AssistantMessageEvent::Start { partial } => {
                        let new_projection = StreamMessageProjection::new(
                            outer_for_task.clone(),
                            partial.clone(),
                            StreamMessageProjectionOptions {
                                preserve_source_metadata: true,
                            },
                        );
                        let mut new_native =
                            RecoveryNativeProjection::new(outer_for_task.clone(), protocol);
                        new_native.reserve_visible_ids(&partial);
                        outer_for_task.push(AssistantMessageEvent::Start {
                            partial: new_projection.message.clone(),
                        });
                        projection = Some(new_projection);
                        native_projection = Some(new_native);
                    }
                    AssistantMessageEvent::TextStart {
                        content_index,
                        partial,
                    } => {
                        if content_index >= partial.content.len()
                            || !matches!(
                                partial.content.get(content_index),
                                Some(crate::types::ContentBlock::Text(_))
                            )
                            || !content_lifecycle.can_start(content_index)
                        {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidContentEventOrder
                            );
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let (Some(current_projection), Some(current_native)) =
                            (&mut projection, &mut native_projection)
                        else {
                            return;
                        };
                        let next_text = RecoveryTextProjection::new(
                            tools.clone(),
                            content_index,
                            RecoveryTextProjectionOptions {
                                create_parser: Some(Arc::clone(&create_parser)),
                                protocol,
                            },
                        );
                        if !next_text.start(current_projection, current_native, &partial) {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidNativeEventOrder
                            );
                        }
                        text_projection = Some(next_text);
                        content_lifecycle.start(content_index, RecoveryContentKind::Text);
                    }
                    AssistantMessageEvent::TextDelta {
                        content_index,
                        delta,
                        partial,
                    } => {
                        if !content_lifecycle.is_active(content_index, RecoveryContentKind::Text) {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidContentEventOrder
                            );
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let (Some(current_projection), Some(current_native), Some(current_text)) = (
                            &mut projection,
                            &mut native_projection,
                            &mut text_projection,
                        ) else {
                            continue;
                        };
                        saw_tool_call =
                            current_text.feed(current_projection, current_native, &delta)
                                || saw_tool_call;
                    }
                    AssistantMessageEvent::TextEnd {
                        content_index,
                        content: _,
                        partial,
                    } => {
                        if !content_lifecycle.is_active(content_index, RecoveryContentKind::Text) {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidContentEventOrder
                            );
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        saw_tool_call = finish_text!() || saw_tool_call;
                        content_lifecycle.end(content_index, RecoveryContentKind::Text);
                    }
                    AssistantMessageEvent::ThinkingStart {
                        content_index,
                        partial,
                    } => {
                        if content_index >= partial.content.len()
                            || !matches!(
                                partial.content.get(content_index),
                                Some(crate::types::ContentBlock::Thinking(_))
                            )
                            || !content_lifecycle.can_start(content_index)
                        {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidContentEventOrder
                            );
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let (Some(current_projection), Some(current_native)) =
                            (&mut projection, &mut native_projection)
                        else {
                            return;
                        };
                        let outer_index =
                            current_projection.start_thinking(content_index, &partial);
                        if !current_native.record_projected_block(content_index, outer_index) {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidNativeEventOrder
                            );
                        }
                        content_lifecycle.start(content_index, RecoveryContentKind::Thinking);
                    }
                    AssistantMessageEvent::ThinkingDelta {
                        content_index,
                        delta,
                        partial,
                    } => {
                        if !content_lifecycle
                            .is_active(content_index, RecoveryContentKind::Thinking)
                        {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidContentEventOrder
                            );
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let Some(current_projection) = &mut projection else {
                            continue;
                        };
                        current_projection.project_thinking_delta(content_index, &delta, &partial);
                    }
                    AssistantMessageEvent::ThinkingEnd {
                        content_index,
                        content,
                        partial,
                    } => {
                        if !content_lifecycle
                            .is_active(content_index, RecoveryContentKind::Thinking)
                        {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidContentEventOrder
                            );
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let Some(current_projection) = &mut projection else {
                            continue;
                        };
                        current_projection.finish_thinking(content_index, &content, &partial);
                        content_lifecycle.end(content_index, RecoveryContentKind::Thinking);
                    }
                    AssistantMessageEvent::ToolcallStart {
                        content_index,
                        partial,
                    } => {
                        if content_index >= partial.content.len()
                            || !matches!(
                                partial.content.get(content_index),
                                Some(crate::types::ContentBlock::ToolCall(_))
                            )
                            || !content_lifecycle.can_start(content_index)
                        {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidNativeEventOrder
                            );
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let (Some(current_projection), Some(current_native)) =
                            (&mut projection, &mut native_projection)
                        else {
                            return;
                        };
                        match current_native.project_native_start(
                            &mut current_projection.message,
                            &partial,
                            content_index,
                        ) {
                            ProjectNativeStartResult::Projected => {}
                            ProjectNativeStartResult::Collision => {
                                terminate_for_failure!(&partial, RecoveryStreamFailure::Collision)
                            }
                            ProjectNativeStartResult::Invalid => terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidNativeEventOrder
                            ),
                        }
                        content_lifecycle.start(content_index, RecoveryContentKind::ToolCall);
                        saw_tool_call = true;
                    }
                    AssistantMessageEvent::ToolcallDelta {
                        content_index,
                        delta,
                        partial,
                    } => {
                        if !content_lifecycle
                            .is_active(content_index, RecoveryContentKind::ToolCall)
                        {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidNativeEventOrder
                            );
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let (Some(current_projection), Some(current_native)) =
                            (&mut projection, &mut native_projection)
                        else {
                            continue;
                        };
                        if !current_native.project_native_delta(
                            &mut current_projection.message,
                            &partial,
                            content_index,
                            &delta,
                        ) {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidNativeEventOrder
                            );
                        }
                    }
                    AssistantMessageEvent::ToolcallEnd {
                        content_index,
                        tool_call,
                        partial,
                    } => {
                        if !content_lifecycle
                            .is_active(content_index, RecoveryContentKind::ToolCall)
                        {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidNativeEventOrder
                            );
                        }
                        if !prepare_content_event!(&partial, content_index) {
                            terminate_for_failure!(&partial, RecoveryStreamFailure::Collision);
                        }
                        let (Some(current_projection), Some(current_native)) =
                            (&mut projection, &mut native_projection)
                        else {
                            continue;
                        };
                        if !current_native.project_native_end(
                            &mut current_projection.message,
                            content_index,
                            tool_call,
                        ) {
                            terminate_for_failure!(
                                &partial,
                                RecoveryStreamFailure::InvalidNativeEventOrder
                            );
                        }
                        content_lifecycle.end(content_index, RecoveryContentKind::ToolCall);
                        saw_tool_call = true;
                    }
                    AssistantMessageEvent::Done { reason, message } => {
                        if !synchronize_terminal(
                            &mut projection,
                            &mut native_projection,
                            &outer_for_task,
                            &mut text_projection,
                            &mut saw_tool_call,
                            &message,
                            protocol,
                        ) {
                            return;
                        }
                        let Some(current_projection) = &mut projection else {
                            return;
                        };
                        match done_reason_to_stop_family(reason) {
                            Some(family) => {
                                terminal.done(current_projection, &message, saw_tool_call, family)
                            }
                            None => {
                                outer_for_task.push(AssistantMessageEvent::Done {
                                    reason,
                                    message: message.clone(),
                                });
                                outer_for_task.end(Some(message));
                            }
                        }
                        return;
                    }
                    AssistantMessageEvent::Error { reason, error } => {
                        if projection.is_none() {
                            outer_for_task.push(AssistantMessageEvent::Error {
                                reason,
                                error: error.clone(),
                            });
                            outer_for_task.end(Some(error));
                            return;
                        }
                        if !synchronize_terminal(
                            &mut projection,
                            &mut native_projection,
                            &outer_for_task,
                            &mut text_projection,
                            &mut saw_tool_call,
                            &error,
                            protocol,
                        ) {
                            return;
                        }
                        let Some(current_projection) = &mut projection else {
                            return;
                        };
                        terminal.source_error(
                            current_projection,
                            &error,
                            saw_tool_call,
                            error_reason_to_aborted_family(reason),
                        );
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
    outer_stream.set_cancellation_task(task);

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
        *projection = Some(StreamMessageProjection::new(
            outer_stream.clone(),
            source.clone(),
            StreamMessageProjectionOptions {
                preserve_source_metadata: true,
            },
        ));
    }
    if native_projection.is_none() {
        *native_projection = Some(RecoveryNativeProjection::new(
            outer_stream.clone(),
            protocol,
        ));
    }
    let current_projection = projection.as_mut().expect("projection just initialized");
    let current_native = native_projection
        .as_mut()
        .expect("native_projection just initialized");
    current_projection.sync(source);
    current_native.reserve_visible_ids(source);
    if let Some(mut current_text) = text_projection.take() {
        *saw_tool_call = current_text.finish(current_projection, current_native) || *saw_tool_call;
    }
    if current_native.synchronize_remaining(&mut current_projection.message, source) {
        true
    } else {
        terminate_recovery_stream_for_failure(
            outer_stream,
            current_projection,
            TerminateRecoveryStreamOptions {
                source,
                failure: RecoveryStreamFailure::Collision,
                protocol,
            },
        );
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentBlock, StopReason, TextContent, Tool, ToolCall, Usage};
    use serde_json::{Value, json};

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.into(),
            description: "d".into(),
            parameters: json!({"type": "object"}),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn message(content: Vec<ContentBlock>, stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            error_cause: None,
            prompt_cache_breakpoint: None,
            prompt_cache_diagnostics: None,
            thinking_level: None,
            provider_diagnostic: None,
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
            failure_source: None,
            timing: None,
        }
    }

    #[tokio::test]
    async fn terminal_wrappers_await_upstream_cleanup_without_replacing_results() {
        for wrapper in 0..3 {
            for terminal_error in [false, true] {
                let inner = AssistantMessageEventStream::assistant();
                let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
                let (release_tx, release_rx) = tokio::sync::oneshot::channel();
                inner.set_cancellation_handler(Box::new(move || {
                    Box::pin(async move {
                        entered_tx.send(()).expect("cleanup subscriber");
                        release_rx.await.expect("release");
                        Ok(())
                    })
                }));
                let outer = match wrapper {
                    0 => wrap_stream_with_invoke_recovery(inner.clone(), vec![], InvokeRecoveryOptions::default()),
                    1 => crate::tool_call_middleware::stream_wrapper::wrap_stream_with_tool_call_middleware(
                        inner.clone(),
                        crate::tool_call_middleware::context_transformer::get_protocol(ToolCallFormat::Gemma4Delimiter),
                        vec![],
                    ),
                    _ => crate::tool_call_middleware::protocols::kimi_xtml::thinking_recovery_stream::wrap_stream_with_kimi_thinking_recovery(inner.clone()),
                };
                let terminal = message(
                    vec![],
                    if terminal_error {
                        StopReason::Error
                    } else {
                        StopReason::Stop
                    },
                );
                if terminal_error {
                    inner.push(AssistantMessageEvent::Error {
                        reason: ErrorReason::Error,
                        error: terminal,
                    });
                } else {
                    inner.push(AssistantMessageEvent::Done {
                        reason: DoneReason::Stop,
                        message: terminal,
                    });
                }
                let bound = std::time::Duration::from_secs(5);
                let expected = tokio::time::timeout(bound, outer.result())
                    .await
                    .expect("result deadline")
                    .expect("result");
                let mut cancellation = Box::pin(outer.cancel());
                assert!(futures::poll!(cancellation.as_mut()).is_pending());
                tokio::time::timeout(bound, entered_rx)
                    .await
                    .expect("cleanup deadline")
                    .expect("cleanup");
                assert!(futures::poll!(cancellation.as_mut()).is_pending());
                release_tx.send(()).expect("release subscriber");
                tokio::time::timeout(bound, cancellation)
                    .await
                    .expect("cancel deadline")
                    .expect("cancel");
                assert_eq!(outer.result_if_ready(), Some(Ok(expected)));
            }
        }
    }

    #[tokio::test]
    async fn projection_cancellation_closes_upstream_before_flushing_partial_text() {
        for recovery in [false, true] {
            let inner = AssistantMessageEventStream::assistant();
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            inner.set_cancellation_handler(Box::new(move || {
                Box::pin(async move {
                    entered_tx.send(()).expect("close subscriber");
                    release_rx.await.expect("release");
                    Ok(())
                })
            }));
            let outer = if recovery {
                wrap_stream_with_invoke_recovery(
                    inner.clone(),
                    vec![],
                    InvokeRecoveryOptions::default(),
                )
            } else {
                crate::tool_call_middleware::stream_wrapper::wrap_stream_with_tool_call_middleware(
                    inner.clone(),
                    crate::tool_call_middleware::context_transformer::get_protocol(
                        ToolCallFormat::Gemma4Delimiter,
                    ),
                    vec![],
                )
            };
            let partial = message(
                vec![ContentBlock::Text(TextContent::default())],
                StopReason::Pending,
            );
            inner.push(AssistantMessageEvent::Start {
                partial: partial.clone(),
            });
            inner.push(AssistantMessageEvent::TextStart {
                content_index: 0,
                partial: partial.clone(),
            });
            inner.push(AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "hello".into(),
                partial,
            });
            let bound = std::time::Duration::from_secs(5);
            loop {
                let event = tokio::time::timeout(bound, outer.next())
                    .await
                    .expect("delta deadline")
                    .expect("event")
                    .expect("some");
                if matches!(event, AssistantMessageEvent::TextDelta { .. }) {
                    break;
                }
            }
            let mut cancellation = Box::pin(outer.cancel());
            assert!(futures::poll!(cancellation.as_mut()).is_pending());
            tokio::time::timeout(bound, entered_rx)
                .await
                .expect("close deadline")
                .expect("close");
            assert!(outer.result_if_ready().is_none());
            release_tx.send(()).expect("release subscriber");
            tokio::time::timeout(bound, cancellation)
                .await
                .expect("cancel deadline")
                .expect("cancel");
            let events = outer.collect().await.expect("events");
            assert!(matches!(
                events.last(),
                Some(AssistantMessageEvent::Error { .. })
            ));
            let result = outer.result_if_ready().expect("settled").expect("result");
            assert!(
                result
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Text(text) if text.text == "hello"))
            );
        }
    }

    #[tokio::test]
    async fn cancellation_awaits_inner_close_and_finalizes_outer() {
        let inner = AssistantMessageEventStream::assistant();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        inner.set_cancellation_handler(Box::new(move || {
            Box::pin(async move {
                entered_tx.send(()).expect("subscriber");
                release_rx.await.expect("release");
                Ok(())
            })
        }));
        let outer = wrap_stream_with_invoke_recovery(
            inner.clone(),
            vec![],
            InvokeRecoveryOptions::default(),
        );
        inner.push(AssistantMessageEvent::Start {
            partial: message(vec![], StopReason::Pending),
        });
        let deadline = std::time::Duration::from_secs(5);
        tokio::time::timeout(deadline, outer.next())
            .await
            .expect("start deadline")
            .expect("start");
        let cancel_stream = outer.clone();
        let cancellation = tokio::spawn(async move { cancel_stream.cancel().await });
        tokio::time::timeout(deadline, entered_rx)
            .await
            .expect("inner close deadline")
            .expect("inner close");
        assert!(!cancellation.is_finished());
        release_tx.send(()).expect("release subscriber");
        tokio::time::timeout(deadline, cancellation)
            .await
            .expect("cancel deadline")
            .expect("task")
            .expect("cancel");
        let result = tokio::time::timeout(deadline, outer.result())
            .await
            .expect("result deadline")
            .expect("result");
        assert_eq!(result.stop_reason, StopReason::Error);
    }

    #[tokio::test]
    async fn iterator_failure_flushes_buffered_protocol_text() {
        let inner = AssistantMessageEventStream::assistant();
        let outer =
            crate::tool_call_middleware::stream_wrapper::wrap_stream_with_tool_call_middleware(
                inner.clone(),
                crate::tool_call_middleware::context_transformer::get_protocol(
                    ToolCallFormat::Gemma4Delimiter,
                ),
                vec![],
            );
        let partial = message(
            vec![ContentBlock::Text(TextContent::default())],
            StopReason::Pending,
        );
        inner.push(AssistantMessageEvent::Start {
            partial: partial.clone(),
        });
        inner.push(AssistantMessageEvent::TextStart {
            content_index: 0,
            partial: partial.clone(),
        });
        inner.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "<|tool".into(),
            partial,
        });
        inner.fail(crate::utils::event_stream::StreamError::new(
            "iterator failed",
        ));
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), outer.result())
            .await
            .expect("deadline")
            .expect("result");
        assert!(
            result
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Text(text) if text.text == "<|tool"))
        );
    }

    #[tokio::test]
    async fn native_tool_calls_survive_into_the_terminal_message() {
        // senpi shares `projection.message` with the native projection, so a provider-native
        // tool call is part of the message the terminal finalizes. Without that, a Claude model on
        // openai-completions ended every tool turn as `toolUse` with no tool call.
        use crate::types::ThinkingContent;
        let thinking = || {
            ContentBlock::Thinking(ThinkingContent {
                thinking: "plan".into(),
                ..ThinkingContent::default()
            })
        };
        let text = || {
            ContentBlock::Text(TextContent {
                text: "Reading it.".into(),
                ..TextContent::default()
            })
        };
        for prefix in [
            vec![],
            vec![text()],
            vec![thinking()],
            vec![thinking(), text()],
        ] {
            let inner = AssistantMessageEventStream::assistant();
            let outer = wrap_stream_with_invoke_recovery(
                inner.clone(),
                vec![tool("read")],
                InvokeRecoveryOptions::default(),
            );
            inner.push(AssistantMessageEvent::Start {
                partial: message(vec![], StopReason::Pending),
            });
            let mut content = Vec::new();
            for block in &prefix {
                content.push(block.clone());
                let index = content.len() - 1;
                let partial = message(content.clone(), StopReason::Pending);
                match block {
                    ContentBlock::Thinking(_) => {
                        inner.push(AssistantMessageEvent::ThinkingStart {
                            content_index: index,
                            partial: partial.clone(),
                        });
                        inner.push(AssistantMessageEvent::ThinkingDelta {
                            content_index: index,
                            delta: "plan".into(),
                            partial: partial.clone(),
                        });
                        inner.push(AssistantMessageEvent::ThinkingEnd {
                            content_index: index,
                            content: "plan".into(),
                            partial,
                        });
                    }
                    _ => {
                        inner.push(AssistantMessageEvent::TextStart {
                            content_index: index,
                            partial: partial.clone(),
                        });
                        inner.push(AssistantMessageEvent::TextDelta {
                            content_index: index,
                            delta: "Reading it.".into(),
                            partial: partial.clone(),
                        });
                        inner.push(AssistantMessageEvent::TextEnd {
                            content_index: index,
                            content: "Reading it.".into(),
                            partial,
                        });
                    }
                }
            }
            let mut arguments = serde_json::Map::new();
            arguments.insert("path".into(), json!("hello.txt"));
            let call = ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments,
                incomplete: None,
                error_message: None,
                thought_signature: None,
                namespace: None,
            };
            content.push(ContentBlock::ToolCall(call.clone()));
            let index = content.len() - 1;
            let partial = message(content.clone(), StopReason::Pending);
            inner.push(AssistantMessageEvent::ToolcallStart {
                content_index: index,
                partial: partial.clone(),
            });
            inner.push(AssistantMessageEvent::ToolcallDelta {
                content_index: index,
                delta: r#"{"path":"hello.txt"}"#.into(),
                partial: partial.clone(),
            });
            inner.push(AssistantMessageEvent::ToolcallEnd {
                content_index: index,
                tool_call: call.clone(),
                partial,
            });
            inner.push(AssistantMessageEvent::Done {
                reason: DoneReason::ToolUse,
                message: message(content.clone(), StopReason::ToolUse),
            });
            inner.end(None);

            let mut terminal = None;
            while let Ok(Some(event)) = outer.next().await {
                if matches!(
                    event,
                    AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
                ) {
                    terminal = Some(event);
                    break;
                }
            }
            let Some(AssistantMessageEvent::Done {
                reason,
                message: result,
            }) = terminal
            else {
                panic!("expected done for prefix {prefix:?}")
            };
            assert_eq!(reason, DoneReason::ToolUse);
            assert_eq!(result.content, content, "prefix {prefix:?}");
        }
    }

    #[tokio::test]
    async fn recovers_a_leaked_invoke_from_streamed_text_and_promotes_stop_reason() {
        let inner = AssistantMessageEventStream::assistant();
        let outer = wrap_stream_with_invoke_recovery(
            inner.clone(),
            vec![tool("get_weather")],
            InvokeRecoveryOptions::default(),
        );

        let start_source = message(
            vec![ContentBlock::Text(TextContent::default())],
            StopReason::Pending,
        );
        inner.push(AssistantMessageEvent::Start {
            partial: start_source.clone(),
        });
        inner.push(AssistantMessageEvent::TextStart {
            content_index: 0,
            partial: start_source.clone(),
        });
        let invoke_text =
            r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#;
        let mut delta_source = start_source.clone();
        delta_source.content[0] = ContentBlock::Text(TextContent {
            text: invoke_text.into(),
            ..TextContent::default()
        });
        inner.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: invoke_text.into(),
            partial: delta_source.clone(),
        });
        inner.push(AssistantMessageEvent::TextEnd {
            content_index: 0,
            content: invoke_text.into(),
            partial: delta_source.clone(),
        });
        let mut done_source = delta_source.clone();
        done_source.stop_reason = StopReason::Stop;
        inner.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: done_source,
        });
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
        let Some(AssistantMessageEvent::Done { reason, message }) = done_event else {
            panic!("expected a done event")
        };
        assert_eq!(reason, DoneReason::ToolUse);
        assert!(message.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(tool_call) if tool_call.name == "get_weather")));
    }

    #[tokio::test]
    async fn passes_through_plain_text_with_no_tool_call_recovered() {
        let inner = AssistantMessageEventStream::assistant();
        let outer = wrap_stream_with_invoke_recovery(
            inner.clone(),
            vec![tool("get_weather")],
            InvokeRecoveryOptions::default(),
        );

        let start_source = message(
            vec![ContentBlock::Text(TextContent::default())],
            StopReason::Pending,
        );
        inner.push(AssistantMessageEvent::Start {
            partial: start_source.clone(),
        });
        inner.push(AssistantMessageEvent::TextStart {
            content_index: 0,
            partial: start_source.clone(),
        });
        let mut delta_source = start_source.clone();
        delta_source.content[0] = ContentBlock::Text(TextContent {
            text: "hello there".into(),
            ..TextContent::default()
        });
        inner.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "hello there".into(),
            partial: delta_source.clone(),
        });
        inner.push(AssistantMessageEvent::TextEnd {
            content_index: 0,
            content: "hello there".into(),
            partial: delta_source.clone(),
        });
        let mut done_source = delta_source.clone();
        done_source.stop_reason = StopReason::Stop;
        inner.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: done_source,
        });
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
        let Some(AssistantMessageEvent::Done { reason, message }) = done_event else {
            panic!("expected a done event")
        };
        assert_eq!(reason, DoneReason::Stop);
        assert!(
            !message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolCall(_)))
        );
    }

    // --- invoke-recovery-tool-alias.test.ts ---------------------------------
    //
    // Upstream gateways disguise tool names on the wire: ccapi-cf pascal-cases `todo` -> `Todo`, and
    // CC-pool layers expose non-native tools as `mcp_<hash>-<Name>` (e.g. `mcp_49f0-Todo`). Reverse
    // mapping only covers native tool_use blocks, so a leaked text invoke keeps the wire alias and
    // the recovery resolver must still recognize the registered tool behind it.

    fn schema_tool(name: &str, description: &str, parameters: Value) -> Tool {
        Tool {
            name: name.into(),
            description: description.into(),
            parameters,
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn todo_tool() -> Tool {
        schema_tool(
            "todo",
            "Manage todos",
            json!({"type": "object", "required": ["op"], "properties": {"op": {"type": "string"}, "task": {"type": "string"}}}),
        )
    }

    fn task_send_tool() -> Tool {
        schema_tool(
            "task_send",
            "Send to a task",
            json!({"type": "object", "required": ["to"], "properties": {"to": {"type": "string"}}}),
        )
    }

    fn todo_twin_tool() -> Tool {
        schema_tool(
            "to_do",
            "Ambiguous twin of todo",
            json!({"type": "object", "required": ["op"], "properties": {"op": {"type": "string"}}}),
        )
    }

    /// The TS fixtures' `TextStreamHarness`: a mutable producer for the real assistant event contract.
    struct TextStreamHarness {
        inner: AssistantMessageEventStream,
        partial: AssistantMessage,
        source_text: String,
    }

    impl TextStreamHarness {
        fn new() -> Self {
            // The TS harness pushes `text_start` with a `partial` object it mutates afterwards; the
            // wrapper therefore observes a partial whose `content[0]` is already the text block.
            // Rust events carry owned clones, so the block is present from the start.
            Self {
                inner: AssistantMessageEventStream::assistant(),
                partial: message(
                    vec![ContentBlock::Text(TextContent::default())],
                    StopReason::Pending,
                ),
                source_text: String::new(),
            }
        }

        fn start(&mut self) {
            self.inner.push(AssistantMessageEvent::Start {
                partial: self.partial.clone(),
            });
            self.inner.push(AssistantMessageEvent::TextStart {
                content_index: 0,
                partial: self.partial.clone(),
            });
        }

        fn delta(&mut self, text: &str) {
            self.source_text.push_str(text);
            if let Some(ContentBlock::Text(block)) = self.partial.content.first_mut() {
                block.text.clone_from(&self.source_text);
            }
            self.inner.push(AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: text.into(),
                partial: self.partial.clone(),
            });
        }

        fn finish(&mut self) {
            self.inner.push(AssistantMessageEvent::TextEnd {
                content_index: 0,
                content: self.source_text.clone(),
                partial: self.partial.clone(),
            });
            let final_message = message(
                vec![ContentBlock::Text(TextContent {
                    text: self.source_text.clone(),
                    ..TextContent::default()
                })],
                StopReason::Stop,
            );
            self.inner.push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: final_message,
            });
            self.inner.end(None);
        }
    }

    /// Runs one leaked invoke through the recovery wrapper and returns its events and the terminal
    /// message. The TS suite reads `await wrapped.result()`; the terminal `done` event carries the
    /// same message (that is exactly what the stream's `extract_result` returns), so the assertions
    /// are made against it instead of a second await that could park on an unsettled stream.
    async fn run_leak(
        input: &str,
        tools: Vec<Tool>,
    ) -> (Vec<AssistantMessageEvent>, AssistantMessage) {
        let mut producer = TextStreamHarness::new();
        let wrapped = wrap_stream_with_invoke_recovery(
            producer.inner.clone(),
            tools,
            InvokeRecoveryOptions::default(),
        );
        producer.start();
        producer.delta(input);
        producer.finish();
        let mut events = Vec::new();
        let mut result = None;
        while let Ok(Some(event)) = wrapped.next().await {
            let terminal = matches!(event, AssistantMessageEvent::Done { .. });
            if let AssistantMessageEvent::Done { message, .. } = &event {
                result = Some(message.clone());
            }
            events.push(event);
            if terminal {
                break;
            }
        }
        (events, result.expect("a terminal done event"))
    }

    fn call_blocks(result: &AssistantMessage) -> Vec<&ToolCall> {
        result
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect()
    }

    fn text_content(result: &AssistantMessage) -> String {
        result
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn tool_events(events: &[AssistantMessageEvent]) -> Vec<&AssistantMessageEvent> {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    AssistantMessageEvent::ToolcallStart { .. }
                        | AssistantMessageEvent::ToolcallDelta { .. }
                        | AssistantMessageEvent::ToolcallEnd { .. }
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn recovers_the_exact_ccapi_clb_leak_mapping_mcp_49f0_todo_onto_todo() {
        // Exact bytes captured from session 01a01381 event 1214 (ccapi-clb), including the stray
        // `count` text prefix the model emitted before the invoke.
        let leaked = "count\n<invoke name=\"mcp_49f0-Todo\">\n<parameter name=\"op\">done</parameter>\n<parameter name=\"task\">Review loop until mergeable</parameter>\n</invoke>";
        let (events, result) = run_leak(leaked, vec![todo_tool()]).await;
        let calls = call_blocks(&result);
        assert_eq!(calls.len(), 1, "{}", text_content(&result));
        assert_eq!(calls[0].name, "todo");
        assert_eq!(calls[0].arguments.get("op"), Some(&json!("done")));
        assert_eq!(
            calls[0].arguments.get("task"),
            Some(&json!("Review loop until mergeable"))
        );
        assert_eq!(tool_events(&events).len(), 3);
        assert_eq!(text_content(&result), "count\n");
        assert!(matches!(
            events.last(),
            Some(AssistantMessageEvent::Done { .. })
        ));
    }

    #[tokio::test]
    async fn resolves_hash_variant_prefixes_bare_pascal_aliases_and_cc_sdk_mcp_names() {
        let cases: Vec<(&str, &str, Vec<Tool>)> = vec![
            (
                r#"<invoke name="mcp_deadbeef-Todo"><parameter name="op">done</parameter></invoke>"#,
                "todo",
                vec![todo_tool()],
            ),
            (
                r#"<invoke name="Todo"><parameter name="op">done</parameter></invoke>"#,
                "todo",
                vec![todo_tool()],
            ),
            (
                r#"<invoke name="TaskSend"><parameter name="to">st_1</parameter></invoke>"#,
                "task_send",
                vec![task_send_tool()],
            ),
            (
                r#"<invoke name="mcp_49f0-TaskSend"><parameter name="to">st_1</parameter></invoke>"#,
                "task_send",
                vec![task_send_tool()],
            ),
            (
                r#"<invoke name="mcp__custom-tools__todo"><parameter name="op">done</parameter></invoke>"#,
                "todo",
                vec![todo_tool()],
            ),
        ];
        for (input, expected, tools) in cases {
            let (_, result) = run_leak(input, tools).await;
            let calls = call_blocks(&result);
            assert_eq!(calls.len(), 1, "alias case {input}");
            assert_eq!(calls[0].name, expected, "alias case {input}");
        }
    }

    #[tokio::test]
    async fn keeps_hallucinated_and_ambiguous_names_as_literal_text() {
        let (_, unknown) = run_leak(r#"<invoke name="mcp_49f0-DoesNotExist"><parameter name="op">done</parameter></invoke>"#, vec![todo_tool()]).await;
        assert!(call_blocks(&unknown).is_empty());
        assert!(text_content(&unknown).contains("mcp_49f0-DoesNotExist"));

        let (_, ambiguous) = run_leak(
            r#"<invoke name="mcp_1-TaskSend"><parameter name="to">st_1</parameter></invoke>"#,
            vec![task_send_tool(), schema_tool("tasksend", "Send to a task", json!({"type": "object", "required": ["to"], "properties": {"to": {"type": "string"}}}))],
        )
        .await;
        assert!(call_blocks(&ambiguous).is_empty());
        assert!(text_content(&ambiguous).contains("name=\"mcp_1-TaskSend\""));

        let (_, exact) = run_leak(
            r#"<invoke name="todo"><parameter name="op">done</parameter></invoke>"#,
            vec![todo_tool(), todo_twin_tool()],
        )
        .await;
        let exact_calls = call_blocks(&exact);
        assert_eq!(exact_calls.len(), 1);
        assert_eq!(exact_calls[0].name, "todo");
    }

    #[tokio::test]
    async fn preserves_the_classic_exact_name_recovery_path() {
        let (_, result) = run_leak(
            r#"<invoke name="Bash"><parameter name="command">echo hi</parameter></invoke>"#,
            vec![schema_tool("Bash", "Run a shell command", json!({"type": "object", "required": ["command"], "properties": {"command": {"type": "string"}}}))],
        )
        .await;
        let calls = call_blocks(&result);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
    }
}
