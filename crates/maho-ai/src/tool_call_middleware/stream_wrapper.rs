//! Port of senpi packages/ai/src/tool-call-middleware/stream-wrapper.ts.

use crate::types::{AssistantMessage, AssistantMessageEvent, DoneReason, StopReason, Tool};
use crate::utils::event_stream::{AssistantMessageEventStream, StreamError};

use super::stream_wrapper_shared::{StreamMessageProjection, StreamMessageProjectionOptions};
use super::types::{StreamParser, ToolCallProtocol};

pub fn wrap_stream_with_tool_call_middleware(
    inner_stream: AssistantMessageEventStream,
    protocol: &'static dyn ToolCallProtocol,
    tools: Vec<Tool>,
) -> AssistantMessageEventStream {
    let outer_stream = AssistantMessageEventStream::assistant();
    let outer_for_task = outer_stream.clone();

    tokio::spawn(async move {
        let mut parser = protocol.create_stream_parser(tools, None);
        let mut projection: Option<StreamMessageProjection> = None;
        let mut saw_tool_call = false;
        let mut parser_has_pending_input = false;

        loop {
            match inner_stream.next().await {
                Ok(Some(event)) => match event {
                    AssistantMessageEvent::Start { partial } => {
                        let new_projection = StreamMessageProjection::new(outer_for_task.clone(), partial, StreamMessageProjectionOptions::default());
                        outer_for_task.push(AssistantMessageEvent::Start { partial: new_projection.message.clone() });
                        projection = Some(new_projection);
                    }
                    AssistantMessageEvent::TextStart { content_index, partial } => {
                        let Some(projection) = &mut projection else { continue };
                        projection.sync(partial.clone());
                        projection.start_text(content_index, partial.content.get(content_index));
                    }
                    AssistantMessageEvent::TextDelta { content_index: _, delta, partial } => {
                        let Some(projection) = &mut projection else { continue };
                        projection.sync(partial);
                        if !delta.is_empty() {
                            parser_has_pending_input = true;
                        }
                        let result = projection.project_parser_events(&parser.feed(&delta));
                        saw_tool_call = saw_tool_call || result.saw_tool_call;
                    }
                    AssistantMessageEvent::TextEnd { content_index: _, content: _, partial } => {
                        let Some(projection) = &mut projection else { continue };
                        projection.sync(partial);
                        if parser_has_pending_input {
                            let result = projection.project_parser_events(&parser.finish());
                            saw_tool_call = saw_tool_call || result.saw_tool_call;
                            projection.finish_text();
                            parser_has_pending_input = false;
                        }
                    }
                    AssistantMessageEvent::ThinkingStart { content_index, partial } => {
                        let Some(projection) = &mut projection else { continue };
                        projection.sync(partial.clone());
                        projection.start_thinking(content_index, &partial);
                    }
                    AssistantMessageEvent::ThinkingDelta { content_index, delta, partial } => {
                        let Some(projection) = &mut projection else { continue };
                        projection.sync(partial.clone());
                        projection.project_thinking_delta(content_index, &delta, &partial);
                    }
                    AssistantMessageEvent::ThinkingEnd { content_index, content, partial } => {
                        let Some(projection) = &mut projection else { continue };
                        projection.sync(partial.clone());
                        projection.finish_thinking(content_index, &content, &partial);
                    }
                    AssistantMessageEvent::Done { reason, message } => {
                        let mut projection = projection.take().unwrap_or_else(|| {
                            StreamMessageProjection::new(outer_for_task.clone(), message.clone(), StreamMessageProjectionOptions::default())
                        });
                        if parser_has_pending_input {
                            let result = projection.project_parser_events(&parser.finish());
                            saw_tool_call = saw_tool_call || result.saw_tool_call;
                            projection.finish_text();
                            parser_has_pending_input = false;
                        }
                        projection.finalize_dangling_tool_calls();
                        let final_message = projection.finalize(&message, saw_tool_call);
                        let recovered = saw_tool_call || projection.has_finalized_tool_call_content();
                        let final_reason = if recovered && matches!(reason, DoneReason::Stop | DoneReason::Length) {
                            DoneReason::ToolUse
                        } else {
                            reason
                        };
                        outer_for_task.push(AssistantMessageEvent::Done { reason: final_reason, message: final_message.clone() });
                        outer_for_task.end(Some(final_message));
                        return;
                    }
                    AssistantMessageEvent::Error { reason, error } => {
                        let Some(mut current_projection) = projection.take() else {
                            outer_for_task.push(AssistantMessageEvent::Error { reason, error: error.clone() });
                            outer_for_task.end(Some(error));
                            return;
                        };
                        current_projection.sync(error.clone());
                        if parser_has_pending_input {
                            let result = current_projection.project_parser_events(&parser.finish());
                            saw_tool_call = saw_tool_call || result.saw_tool_call;
                            current_projection.finish_text();
                            parser_has_pending_input = false;
                        }
                        current_projection.finalize_dangling_tool_calls();
                        let message = current_projection.finalize(&error, saw_tool_call);
                        if current_projection.has_finalized_tool_call_content() {
                            let mut recovered_message = message.clone();
                            recovered_message.stop_reason = StopReason::ToolUse;
                            outer_for_task.push(AssistantMessageEvent::Done { reason: DoneReason::ToolUse, message: recovered_message.clone() });
                            outer_for_task.end(Some(recovered_message));
                            return;
                        }
                        outer_for_task.push(AssistantMessageEvent::Error { reason, error: message.clone() });
                        outer_for_task.end(Some(message));
                        return;
                    }
                    AssistantMessageEvent::ToolcallStart { .. } | AssistantMessageEvent::ToolcallDelta { .. } | AssistantMessageEvent::ToolcallEnd { .. } => {}
                },
                Ok(None) => {
                    let inner_message = match inner_stream.result().await {
                        Ok(message) => message,
                        Err(error) => {
                            outer_for_task.fail(error);
                            return;
                        }
                    };
                    let mut current_projection = projection.take().unwrap_or_else(|| {
                        StreamMessageProjection::new(outer_for_task.clone(), inner_message.clone(), StreamMessageProjectionOptions::default())
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
                    if parser_has_pending_input {
                        let _ = &mut parser;
                    }
                    let mut current_projection = match projection.take() {
                        Some(current) => current,
                        None => match inner_stream.result().await {
                            Ok(message) => StreamMessageProjection::new(outer_for_task.clone(), message, StreamMessageProjectionOptions::default()),
                            Err(inner_error) => {
                                outer_for_task.fail(StreamError::new(inner_error.message));
                                return;
                            }
                        },
                    };
                    current_projection.finalize_dangling_tool_calls();
                    let mut fallback = current_projection.message.clone();
                    fallback.stop_reason = StopReason::Error;
                    fallback.error_message = Some(error.message.clone());
                    outer_for_task.push(AssistantMessageEvent::Error { reason: crate::types::ErrorReason::Error, error: fallback });
                    outer_for_task.end(None);
                    return;
                }
            }
        }
    });

    outer_stream
}
