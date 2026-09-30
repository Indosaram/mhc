//! Port of senpi packages/ai/src/api/cursor-agent/measure.ts (browser-safe Cursor history
//! serialization).
//!
//! measure.ts carries its own copies of the blob-store, hash and history helpers that
//! `cursor-agent.ts` also defines (the TS file duplicates them deliberately so this module
//! stays browser-safe); the port keeps both, so this module reproduces measure.ts's own
//! non-cryptographic `hashBytes` rather than the `node:crypto` SHA-256 the API file uses.

use serde_json::{json, Map, Value};

use crate::cursor::composer_prompt::{is_cursor_composer_model, CURSOR_COMPOSER_PROMPT};
use crate::types::{AssistantMessage, ContentBlock, Message, ToolCall, ToolResultMessage};

use super::r#gen::agent_pb::{
    conversation_step, AgentConversationTurnStructure, AssistantMessage as PbAssistantMessage, ConversationStep,
    ConversationTurnStructure, McpArgs, McpImageContent, McpSuccess, McpTextContent, McpToolCall, McpToolError,
    McpToolResult, McpToolResultContentItem, SelectedContext, SelectedImage, ToolCall as PbToolCall,
    UserMessage as PbUserMessage,
};
use super::value_pb::value_to_bytes;

pub type BlobStore = std::collections::HashMap<String, Vec<u8>>;

