//! Port of senpi `packages/agent/src/proxy-event-reducer.ts`.

use std::collections::BTreeMap;

use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, DoneReason, ErrorReason, StopReason,
    TextContent, ThinkingContent, ToolCall,
};
use maho_ai::utils::json_parse::parse_streaming_json;
use serde_json::Map;

use crate::proxy::ProxyAssistantMessageEvent;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ProxyEventError {
    pub message: String,
}

fn ensure_block(content: &mut Vec<ContentBlock>, index: usize) {
    while content.len() <= index {
        content.push(ContentBlock::Text(TextContent::default()));
    }
}

fn set_raw_arguments(
    partial: &mut AssistantMessage,
    content_index: usize,
    partial_json: &mut BTreeMap<usize, String>,
    raw: String,
) -> Result<(), ProxyEventError> {
    let invalid = || ProxyEventError {
        message: "Received tool argument update for non-toolCall content".to_owned(),
    };
    let content = partial.content.get_mut(content_index).ok_or_else(invalid)?;
    let ContentBlock::ToolCall(tool_call) = content else {
        return Err(invalid());
    };
    let parsed = parse_streaming_json(Some(&raw));
    tool_call.arguments = parsed.as_object().cloned().unwrap_or_default();
    partial_json.insert(content_index, raw);
    Ok(())
}

