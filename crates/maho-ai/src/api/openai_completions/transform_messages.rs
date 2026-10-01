//! Port of senpi packages/ai/src/api/transform-messages.ts.
//!
//! Node-private copy: `src/api/transform_messages.rs` is owned by node 12-misc. Kept here so this
//! lane compiles standalone; delete it once 12-misc merges.

use crate::model::Model;
use crate::types::{AssistantMessage, ContentBlock, Message, StopReason, TextContent, ToolResultMessage};
use crate::utils::diagnostics::now_ms;

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str = "(tool image omitted: model does not support images)";
const NO_VIDEO_USER_PLACEHOLDER: &str = "(video omitted: model does not support video input)";
const NO_VIDEO_TOOL_PLACEHOLDER: &str = "(tool video omitted: model does not support video input)";

fn replace_media_with_placeholder(
    content: &[ContentBlock],
    placeholder: &str,
    matches: impl Fn(&crate::types::ImageContent) -> bool,
) -> Vec<ContentBlock> {
    let has_match = content
        .iter()
        .any(|block| matches!(block, ContentBlock::Image(image) if matches(image)));
    if !has_match {
        return content.to_vec();
    }
    let mut result: Vec<ContentBlock> = Vec::new();
    let mut previous_was_placeholder = false;

    for block in content {
        if let ContentBlock::Image(image) = block
            && matches(image) {
                if !previous_was_placeholder {
                    result.push(ContentBlock::Text(TextContent { text: placeholder.to_owned(), ..TextContent::default() }));
                }
                previous_was_placeholder = true;
                continue;
            }

        previous_was_placeholder = matches!(block, ContentBlock::Text(text) if text.text == placeholder);
        result.push(block.clone());
    }

    result
}

fn replace_images_with_placeholder(content: &[ContentBlock], placeholder: &str) -> Vec<ContentBlock> {
    let mut result: Vec<ContentBlock> = Vec::new();
    let mut previous_was_placeholder = false;

    for block in content {
        if matches!(block, ContentBlock::Image(_)) {
            if !previous_was_placeholder {
                result.push(ContentBlock::Text(TextContent { text: placeholder.to_owned(), ..TextContent::default() }));
            }
            previous_was_placeholder = true;
            continue;
        }

        previous_was_placeholder = matches!(block, ContentBlock::Text(text) if text.text == placeholder);
        result.push(block.clone());
    }

    result
}