fn hash_bytes(input: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (index, byte) in input.iter().enumerate() {
        let slot = index % 32;
        out[slot] = out[slot].wrapping_add(*byte).wrapping_add(index as u8);
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn digest_hex(data: &str) -> String {
    hex(&hash_bytes(data.as_bytes()))
}

fn format_uuid(hex: &str) -> String {
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

fn deterministic_uuid(seed: &str) -> String {
    format_uuid(&digest_hex(seed))
}

fn random_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn store_cursor_blob(store: &mut BlobStore, data: Vec<u8>) -> Vec<u8> {
    let id = hash_bytes(&data).to_vec();
    store.insert(hex(&id), data);
    id
}

fn read_cursor_blob(store: &BlobStore, blob_id: &[u8]) -> Vec<u8> {
    store.get(&hex(blob_id)).cloned().unwrap_or_else(|| panic!("Cursor blob not found"))
}

pub fn tool_result_to_text(result: &ToolResultMessage) -> String {
    result
        .content
        .iter()
        .map(|item| match item {
            ContentBlock::Text(text) => text.text.clone(),
            ContentBlock::Image(image) => format!("[{} image]", image.mime_type),
            ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::ProviderNative(_) => {
                "[undefined image]".to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_user_message_text(message: &Message) -> String {
    let Message::User(user) = message else { return String::new() };
    match &user.content {
        crate::types::UserContent::Text(text) => text.trim().to_owned(),
        crate::types::UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned(),
    }
}

fn has_user_message_images(message: &Message) -> bool {
    matches!(&message, Message::User(user) if matches!(&user.content, crate::types::UserContent::Blocks(blocks) if blocks.iter().any(|block| matches!(block, ContentBlock::Image(_)))))
}

fn build_cursor_root_prompt_content(content: &crate::types::UserContent) -> Vec<Value> {
    match content {
        crate::types::UserContent::Text(text) => {
            let text = text.trim();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![json!({ "type": "text", "text": text })]
            }
        }
        crate::types::UserContent::Blocks(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match block {
                    ContentBlock::Text(text) => {
                        let text = text.text.trim();
                        if !text.is_empty() {
                            parts.push(json!({ "type": "text", "text": text }));
                        }
                    }
                    ContentBlock::Image(image) => parts.push(json!({
                        "type": "image",
                        "image": format!("data:{};base64,{}", image.mime_type, image.data),
                        "mediaType": image.mime_type,
                    })),
                    ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::ProviderNative(_) => {
                        parts.push(json!({
                            "type": "image",
                            "image": "data:undefined;base64,undefined",
                            "mediaType": Value::Null,
                        }));
                    }
                }
            }
            parts
        }
    }
}

fn cursor_user_content_key(content: &crate::types::UserContent) -> String {
    match content {
        crate::types::UserContent::Text(text) => text.trim().to_owned(),
        crate::types::UserContent::Blocks(blocks) => {
            let mut data = String::new();
            for block in blocks {
                match block {
                    ContentBlock::Text(text) => {
                        data.push_str("text");
                        data.push_str(&text.text);
                    }
                    ContentBlock::Image(image) => {
                        data.push_str("image");
                        data.push_str(&image.mime_type);
                        data.push_str(&image.data);
                    }
                    ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::ProviderNative(_) => {
                        data.push_str("image");
                        data.push_str("undefined");
                        data.push_str("undefined");
                    }
                }
            }
            digest_hex(&data)
        }
    }
}

fn build_cursor_assistant_content(message: &AssistantMessage) -> Vec<Value> {
    let mut content = Vec::new();
    for item in &message.content {
        match item {
            ContentBlock::Text(text) => {
                if !text.text.is_empty() {
                    content.push(json!({ "type": "text", "text": text.text }));
                }
            }
            ContentBlock::ToolCall(call) => content.push(json!({
                "type": "tool-call",
                "toolCallId": call.id,
                "toolName": call.name,
                "args": call.arguments,
            })),
            ContentBlock::Thinking(_) | ContentBlock::Image(_) | ContentBlock::ProviderNative(_) => {}
        }
    }
    content
}

fn find_last_user_message_index(messages: &[Message]) -> Option<usize> {
    messages.iter().rposition(|message| matches!(message, Message::User(_)))
}

pub fn build_cursor_system_prompt_jsons(system_prompt: Option<&str>, model_id: Option<&str>) -> Vec<String> {
    let trimmed = system_prompt.map(str::trim).filter(|prompt| !prompt.is_empty());
    let host = vec![match trimmed {
        Some(prompt) => json!({ "role": "system", "content": prompt }).to_string(),
        None => json!({ "role": "system", "content": "You are a helpful assistant." }).to_string(),
    }];
    match model_id {
        Some(model_id) if is_cursor_composer_model(model_id) => {
            let mut result = vec![json!({ "role": "system", "content": CURSOR_COMPOSER_PROMPT }).to_string()];
            result.extend(host);
            result
        }
        _ => host,
    }
}

fn build_root_prompt_messages_json(
    messages: &[Message],
    system_prompt_ids: &[Vec<u8>],
    store: &mut BlobStore,
    active_user_message_index: Option<usize>,
) -> Vec<Vec<u8>> {
    let mut entries: Vec<Vec<u8>> = system_prompt_ids.to_vec();
    for (index, message) in messages.iter().enumerate() {
        if Some(index) == active_user_message_index {
            break;
        }
        match message {
            Message::User(user) => {
                let content = build_cursor_root_prompt_content(&user.content);
                if content.is_empty() {
                    continue;
                }
                let bytes = json!({ "role": "user", "content": content }).to_string().into_bytes();
                entries.push(store_cursor_blob(store, bytes));
            }
            Message::Assistant(assistant) => {
                let content = build_cursor_assistant_content(assistant);
                if content.is_empty() {
                    continue;
                }
                let bytes = json!({ "role": "assistant", "content": content }).to_string().into_bytes();
                entries.push(store_cursor_blob(store, bytes));
            }
            Message::ToolResult(result) => {
                let mut item = json!({
                    "type": "tool-result",
                    "toolName": result.tool_name,
                    "toolCallId": result.tool_call_id,
                    "result": tool_result_to_text(result),
                });
                if result.is_error {
                    item["isError"] = Value::Bool(true);
                }
                let bytes = json!({
                    "role": "tool",
                    "id": result.tool_call_id,
                    "content": [item],
                })
                .to_string()
                .into_bytes();
                entries.push(store_cursor_blob(store, bytes));
            }
            Message::ConfigurationUpdate(_) => {}
        }
    }
    entries
}

fn is_plain_record(value: &Value) -> bool {
    value.is_object()
}

fn is_json_value(value: &Value) -> bool {
    match value {
        Value::Null | Value::String(_) | Value::Bool(_) => true,
        Value::Number(number) => number.as_f64().is_some_and(f64::is_finite),
        Value::Array(items) => items.iter().all(is_json_value),
        Value::Object(_) => is_plain_record(value) && value.as_object().is_some_and(|map| map.values().all(is_json_value)),
    }
}

fn encode_cursor_mcp_arguments(tool_call: &ToolCall) -> Result<std::collections::HashMap<String, Vec<u8>>, String> {
    let mut encoded = std::collections::HashMap::new();
    for (name, value) in &tool_call.arguments {
        if !is_json_value(value) {
            return Err(format!("Cursor tool argument {}.{} is not JSON-serializable", tool_call.name, name));
        }
        encoded.insert(name.clone(), value_to_bytes(value));
    }
    Ok(encoded)
}

fn base64_decode(data: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(data).unwrap_or_default()
}

fn create_cursor_mcp_result(result: &ToolResultMessage) -> McpToolResult {
    use super::r#gen::agent_pb::mcp_tool_result;
    if result.is_error {
        return McpToolResult {
            result: Some(mcp_tool_result::Result::Error(McpToolError {
                error: tool_result_to_text(result),
                ..McpToolError::default()
            })),
        };
    }
    let content = result
        .content
        .iter()
        .map(|item| match item {
            ContentBlock::Image(image) => McpToolResultContentItem {
                content: Some(super::r#gen::agent_pb::mcp_tool_result_content_item::Content::Image(McpImageContent {
                    data: base64_decode(&image.data).into(),
                    mime_type: image.mime_type.clone(),
                })),
            },
            ContentBlock::Text(text) => McpToolResultContentItem {
                content: Some(super::r#gen::agent_pb::mcp_tool_result_content_item::Content::Text(McpTextContent {
                    text: text.text.clone(),
                    ..McpTextContent::default()
                })),
            },
            ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::ProviderNative(_) => {
                McpToolResultContentItem {
                    content: Some(super::r#gen::agent_pb::mcp_tool_result_content_item::Content::Text(McpTextContent {
                        text: "[undefined image]".to_owned(),
                        ..McpTextContent::default()
                    })),
                }
            }
        })
        .collect();
    McpToolResult {
        result: Some(mcp_tool_result::Result::Success(McpSuccess { content, is_error: false })),
    }
}

