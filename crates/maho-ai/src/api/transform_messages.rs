//! Port of senpi packages/ai/src/api/transform-messages.ts.

use crate::types::{
    is_video_mime_type, AssistantMessage, ContentBlock, Message, Model, TextContent, ToolResultMessage,
    UserContent,
};
use crate::utils::diagnostics::now_ms;

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str = "(tool image omitted: model does not support images)";
const NO_VIDEO_USER_PLACEHOLDER: &str = "(video omitted: model does not support video input)";
const NO_VIDEO_TOOL_PLACEHOLDER: &str = "(tool video omitted: model does not support video input)";

fn replace_media_with_placeholder(
    content: &[ContentBlock],
    placeholder: &str,
    matches: impl Fn(&ContentBlock) -> bool,
) -> Vec<ContentBlock> {
    if !content.iter().any(|block| block.type_name() == "image" && matches(block)) {
        return content.to_vec();
    }
    let mut result: Vec<ContentBlock> = Vec::with_capacity(content.len());
    let mut previous_was_placeholder = false;

    for block in content {
        if block.type_name() == "image" && matches(block) {
            if !previous_was_placeholder {
                result.push(ContentBlock::text(placeholder));
            }
            previous_was_placeholder = true;
            continue;
        }

        result.push(block.clone());
        previous_was_placeholder = matches!(block, ContentBlock::Text(text) if text.text == placeholder);
    }

    result
}

fn replace_images_with_placeholder(content: &[ContentBlock], placeholder: &str) -> Vec<ContentBlock> {
    let mut result: Vec<ContentBlock> = Vec::with_capacity(content.len());
    let mut previous_was_placeholder = false;

    for block in content {
        if block.type_name() == "image" {
            if !previous_was_placeholder {
                result.push(ContentBlock::text(placeholder));
            }
            previous_was_placeholder = true;
            continue;
        }

        result.push(block.clone());
        previous_was_placeholder = matches!(block, ContentBlock::Text(text) if text.text == placeholder);
    }

    result
}

fn is_video_block(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::Image(image) if is_video_mime_type(&image.mime_type))
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
        .map(|msg| match msg {
            Message::User(user) => {
                let UserContent::Blocks(blocks) = &user.content else { return msg.clone() };
                let mut content = blocks.clone();
                if !supports_video {
                    content = replace_media_with_placeholder(&content, NO_VIDEO_USER_PLACEHOLDER, is_video_block);
                }
                if !supports_images {
                    content = replace_images_with_placeholder(&content, NON_VISION_USER_IMAGE_PLACEHOLDER);
                }
                let mut user = user.clone();
                user.content = UserContent::Blocks(content);
                Message::User(user)
            }
            Message::ToolResult(result) => {
                let mut content = result.content.clone();
                if !supports_video {
                    content = replace_media_with_placeholder(&content, NO_VIDEO_TOOL_PLACEHOLDER, is_video_block);
                }
                if !supports_images {
                    content = replace_images_with_placeholder(&content, NON_VISION_TOOL_IMAGE_PLACEHOLDER);
                }
                let mut result = result.clone();
                result.content = content;
                Message::ToolResult(result)
            }
            other => other.clone(),
        })
        .collect()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransformMessagesOptions {
    /// Preserve provider-native thinking/reasoning replay state for the current model. When false,
    /// standalone same-model thinking is omitted, while thinking attached to tool calls is preserved
    /// for provider validation.
    pub preserve_thinking: Option<bool>,
    pub preserve_text_signatures: Option<bool>,
    /// Preserve same-model thinking blocks that do not carry a usable signature so provider adapters
    /// can downgrade them to plain text or provider-specific compatibility payloads.
    pub preserve_unsigned_thinking: Option<bool>,
    /// Normalize tool-call IDs even when stored provider/API/model metadata match the target.
    pub normalize_same_model_tool_call_ids: Option<bool>,
}

pub type NormalizeToolCallId<'a> = &'a dyn Fn(&str, &Model, &AssistantMessage) -> String;