fn downgrade_unsupported_images(messages: &[Message], model: &Model) -> Vec<Message> {
    use crate::types::InputModality;

    let supports_images = model.input.contains(&InputModality::Image);
    let supports_video = model.input.contains(&InputModality::Video);
    if supports_images && supports_video {
        return messages.to_vec();
    }

    messages
        .iter()
        .map(|message| match message {
            Message::User(user) => {
                let crate::types::UserContent::Blocks(blocks) = &user.content else { return message.clone() };
                let mut content = blocks.clone();
                if !supports_video {
                    content = replace_media_with_placeholder(&content, NO_VIDEO_USER_PLACEHOLDER, |image| {
                        crate::types::is_video_mime_type(&image.mime_type)
                    });
                }
                if !supports_images {
                    content = replace_images_with_placeholder(&content, NON_VISION_USER_IMAGE_PLACEHOLDER);
                }
                let mut next = user.clone();
                next.content = crate::types::UserContent::Blocks(content);
                Message::User(next)
            }
            Message::ToolResult(result) => {
                let mut content = result.content.clone();
                if !supports_video {
                    content = replace_media_with_placeholder(&content, NO_VIDEO_TOOL_PLACEHOLDER, |image| {
                        crate::types::is_video_mime_type(&image.mime_type)
                    });
                }
                if !supports_images {
                    content = replace_images_with_placeholder(&content, NON_VISION_TOOL_IMAGE_PLACEHOLDER);
                }
                let mut next = result.clone();
                next.content = content;
                Message::ToolResult(next)
            }
            _ => message.clone(),
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct TransformMessagesOptions {
    pub preserve_thinking: Option<bool>,
    pub preserve_text_signatures: Option<bool>,
    pub preserve_unsigned_thinking: Option<bool>,
    pub normalize_same_model_tool_call_ids: Option<bool>,
}

pub type ToolCallIdNormalizer<'a> = &'a dyn Fn(&str, &Model, &AssistantMessage) -> String;

pub fn transform_messages(
    messages: &[Message],
    model: &Model,
    normalize_tool_call_id: Option<ToolCallIdNormalizer<'_>>,
    options: &TransformMessagesOptions,
) -> Vec<Message> {
    let mut tool_call_id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let image_aware_messages = downgrade_unsupported_images(messages, model);
    let preserve_thinking = options.preserve_thinking.unwrap_or(true);
    let preserve_text_signatures = options.preserve_text_signatures.unwrap_or(false);
    let preserve_unsigned_thinking = options.preserve_unsigned_thinking.unwrap_or(false);
    let normalize_same_model_tool_call_ids = options.normalize_same_model_tool_call_ids.unwrap_or(false);

    let mut transformed: Vec<Message> = Vec::with_capacity(image_aware_messages.len());
    for message in &image_aware_messages {
        match message {
            Message::User(_) => transformed.push(message.clone()),
            Message::ToolResult(result) => match tool_call_id_map.get(&result.tool_call_id) {
                Some(normalized) if normalized != &result.tool_call_id => {
                    let mut next = result.clone();
                    next.tool_call_id = normalized.clone();
                    transformed.push(Message::ToolResult(next));
                }
                _ => transformed.push(message.clone()),
            },
            Message::ConfigurationUpdate(_) => transformed.push(message.clone()),
            Message::Assistant(assistant) => {
                let is_same_model = assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model == model.id;
                let has_tool_calls = assistant.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_)));
                let preserve_provider_state = preserve_thinking || has_tool_calls;

                let mut transformed_content: Vec<ContentBlock> = Vec::with_capacity(assistant.content.len());
                for block in &assistant.content {
                    match block {
                        ContentBlock::Thinking(thinking) => {
                            if thinking.redacted == Some(true) {
                                if is_same_model {
                                    transformed_content.push(block.clone());
                                }
                                continue;
                            }
                            let has_usable_signature = thinking
                                .thinking_signature
                                .as_deref()
                                .is_some_and(|signature| !signature.trim().is_empty());
                            if is_same_model
                                && has_usable_signature
                                && (preserve_provider_state || thinking.thinking.trim().is_empty())
                            {
                                transformed_content.push(block.clone());
                                continue;
                            }
                            if thinking.thinking.is_empty() || thinking.thinking.trim().is_empty() {
                                continue;
                            }
                            if is_same_model {
                                if preserve_provider_state || (preserve_unsigned_thinking && !has_usable_signature) {
                                    transformed_content.push(block.clone());
                                }
                                continue;
                            }
                            transformed_content.push(ContentBlock::Text(TextContent {
                                text: thinking.thinking.clone(),
                                ..TextContent::default()
                            }));
                        }
                        ContentBlock::Text(text) => {
                            if is_same_model && (preserve_provider_state || preserve_text_signatures) {
                                transformed_content.push(block.clone());
                            } else {
                                transformed_content.push(ContentBlock::Text(TextContent {
                                    text: text.text.clone(),
                                    ..TextContent::default()
                                }));
                            }
                        }
                        ContentBlock::ToolCall(tool_call) => {
                            let mut normalized_tool_call = tool_call.clone();
                            if (!is_same_model || !preserve_provider_state) && tool_call.thought_signature.is_some() {
                                normalized_tool_call.thought_signature = None;
                            }
                            if (!is_same_model || normalize_same_model_tool_call_ids)
                                && let Some(normalize) = normalize_tool_call_id
                            {
                                let normalized_id = normalize(&tool_call.id, model, assistant);
                                if normalized_id != tool_call.id {
                                    tool_call_id_map.insert(tool_call.id.clone(), normalized_id.clone());
                                    normalized_tool_call.id = normalized_id;
                                }
                            }
                            transformed_content.push(ContentBlock::ToolCall(normalized_tool_call));
                        }
                        other => transformed_content.push(other.clone()),
                    }
                }

                let mut next = assistant.as_ref().clone();
                next.content = transformed_content;
                transformed.push(Message::Assistant(Box::new(next)));
            }
        }
    }

    let mut result: Vec<Message> = Vec::new();
    let mut tool_results_by_id: std::collections::HashMap<String, Vec<ResultEntry>> = std::collections::HashMap::new();
    let mut next_tool_call_index_by_id: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    let mut dropped_call_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (source_index, message) in transformed.iter().enumerate() {
        match message {
            Message::ToolResult(result_message) => {
                let entries = tool_results_by_id.entry(result_message.tool_call_id.clone()).or_default();
                entries.push(ResultEntry { source_index, consumed: false });
            }
            Message::Assistant(assistant) => {
                if matches!(assistant.stop_reason, StopReason::Error | StopReason::Aborted) {
                    for block in &assistant.content {
                        if let ContentBlock::ToolCall(tool_call) = block {
                            dropped_call_ids.insert(tool_call.id.clone());
                        }
                    }
                    continue;
                }
                for block in &assistant.content {
                    if let ContentBlock::ToolCall(tool_call) = block {
                        next_tool_call_index_by_id.entry(tool_call.id.clone()).or_default().push(source_index);
                    }
                }
            }
            _ => {}
        }
    }

    for (source_index, message) in transformed.iter().enumerate() {
        match message {
            Message::ToolResult(result_message) => {
                let entry = tool_results_by_id
                    .get(&result_message.tool_call_id)
                    .and_then(|entries| entries.iter().find(|entry| entry.source_index == source_index));
                let consumed = entry.is_some_and(|entry| entry.consumed);
                if !consumed
                    && (!dropped_call_ids.contains(&result_message.tool_call_id)
                        || next_tool_call_index_by_id.contains_key(&result_message.tool_call_id))
                {
                    result.push(message.clone());
                }
            }
            Message::Assistant(assistant) => {
                if matches!(assistant.stop_reason, StopReason::Error | StopReason::Aborted) {
                    continue;
                }
                result.push(message.clone());
                for block in &assistant.content {
                    let ContentBlock::ToolCall(tool_call) = block else { continue };
                    let next_declaration_index = next_tool_call_index_by_id
                        .get(&tool_call.id)
                        .and_then(|indexes| indexes.iter().copied().find(|index| *index > source_index))
                        .unwrap_or(usize::MAX);
                    let matched = tool_results_by_id.get_mut(&tool_call.id).and_then(|entries| {
                        entries.iter_mut().find(|entry| {
                            !entry.consumed
                                && entry.source_index > source_index
                                && entry.source_index < next_declaration_index
                        })
                    });

                    match matched {
                        Some(entry) => {
                            entry.consumed = true;
                            let index = entry.source_index;
                            result.push(transformed[index].clone());
                        }
                        None => result.push(Message::ToolResult(ToolResultMessage {
                            tool_call_id: tool_call.id.clone(),
                            tool_name: tool_call.name.clone(),
                            content: vec![ContentBlock::Text(TextContent {
                                text: "No result provided".to_owned(),
                                ..TextContent::default()
                            })],
                            details: None,
                            usage: None,
                            added_tool_names: None,
                            is_error: true,
                            timestamp: now_ms(),
                        })),
                    }
                }
            }
            _ => result.push(message.clone()),
        }
    }

    result
}

struct ResultEntry {
    source_index: usize,
    consumed: bool,
}