fn create_cursor_tool_call_step(
    tool_call: &ToolCall,
    result: Option<&ToolResultMessage>,
) -> Result<ConversationStep, String> {
    let mcp_call = McpToolCall {
        args: Some(McpArgs {
            name: tool_call.name.clone(),
            args: encode_cursor_mcp_arguments(tool_call)?.into_iter().map(|(k, v)| (k, v.into())).collect(),
            tool_call_id: tool_call.id.clone(),
            provider_identifier: "pi-agent".to_owned(),
            tool_name: tool_call.name.clone(),
            ..McpArgs::default()
        }),
        result: result.map(create_cursor_mcp_result),
        ..McpToolCall::default()
    };
    Ok(ConversationStep {
        message: Some(conversation_step::Message::ToolCall(PbToolCall {
            tool: Some(super::r#gen::agent_pb::tool_call::Tool::McpToolCall(mcp_call)),
            tool_call_id: Some(tool_call.id.clone()),
        })),
    })
}

fn build_conversation_turns(
    messages: &[Message],
    store: &mut BlobStore,
    active_user_message_index: Option<usize>,
) -> Result<Vec<Vec<u8>>, String> {
    use prost::Message as _;

    let mut turns = Vec::new();
    let history_end = active_user_message_index.unwrap_or(messages.len());
    let mut tool_results: std::collections::HashMap<String, &ToolResultMessage> = std::collections::HashMap::new();
    let mut paired_tool_call_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for message in messages.iter().take(history_end) {
        match message {
            Message::ToolResult(result) => {
                tool_results.insert(result.tool_call_id.clone(), result);
            }
            Message::Assistant(assistant) => {
                for item in &assistant.content {
                    if let ContentBlock::ToolCall(call) = item {
                        paired_tool_call_ids.insert(call.id.clone());
                    }
                }
            }
            Message::User(_) | Message::ConfigurationUpdate(_) => {}
        }
    }

    let mut index = 0;
    while index < messages.len() {
        let Message::User(user) = &messages[index] else {
            index += 1;
            continue;
        };
        if Some(index) == active_user_message_index {
            break;
        }

        let user_text = extract_user_message_text(&messages[index]);
        if user_text.is_empty() && !has_user_message_images(&messages[index]) {
            index += 1;
            continue;
        }

        let user_message = create_cursor_user_message(
            &user.content,
            &user_text,
            deterministic_uuid(&format!("u:{}:{}", turns.len(), cursor_user_content_key(&user.content))),
        );
        let user_message_blob_id = store_cursor_blob(store, user_message.encode_to_vec());
        let mut step_blob_ids: Vec<Vec<u8>> = Vec::new();
        index += 1;

        while index < messages.len() && !matches!(messages[index], Message::User(_)) {
            match &messages[index] {
                Message::Assistant(assistant) => {
                    for item in &assistant.content {
                        let step = match item {
                            ContentBlock::Text(text) => {
                                if text.text.is_empty() {
                                    continue;
                                }
                                ConversationStep {
                                    message: Some(conversation_step::Message::AssistantMessage(PbAssistantMessage {
                                        text: text.text.clone(),
                                    })),
                                }
                            }
                            ContentBlock::Thinking(_) => continue,
                            ContentBlock::ToolCall(call) => {
                                create_cursor_tool_call_step(call, tool_results.get(&call.id).copied())?
                            }
                            ContentBlock::Image(_) | ContentBlock::ProviderNative(_) => continue,
                        };
                        step_blob_ids.push(store_cursor_blob(store, step.encode_to_vec()));
                    }
                }
                Message::ToolResult(result) if !paired_tool_call_ids.contains(&result.tool_call_id) => {
                    let text = tool_result_to_text(result);
                    if !text.is_empty() {
                        let prefix = if result.is_error { "[Tool Error]" } else { "[Tool Result]" };
                        let step = ConversationStep {
                            message: Some(conversation_step::Message::AssistantMessage(PbAssistantMessage {
                                text: format!("{prefix}\n{text}"),
                            })),
                        };
                        step_blob_ids.push(store_cursor_blob(store, step.encode_to_vec()));
                    }
                }
                Message::User(_) | Message::ToolResult(_) | Message::ConfigurationUpdate(_) => {}
            }
            index += 1;
        }

        let agent_turn = AgentConversationTurnStructure {
            user_message: user_message_blob_id.into(),
            steps: step_blob_ids.into_iter().map(Into::into).collect(),
            request_id: None,
        };
        let turn = ConversationTurnStructure {
            turn: Some(super::r#gen::agent_pb::conversation_turn_structure::Turn::AgentConversationTurn(agent_turn)),
        };
        turns.push(store_cursor_blob(store, turn.encode_to_vec()));
    }

    Ok(turns)
}

/// Result of [`build_cursor_history_for_test`], matching the TS return shape.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorHistoryForTest {
    pub root_prompt_messages_json: Vec<Value>,
    pub turn_user_messages_json: Vec<Value>,
    pub turn_step_messages_json: Vec<Vec<Value>>,
}

/// Exported for tests: decodes Cursor history blobs built from conversation messages.
pub fn build_cursor_history_for_test(
    messages: &[Message],
    active_user_message_index: Option<usize>,
) -> Result<CursorHistoryForTest, String> {
    use prost::Message as _;

    let active = active_user_message_index.or_else(|| find_last_user_message_index(messages));
    let mut store = BlobStore::new();
    let root_prompt_messages_json = build_root_prompt_messages_json(messages, &[], &mut store, active)
        .into_iter()
        .map(|blob_id| {
            let bytes = read_cursor_blob(&store, &blob_id);
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        })
        .collect();

    let mut turn_user_messages_json = Vec::new();
    let mut turn_step_messages_json = Vec::new();
    for turn_blob_id in build_conversation_turns(messages, &mut store, active)? {
        let turn = ConversationTurnStructure::decode(read_cursor_blob(&store, &turn_blob_id).as_slice())
            .map_err(|error| error.to_string())?;
        let Some(super::r#gen::agent_pb::conversation_turn_structure::Turn::AgentConversationTurn(turn)) = turn.turn
        else {
            continue;
        };
        let user_message = PbUserMessage::decode(read_cursor_blob(&store, &turn.user_message).as_slice())
            .map_err(|error| error.to_string())?;
        turn_user_messages_json.push(user_message_to_json(&user_message));
        turn_step_messages_json.push(
            turn.steps
                .iter()
                .map(|step_blob_id| {
                    ConversationStep::decode(read_cursor_blob(&store, step_blob_id).as_slice())
                        .map(|step| conversation_step_to_json(&step))
                        .unwrap_or(Value::Null)
                })
                .collect(),
        );
    }
    Ok(CursorHistoryForTest { root_prompt_messages_json, turn_user_messages_json, turn_step_messages_json })
}

fn create_cursor_user_message(
    content: &crate::types::UserContent,
    text: &str,
    message_id: String,
) -> PbUserMessage {
    let images = match content {
        crate::types::UserContent::Text(_) => Vec::new(),
        crate::types::UserContent::Blocks(blocks) => extract_images(blocks),
    };
    PbUserMessage {
        text: text.to_owned(),
        message_id,
        selected_context: if images.is_empty() {
            None
        } else {
            Some(SelectedContext { selected_images: images, ..SelectedContext::default() })
        },
        ..PbUserMessage::default()
    }
}

fn extract_images(blocks: &[ContentBlock]) -> Vec<SelectedImage> {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image(image) => Some(SelectedImage {
                uuid: random_uuid(),
                mime_type: image.mime_type.clone(),
                data_or_blob_id: Some(super::r#gen::agent_pb::selected_image::DataOrBlobId::Data(
                    base64_decode(&image.data).into(),
                )),
                ..SelectedImage::default()
            }),
            _ => None,
        })
        .collect()
}