pub fn transform_messages(
    messages: &[Message],
    model: &Model,
    normalize_tool_call_id: Option<NormalizeToolCallId<'_>>,
    options: &TransformMessagesOptions,
) -> Vec<Message> {
    use std::collections::HashMap;

    let mut tool_call_id_map: HashMap<String, String> = HashMap::new();
    let image_aware_messages = downgrade_unsupported_images(messages, model);
    let preserve_thinking = options.preserve_thinking.unwrap_or(true);
    let preserve_text_signatures = options.preserve_text_signatures.unwrap_or(false);
    let preserve_unsigned_thinking = options.preserve_unsigned_thinking.unwrap_or(false);
    let normalize_same_model_tool_call_ids = options.normalize_same_model_tool_call_ids.unwrap_or(false);

    let transformed: Vec<Message> = image_aware_messages
        .into_iter()
        .map(|msg| match msg {
            Message::User(_) => msg,
            Message::ToolResult(result) => match tool_call_id_map.get(&result.tool_call_id) {
                Some(normalized) if normalized != &result.tool_call_id => {
                    let mut result = result;
                    result.tool_call_id = normalized.clone();
                    Message::ToolResult(result)
                }
                _ => Message::ToolResult(result),
            },
            Message::ConfigurationUpdate(_) => msg,
            Message::Assistant(assistant) => {
                let is_same_model = assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model == model.id;
                let has_tool_calls = assistant.content.iter().any(|block| block.type_name() == "toolCall");
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
                                .as_ref()
                                .is_some_and(|signature| !signature.trim().is_empty());
                            if is_same_model && has_usable_signature && (preserve_provider_state || thinking.thinking.trim().is_empty()) {
                                transformed_content.push(block.clone());
                                continue;
                            }
                            if thinking.thinking.trim().is_empty() {
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
                                {
                                    let normalized_id = normalize(&tool_call.id, model, &assistant);
                                    if normalized_id != tool_call.id {
                                        tool_call_id_map.insert(tool_call.id.clone(), normalized_id.clone());
                                        normalized_tool_call.id = normalized_id;
                                    }
                                }
                            }
                            transformed_content.push(ContentBlock::ToolCall(normalized_tool_call));
                        }
                        other => transformed_content.push(other.clone()),
                    }
                }

                let mut assistant = *assistant;
                assistant.content = transformed_content;
                Message::Assistant(Box::new(assistant))
            }
        })
        .collect();

    let mut result: Vec<Message> = Vec::with_capacity(transformed.len());
    let mut tool_results_by_id: HashMap<String, Vec<(ToolResultMessage, usize, bool)>> = HashMap::new();
    let mut next_tool_call_index_by_id: HashMap<String, Vec<usize>> = HashMap::new();
    let mut dropped_call_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (source_index, message) in transformed.iter().enumerate() {
        match message {
            Message::ToolResult(result_message) => {
                tool_results_by_id
                    .entry(result_message.tool_call_id.clone())
                    .or_default()
                    .push((result_message.clone(), source_index, false));
                continue;
            }
            Message::Assistant(assistant) => {
                if assistant.stop_reason == crate::types::StopReason::Error
                    || assistant.stop_reason == crate::types::StopReason::Aborted
                {
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
                    .and_then(|entries| entries.iter().find(|(_, index, _)| *index == source_index));
                let consumed = entry.is_some_and(|(_, _, consumed)| *consumed);
                if !consumed
                    && (!dropped_call_ids.contains(&result_message.tool_call_id)
                        || next_tool_call_index_by_id.contains_key(&result_message.tool_call_id))
                {
                    result.push(Message::ToolResult(result_message.clone()));
                }
                continue;
            }
            Message::Assistant(assistant) => {
                if assistant.stop_reason == crate::types::StopReason::Error
                    || assistant.stop_reason == crate::types::StopReason::Aborted
                {
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
                        entries.iter_mut().find(|(_, index, consumed)| {
                            !*consumed && *index > source_index && *index < next_declaration_index
                        })
                    });

                    if let Some((matched_message, _, consumed)) = matched {
                        *consumed = true;
                        result.push(Message::ToolResult(matched_message.clone()));
                        continue;
                    }

                    result.push(Message::ToolResult(ToolResultMessage {
                        tool_call_id: tool_call.id.clone(),
                        tool_name: tool_call.name.clone(),
                        content: vec![ContentBlock::text("No result provided")],
                        details: None,
                        usage: None,
                        added_tool_names: None,
                        is_error: true,
                        timestamp: now_ms(),
                    }));
                }
            }
            other => result.push(other.clone()),
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ImageContent, InputModality, ProviderId, StopReason, ThinkingContent, ToolCall, Usage};

    fn any_model() -> Model {
        crate::models_generated::get_builtin_provider_models("anthropic").expect("models")[0].clone()
    }

    fn model_with_input(input: Vec<InputModality>) -> Model {
        let mut model = any_model();
        model.input = input;
        model
    }

    fn text_model() -> Model {
        model_with_input(vec![InputModality::Text])
    }

    fn assistant_message(content: Vec<ContentBlock>, stop_reason: StopReason) -> Message {
        Message::Assistant(Box::new(AssistantMessage {
            content,
            api: "anthropic-messages".into(),
            provider: ProviderId::from("anthropic"),
            model: String::new(),
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
        }))
    }

    fn user_image_message() -> Message {
        Message::User(crate::types::UserMessage {
            content: UserContent::Blocks(vec![
                ContentBlock::text("hi"),
                ContentBlock::Image(ImageContent { data: "AA==".into(), mime_type: "image/png".into() }),
            ]),
            timestamp: 0,
        })
    }

    #[test]
    fn collapses_consecutive_unsupported_images_into_one_placeholder() {
        let messages = vec![Message::User(crate::types::UserMessage {
            content: UserContent::Blocks(vec![
                ContentBlock::Image(ImageContent { data: "AA==".into(), mime_type: "image/png".into() }),
                ContentBlock::Image(ImageContent { data: "AA==".into(), mime_type: "image/png".into() }),
                ContentBlock::text("after"),
            ]),
            timestamp: 0,
        })];
        let transformed = transform_messages(&messages, &text_model(), None, &TransformMessagesOptions::default());
        let Message::User(user) = &transformed[0] else { panic!("user") };
        let UserContent::Blocks(blocks) = &user.content else { panic!("blocks") };
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], ContentBlock::text(NON_VISION_USER_IMAGE_PLACEHOLDER));
        assert_eq!(blocks[1], ContentBlock::text("after"));
    }

    #[test]
    fn video_placeholder_precedes_the_image_placeholder() {
        let messages = vec![Message::User(crate::types::UserMessage {
            content: UserContent::Blocks(vec![ContentBlock::Image(ImageContent {
                data: "AA==".into(),
                mime_type: "video/mp4".into(),
            })]),
            timestamp: 0,
        })];
        let transformed = transform_messages(&messages, &text_model(), None, &TransformMessagesOptions::default());
        let Message::User(user) = &transformed[0] else { panic!("user") };
        let UserContent::Blocks(blocks) = &user.content else { panic!("blocks") };
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], ContentBlock::text(NO_VIDEO_USER_PLACEHOLDER));
    }

    #[test]
    fn keeps_images_for_vision_models() {
        let messages = vec![user_image_message()];
        let model = model_with_input(vec![InputModality::Text, InputModality::Image]);
        let transformed = transform_messages(&messages, &model, None, &TransformMessagesOptions::default());
        let Message::User(user) = &transformed[0] else { panic!("user") };
        let UserContent::Blocks(blocks) = &user.content else { panic!("blocks") };
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn cross_model_thinking_becomes_text_and_same_model_signed_thinking_is_kept() {
        let thinking = ContentBlock::Thinking(ThinkingContent {
            thinking: "reasoning".into(),
            thinking_signature: Some("sig".into()),
            ..ThinkingContent::default()
        });
        let mut model = text_model();
        model.provider = "anthropic".into();
        model.api = "anthropic-messages".into();
        model.id = String::new();

        let same = transform_messages(
            &[assistant_message(vec![thinking.clone()], StopReason::Stop)],
            &model,
            None,
            &TransformMessagesOptions::default(),
        );
        let Message::Assistant(assistant) = &same[0] else { panic!("assistant") };
        assert_eq!(assistant.content[0], thinking);

        let mut other = model.clone();
        other.id = "other".into();
        let cross = transform_messages(
            &[assistant_message(vec![thinking], StopReason::Stop)],
            &other,
            None,
            &TransformMessagesOptions::default(),
        );
        let Message::Assistant(assistant) = &cross[0] else { panic!("assistant") };
        assert_eq!(assistant.content[0], ContentBlock::text("reasoning"));
    }

    #[test]
    fn pairs_tool_calls_with_the_earliest_unconsumed_result_and_synthesizes_missing_ones() {
        let call = ContentBlock::ToolCall(ToolCall {
            id: "c1".into(),
            name: "t".into(),
            arguments: serde_json::Map::new(),
            incomplete: None,
            error_message: None,
            thought_signature: None,
            namespace: None,
        });
        let messages = vec![
            assistant_message(vec![call.clone()], StopReason::ToolUse),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "c1".into(),
                tool_name: "t".into(),
                content: vec![ContentBlock::text("ok")],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: false,
                timestamp: 0,
            }),
        ];
        let transformed = transform_messages(&messages, &text_model(), None, &TransformMessagesOptions::default());
        assert_eq!(transformed.len(), 2);
        assert!(matches!(transformed[1], Message::ToolResult(_)));

        let orphan = transform_messages(
            &[assistant_message(vec![call], StopReason::ToolUse)],
            &text_model(),
            None,
            &TransformMessagesOptions::default(),
        );
        let Message::ToolResult(result) = &orphan[1] else { panic!("tool result") };
        assert_eq!(result.content[0], ContentBlock::text("No result provided"));
        assert!(result.is_error);
    }

    #[test]
    fn drops_errored_assistant_turns_and_their_orphan_results() {
        let call = ContentBlock::ToolCall(ToolCall {
            id: "c1".into(),
            name: "t".into(),
            arguments: serde_json::Map::new(),
            incomplete: None,
            error_message: None,
            thought_signature: None,
            namespace: None,
        });
        let messages = vec![
            assistant_message(vec![call], StopReason::Error),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "c1".into(),
                tool_name: "t".into(),
                content: vec![ContentBlock::text("late")],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: false,
                timestamp: 0,
            }),
        ];
        let transformed = transform_messages(&messages, &text_model(), None, &TransformMessagesOptions::default());
        assert!(transformed.is_empty());
    }
}