/// Process a proxy event and update the partial message.
pub fn process_proxy_event(
    proxy_event: ProxyAssistantMessageEvent,
    partial: &mut AssistantMessage,
    partial_json: &mut BTreeMap<usize, String>,
) -> Result<Option<AssistantMessageEvent>, ProxyEventError> {
    Ok(match proxy_event {
        ProxyAssistantMessageEvent::Start => Some(AssistantMessageEvent::Start {
            partial: partial.clone(),
        }),

        ProxyAssistantMessageEvent::TextStart { content_index } => {
            ensure_block(&mut partial.content, content_index);
            partial.content[content_index] = ContentBlock::Text(TextContent::default());
            Some(AssistantMessageEvent::TextStart {
                content_index,
                partial: partial.clone(),
            })
        }

        ProxyAssistantMessageEvent::TextDelta {
            content_index,
            delta,
        } => {
            let invalid = || ProxyEventError {
                message: "Received text_delta for non-text content".to_owned(),
            };
            let content = partial.content.get_mut(content_index).ok_or_else(invalid)?;
            match content {
                ContentBlock::Text(text) => {
                    text.text.push_str(&delta);
                    Some(AssistantMessageEvent::TextDelta {
                        content_index,
                        delta,
                        partial: partial.clone(),
                    })
                }
                _ => return Err(invalid()),
            }
        }

        ProxyAssistantMessageEvent::TextEnd {
            content_index,
            content_signature,
        } => {
            let invalid = || ProxyEventError {
                message: "Received text_end for non-text content".to_owned(),
            };
            let content = partial.content.get_mut(content_index).ok_or_else(invalid)?;
            match content {
                ContentBlock::Text(text) => {
                    text.text_signature = content_signature;
                    let text = text.text.clone();
                    Some(AssistantMessageEvent::TextEnd {
                        content_index,
                        content: text,
                        partial: partial.clone(),
                    })
                }
                _ => return Err(invalid()),
            }
        }

        ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
            ensure_block(&mut partial.content, content_index);
            partial.content[content_index] = ContentBlock::Thinking(ThinkingContent::default());
            Some(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: partial.clone(),
            })
        }

        ProxyAssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
        } => {
            let invalid = || ProxyEventError {
                message: "Received thinking_delta for non-thinking content".to_owned(),
            };
            let content = partial.content.get_mut(content_index).ok_or_else(invalid)?;
            match content {
                ContentBlock::Thinking(thinking) => {
                    thinking.thinking.push_str(&delta);
                    Some(AssistantMessageEvent::ThinkingDelta {
                        content_index,
                        delta,
                        partial: partial.clone(),
                    })
                }
                _ => return Err(invalid()),
            }
        }

        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index,
            content_signature,
        } => {
            let invalid = || ProxyEventError {
                message: "Received thinking_end for non-thinking content".to_owned(),
            };
            let content = partial.content.get_mut(content_index).ok_or_else(invalid)?;
            match content {
                ContentBlock::Thinking(thinking) => {
                    thinking.thinking_signature = content_signature;
                    let text = thinking.thinking.clone();
                    Some(AssistantMessageEvent::ThinkingEnd {
                        content_index,
                        content: text,
                        partial: partial.clone(),
                    })
                }
                _ => return Err(invalid()),
            }
        }

        ProxyAssistantMessageEvent::ToolcallStart {
            content_index,
            id,
            tool_name,
        } => {
            ensure_block(&mut partial.content, content_index);
            partial.content[content_index] = ContentBlock::ToolCall(ToolCall {
                id,
                name: tool_name,
                arguments: Map::new(),
                incomplete: None,
                error_message: None,
                thought_signature: None,
                namespace: None,
            });
            partial_json.insert(content_index, String::new());
            Some(AssistantMessageEvent::ToolcallStart {
                content_index,
                partial: partial.clone(),
            })
        }

        ProxyAssistantMessageEvent::ToolcallDelta {
            content_index,
            delta,
        } => {
            let Some(ContentBlock::ToolCall(_)) = partial.content.get(content_index) else {
                return Err(ProxyEventError {
                    message: "Received toolcall_delta for non-toolCall content".to_owned(),
                });
            };
            let mut raw = partial_json
                .get(&content_index)
                .cloned()
                .unwrap_or_default();
            raw.push_str(&delta);
            set_raw_arguments(partial, content_index, partial_json, raw)?;
            Some(AssistantMessageEvent::ToolcallDelta {
                content_index,
                delta,
                partial: partial.clone(),
            })
        }

        ProxyAssistantMessageEvent::MessageToolArgumentsReset {
            content_index,
            raw_argument_prefix,
            ..
        } => {
            let Some(ContentBlock::ToolCall(_)) = partial.content.get(content_index) else {
                return Err(ProxyEventError {
                    message: "Received message_tool_arguments_reset for non-toolCall content"
                        .to_owned(),
                });
            };
            set_raw_arguments(partial, content_index, partial_json, raw_argument_prefix)?;
            None
        }

        ProxyAssistantMessageEvent::ToolcallEnd {
            content_index,
            tool_call,
        } => {
            let Some(content) = partial.content.get_mut(content_index) else {
                return Ok(None);
            };
            match content {
                ContentBlock::ToolCall(current) => {
                    let tool_call = tool_call.unwrap_or_else(|| current.clone());
                    partial.content[content_index] = ContentBlock::ToolCall(tool_call.clone());
                    partial_json.remove(&content_index);
                    Some(AssistantMessageEvent::ToolcallEnd {
                        content_index,
                        tool_call,
                        partial: partial.clone(),
                    })
                }
                _ => None,
            }
        }

        ProxyAssistantMessageEvent::Done {
            reason,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = reason;
            partial.usage = usage;
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            Some(AssistantMessageEvent::Done {
                reason: done_reason(reason),
                message: partial.clone(),
            })
        }

        ProxyAssistantMessageEvent::Error {
            reason,
            error_message,
            error_cause,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = error_stop_reason(reason);
            partial.error_message = error_message;
            partial.error_cause = error_cause;
            partial.usage = usage;
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            Some(AssistantMessageEvent::Error {
                reason,
                error: partial.clone(),
            })
        }
    })
}

fn done_reason(reason: StopReason) -> DoneReason {
    match reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    }
}

fn error_stop_reason(reason: ErrorReason) -> StopReason {
    match reason {
        ErrorReason::Aborted => StopReason::Aborted,
        ErrorReason::Error => StopReason::Error,
    }
}