/// `toJson(UserMessageSchema, message)` for the fields this module can produce.
fn user_message_to_json(message: &PbUserMessage) -> Value {
    let mut object = Map::new();
    object.insert("text".to_owned(), Value::String(message.text.clone()));
    object.insert("messageId".to_owned(), Value::String(message.message_id.clone()));
    if let Some(context) = &message.selected_context
        && !context.selected_images.is_empty()
    {
        let images: Vec<Value> = context
            .selected_images
            .iter()
            .map(|image| {
                json!({
                    "uuid": image.uuid,
                    "mimeType": image.mime_type,
                    "data": base64_encode(image.data_or_blob_id.as_ref().and_then(|data| match data {
                        super::r#gen::agent_pb::selected_image::DataOrBlobId::Data(bytes) => Some(bytes.as_ref()),
                        _ => None,
                    }).unwrap_or_default()),
                })
            })
            .collect();
        object.insert("selectedContext".to_owned(), json!({ "selectedImages": images }));
    }
    if message.mode != 0 {
        object.insert("mode".to_owned(), Value::from(message.mode));
    }
    Value::Object(object)
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `toJson(ConversationStepSchema, step)` for the variants this module builds.
fn conversation_step_to_json(step: &ConversationStep) -> Value {
    match &step.message {
        Some(conversation_step::Message::AssistantMessage(message)) => {
            json!({ "assistantMessage": { "text": message.text } })
        }
        Some(conversation_step::Message::ThinkingMessage(message)) => {
            json!({ "thinkingMessage": { "text": message.text, "durationMs": message.duration_ms } })
        }
        Some(conversation_step::Message::ToolCall(tool_call)) => {
            json!({ "toolCall": tool_call_to_json(tool_call) })
        }
        None => Value::Object(Map::new()),
    }
}

fn tool_call_to_json(tool_call: &PbToolCall) -> Value {
    let mut object = Map::new();
    if let Some(id) = &tool_call.tool_call_id {
        object.insert("toolCallId".to_owned(), Value::String(id.clone()));
    }
    if let Some(super::r#gen::agent_pb::tool_call::Tool::McpToolCall(mcp_call)) = &tool_call.tool {
        let mut mcp = Map::new();
        if let Some(args) = &mcp_call.args {
            let mut encoded = Map::new();
            for (key, value) in &args.args {
                encoded.insert(key.clone(), Value::String(base64_encode(value)));
            }
            mcp.insert(
                "args".to_owned(),
                json!({
                    "name": args.name,
                    "args": Value::Object(encoded),
                    "toolCallId": args.tool_call_id,
                    "providerIdentifier": args.provider_identifier,
                    "toolName": args.tool_name,
                }),
            );
        }
        if let Some(result) = &mcp_call.result {
            mcp.insert("result".to_owned(), mcp_result_to_json(result));
        }
        object.insert("mcpToolCall".to_owned(), Value::Object(mcp));
    }
    Value::Object(object)
}

fn mcp_result_to_json(result: &McpToolResult) -> Value {
    match &result.result {
        Some(super::r#gen::agent_pb::mcp_tool_result::Result::Success(success)) => {
            let content: Vec<Value> = success
                .content
                .iter()
                .map(|item| match &item.content {
                    Some(super::r#gen::agent_pb::mcp_tool_result_content_item::Content::Text(text)) => {
                        json!({ "text": { "text": text.text } })
                    }
                    Some(super::r#gen::agent_pb::mcp_tool_result_content_item::Content::Image(image)) => {
                        json!({ "image": { "data": base64_encode(&image.data), "mimeType": image.mime_type } })
                    }
                    None => Value::Object(Map::new()),
                })
                .collect();
            json!({ "success": { "content": content, "isError": success.is_error } })
        }
        Some(super::r#gen::agent_pb::mcp_tool_result::Result::Error(error)) => {
            json!({ "error": { "error": error.error } })
        }
        _ => Value::Object(Map::new()),
    }
}

/// Serialized byte cost of the MODEL INPUT alone: the `rootPromptMessagesJson`
/// blobs Cursor's server replays as the prompt.
pub fn measure_cursor_model_input_serialized_bytes(
    messages: &[Message],
    active_user_message_index: Option<usize>,
) -> usize {
    let active = active_user_message_index.or_else(|| find_last_user_message_index(messages));
    let mut store = BlobStore::new();
    build_root_prompt_messages_json(messages, &[], &mut store, active)
        .iter()
        .map(|blob_id| read_cursor_blob(&store, blob_id).len())
        .sum()
}

pub fn measure_cursor_history_serialized_bytes(
    messages: &[Message],
    active_user_message_index: Option<usize>,
) -> Result<usize, String> {
    Ok(build_cursor_history_wire_bytes_for_test(messages, active_user_message_index)?.iter().map(Vec::len).sum())
}

pub fn build_cursor_history_wire_bytes_for_test(
    messages: &[Message],
    active_user_message_index: Option<usize>,
) -> Result<Vec<Vec<u8>>, String> {
    let active = active_user_message_index.or_else(|| find_last_user_message_index(messages));
    let mut store = BlobStore::new();
    build_root_prompt_messages_json(messages, &[], &mut store, active);
    build_conversation_turns(messages, &mut store, active)?;
    let mut values: Vec<Vec<u8>> = store.into_values().collect();
    values.sort();
    Ok(values)
}
