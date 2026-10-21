use std::sync::Arc;
use maho_ai::utils::assistant_message_frame::AssistantMessageFrame;
use super::super::types::{Error, JsonObject, JsonValue, Result, to_stored};

pub fn apply_frame(output: &mut JsonObject, frame: &AssistantMessageFrame) -> Result<()> {
    use AssistantMessageFrame as F;
    if let F::Start { partial } = frame {
        output.insert("message".into(), to_stored(partial)?.as_json().clone());
        return Ok(());
    }
    let mut message = output.get("message").cloned().ok_or_else(|| Error::InvalidShape(format!("{} before start", frame.type_name())))?;
    let JsonValue::Object(ref mut fields) = message else { return Err(Error::InvalidShape("message is not an object".into())); };
    let content = Arc::make_mut(fields).iter_mut().find(|(key, _)| key == "content").map(|(_, value)| value)
        .ok_or_else(|| Error::InvalidShape("message has no content".into()))?;
    let JsonValue::Array(blocks) = content else { return Err(Error::InvalidShape("message content is not an array".into())); };
    let blocks = Arc::make_mut(blocks);
    match frame {
        F::Start { .. } => unreachable!(),
        F::TextStart { content_index, content } => put(blocks, *content_index, to_stored(&maho_ai::types::ContentBlock::Text(content.clone()))?.as_json().clone())?,
        F::ThinkingStart { content_index, content } => put(blocks, *content_index, to_stored(&maho_ai::types::ContentBlock::Thinking(content.clone()))?.as_json().clone())?,
        F::ToolcallStart { content_index, tool_call } => put(blocks, *content_index, to_stored(&maho_ai::types::ContentBlock::ToolCall(tool_call.clone()))?.as_json().clone())?,
        F::TextDelta { content_index, delta } => append(blocks, *content_index, "text", delta)?,
        F::ThinkingDelta { content_index, delta } => append(blocks, *content_index, "thinking", delta)?,
        F::TextEnd { content_index, content, text_signature } => {
            let mut block = block(blocks, *content_index)?;
            block.insert("text".into(), JsonValue::String(content.as_str().into()));
            block.remove("textSignature");
            if let Some(signature) = text_signature { block.insert("textSignature".into(), JsonValue::String(signature.as_str().into())); }
            put(blocks, *content_index, JsonValue::object(block.into_inner()))?;
        }
        F::ThinkingEnd { content_index, content, thinking_signature, redacted } => {
            let mut block = block(blocks, *content_index)?;
            block.insert("thinking".into(), JsonValue::String(content.as_str().into()));
            block.remove("thinkingSignature");
            block.remove("redacted");
            if let Some(signature) = thinking_signature { block.insert("thinkingSignature".into(), JsonValue::String(signature.as_str().into())); }
            if let Some(redacted) = redacted { block.insert("redacted".into(), JsonValue::Bool(*redacted)); }
            put(blocks, *content_index, JsonValue::object(block.into_inner()))?;
        }
        F::ToolcallCheckpoint { content_index, json } => {
            let parsed = match serde_json::from_str::<JsonValue>(json) {
                Ok(value) => value,
                Err(_) => JsonValue::object(Vec::new()),
            };
            let mut block = block(blocks, *content_index)?;
            block.insert("arguments".into(), parsed);
            put(blocks, *content_index, JsonValue::object(block.into_inner()))?;
        }
        F::ToolcallDelta { .. } => {}
        F::ToolcallEnd { content_index, id, name, arguments, thought_signature, namespace } => {
            let mut block = block(blocks, *content_index)?;
            block.insert("id".into(), JsonValue::String(id.as_str().into()));
            block.insert("name".into(), JsonValue::String(name.as_str().into()));
            block.insert("arguments".into(), to_stored(arguments)?.as_json().clone());
            if let Some(signature) = thought_signature { block.insert("thoughtSignature".into(), JsonValue::String(signature.as_str().into())); }
            if let Some(namespace) = namespace { block.insert("namespace".into(), JsonValue::String(namespace.as_str().into())); }
            put(blocks, *content_index, JsonValue::object(block.into_inner()))?;
        }
    }
    output.insert("message".into(), message);
    Ok(())
}

fn put(blocks: &mut Vec<JsonValue>, index: usize, value: JsonValue) -> Result<()> {
    if index == blocks.len() { blocks.push(value); return Ok(()); }
    let target = blocks.get_mut(index).ok_or_else(|| Error::InvalidShape(format!("missing content block {index}")))?;
    *target = value;
    Ok(())
}

fn block(blocks: &[JsonValue], index: usize) -> Result<JsonObject> {
    match blocks.get(index) {
        Some(JsonValue::Object(value)) => Ok(JsonObject::from((**value).clone())),
        _ => Err(Error::InvalidShape(format!("missing content object {index}"))),
    }
}

fn append(blocks: &mut [JsonValue], index: usize, field: &str, delta: &str) -> Result<()> {
    let mut value = block(blocks, index)?;
    let Some(JsonValue::String(text)) = value.get(field) else { return Err(Error::InvalidShape(format!("missing string {field}"))); };
    let mut units = text.units();
    units.extend(delta.encode_utf16());
    value.insert(field.into(), JsonValue::String(super::super::types::JsString::from_utf16(units)));
    let target = blocks.get_mut(index).ok_or_else(|| Error::InvalidShape(format!("missing content block {index}")))?;
    *target = JsonValue::object(value.into_inner());
    Ok(())
}

#[cfg(test)]
#[path = "frames_tests.rs"]
mod tests;
