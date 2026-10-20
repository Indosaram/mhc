//! Port of senpi packages/ai/src/tool-call-middleware/stream-wrapper.ts.

use crate::types::{AssistantMessageEvent, DoneReason, StopReason, Tool};
use crate::utils::event_stream::AssistantMessageEventStream;

use super::stream_wrapper_shared::{StreamMessageProjection, StreamMessageProjectionOptions};
use super::types::ToolCallProtocol;

pub fn wrap_stream_with_tool_call_middleware(
    inner_stream: AssistantMessageEventStream,
    protocol: &'static dyn ToolCallProtocol,
    tools: Vec<Tool>,
) -> AssistantMessageEventStream {
    let outer_stream = AssistantMessageEventStream::assistant();
    let outer_for_task = outer_stream.clone();
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    let cancellation_source = inner_stream.clone();
    outer_stream.set_cancellation_handler(Box::new(move || {
        Box::pin(async move {
            if cancel_tx.send(()).is_err() {
                return cancellation_source.cancel().await;
            }
            match closed_rx.await {
                Ok(result) => result,
                Err(_) => cancellation_source.cancel().await,
            }
        })
    }));

    let task = tokio::spawn(async move {
        let mut parser = protocol.create_stream_parser(tools, None);
        let mut projection: Option<StreamMessageProjection> = None;
        let mut saw_tool_call = false;
        let mut parser_has_pending_input = false;

        loop {
            let next = tokio::select! {
                biased;
                request = &mut cancel_rx => {
                    if request.is_err() { return; }
                    let result = inner_stream.cancel().await;
                    if let Some(mut projection) = projection.take() {
                        if parser_has_pending_input {
                            projection.project_parser_events(&parser.finish());
                            projection.finish_text();
                        }
                        projection.finalize_dangling_tool_calls();
                        let mut message = projection.message.clone();
                        message.stop_reason = StopReason::Error;
                        message.error_message = Some(match &result {
                            Ok(()) => "Stream consumption cancelled".to_owned(),
                            Err(error) => error.message.clone(),
                        });
                        outer_for_task.push(AssistantMessageEvent::Error { reason: crate::types::ErrorReason::Error, error: message });
                    } else if let Err(error) = &result {
                        outer_for_task.fail(error.clone());
                    } else {
                        outer_for_task.end(None);
                    }
                    let _ = closed_tx.send(result);
                    return;
                }
                next = inner_stream.next() => next,
            };
            match next {
                Ok(Some(event)) => match event {
                    AssistantMessageEvent::Start { partial } => {
                        let new_projection = StreamMessageProjection::new(
                            outer_for_task.clone(),
                            partial,
                            StreamMessageProjectionOptions::default(),
                        );
                        outer_for_task.push(AssistantMessageEvent::Start {
                            partial: new_projection.message.clone(),
                        });
                        projection = Some(new_projection);
                    }
                    AssistantMessageEvent::TextStart {
                        content_index,
                        partial,
                    } => {
                        let Some(projection) = &mut projection else {
                            continue;
                        };
                        projection.sync(&partial);
                        projection.start_text(content_index, partial.content.get(content_index));
                    }
                    AssistantMessageEvent::TextDelta {
                        content_index: _,
                        delta,
                        partial,
                    } => {
                        let Some(projection) = &mut projection else {
                            continue;
                        };
                        projection.sync(&partial);
                        if !delta.is_empty() {
                            parser_has_pending_input = true;
                        }
                        let result = projection.project_parser_events(&parser.feed(&delta));
                        saw_tool_call = saw_tool_call || result.saw_tool_call;
                    }
                    AssistantMessageEvent::TextEnd {
                        content_index: _,
                        content: _,
                        partial,
                    } => {
                        let Some(projection) = &mut projection else {
                            continue;
                        };
                        projection.sync(&partial);
                        if parser_has_pending_input {
                            let result = projection.project_parser_events(&parser.finish());
                            saw_tool_call = saw_tool_call || result.saw_tool_call;
                            projection.finish_text();
                            parser_has_pending_input = false;
                        }
                    }
                    AssistantMessageEvent::ThinkingStart {
                        content_index,
                        partial,
                    } => {
                        let Some(projection) = &mut projection else {
                            continue;
                        };
                        projection.sync(&partial);
                        projection.start_thinking(content_index, &partial);
                    }
                    AssistantMessageEvent::ThinkingDelta {
                        content_index,
                        delta,
                        partial,
                    } => {
                        let Some(projection) = &mut projection else {
                            continue;
                        };
                        projection.sync(&partial);
                        projection.project_thinking_delta(content_index, &delta, &partial);
                    }
                    AssistantMessageEvent::ThinkingEnd {
                        content_index,
                        content,
                        partial,
                    } => {
                        let Some(projection) = &mut projection else {
                            continue;
                        };
                        projection.sync(&partial);
                        projection.finish_thinking(content_index, &content, &partial);
                    }
                    AssistantMessageEvent::Done { reason, message } => {
                        let mut projection = projection.take().unwrap_or_else(|| {
                            StreamMessageProjection::new(
                                outer_for_task.clone(),
                                message.clone(),
                                StreamMessageProjectionOptions::default(),
                            )
                        });
                        if parser_has_pending_input {
                            let result = projection.project_parser_events(&parser.finish());
                            saw_tool_call = saw_tool_call || result.saw_tool_call;
                            projection.finish_text();
                        }
                        projection.finalize_dangling_tool_calls();
                        let final_message = projection.finalize(&message, saw_tool_call);
                        let recovered =
                            saw_tool_call || projection.has_finalized_tool_call_content();
                        let final_reason = if recovered
                            && matches!(reason, DoneReason::Stop | DoneReason::Length)
                        {
                            DoneReason::ToolUse
                        } else {
                            reason
                        };
                        outer_for_task.push(AssistantMessageEvent::Done {
                            reason: final_reason,
                            message: final_message.clone(),
                        });
                        outer_for_task.end(Some(final_message));
                        return;
                    }
                    AssistantMessageEvent::Error { reason, error } => {
                        let Some(mut current_projection) = projection.take() else {
                            outer_for_task.push(AssistantMessageEvent::Error {
                                reason,
                                error: error.clone(),
                            });
                            outer_for_task.end(Some(error));
                            return;
                        };
                        current_projection.sync(&error);
                        if parser_has_pending_input {
                            let result = current_projection.project_parser_events(&parser.finish());
                            saw_tool_call = saw_tool_call || result.saw_tool_call;
                            current_projection.finish_text();
                        }
                        current_projection.finalize_dangling_tool_calls();
                        let message = current_projection.finalize(&error, saw_tool_call);
                        if current_projection.has_finalized_tool_call_content() {
                            let mut recovered_message = message.clone();
                            recovered_message.stop_reason = StopReason::ToolUse;
                            outer_for_task.push(AssistantMessageEvent::Done {
                                reason: DoneReason::ToolUse,
                                message: recovered_message.clone(),
                            });
                            outer_for_task.end(Some(recovered_message));
                            return;
                        }
                        outer_for_task.push(AssistantMessageEvent::Error {
                            reason,
                            error: message.clone(),
                        });
                        outer_for_task.end(Some(message));
                        return;
                    }
                    AssistantMessageEvent::ToolcallStart { .. }
                    | AssistantMessageEvent::ToolcallDelta { .. }
                    | AssistantMessageEvent::ToolcallEnd { .. } => {}
                },
                Ok(None) => {
                    let inner_message = match inner_stream.result_if_ready() {
                        Some(Ok(message)) => message,
                        Some(Err(error)) => {
                            outer_for_task.fail(error);
                            return;
                        }
                        None => {
                            if let Some(mut projection) = projection.take() {
                                if parser_has_pending_input {
                                    projection.project_parser_events(&parser.finish());
                                    projection.finish_text();
                                }
                                projection.finalize_dangling_tool_calls();
                                outer_for_task.end(Some(projection.message.clone()));
                            } else {
                                outer_for_task.end(None);
                            }
                            return;
                        }
                    };
                    let mut current_projection = projection.take().unwrap_or_else(|| {
                        StreamMessageProjection::new(
                            outer_for_task.clone(),
                            inner_message.clone(),
                            StreamMessageProjectionOptions::default(),
                        )
                    });
                    if parser_has_pending_input {
                        let result = current_projection.project_parser_events(&parser.finish());
                        saw_tool_call = saw_tool_call || result.saw_tool_call;
                        current_projection.finish_text();
                    }
                    current_projection.finalize_dangling_tool_calls();
                    let final_message = current_projection.finalize(&inner_message, saw_tool_call);
                    outer_for_task.end(Some(final_message));
                    return;
                }
                Err(error) => {
                    let mut current_projection = match projection.take() {
                        Some(current) => current,
                        None => match inner_stream.result_if_ready() {
                            Some(Ok(message)) => StreamMessageProjection::new(
                                outer_for_task.clone(),
                                message,
                                StreamMessageProjectionOptions::default(),
                            ),
                            Some(Err(inner_error)) => {
                                outer_for_task.fail(inner_error);
                                return;
                            }
                            None => {
                                outer_for_task.fail(error);
                                return;
                            }
                        },
                    };
                    if parser_has_pending_input {
                        current_projection.project_parser_events(&parser.finish());
                        current_projection.finish_text();
                    }
                    current_projection.finalize_dangling_tool_calls();
                    let mut fallback = current_projection.message.clone();
                    fallback.stop_reason = StopReason::Error;
                    fallback.error_message = Some(error.message.clone());
                    if let crate::utils::event_stream::StreamErrorKind::ProviderFailure(
                        diagnostic,
                    ) = &error.kind
                    {
                        fallback.provider_diagnostic = Some(diagnostic.clone());
                    }
                    outer_for_task.push(AssistantMessageEvent::Error {
                        reason: crate::types::ErrorReason::Error,
                        error: fallback,
                    });
                    outer_for_task.end(None);
                    return;
                }
            }
        }
    });
    outer_stream.set_cancellation_task(task);

    outer_stream
}
