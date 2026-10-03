use maho_ext_api::{AgentToolResult, ContentBlock};
use serde_json::{Value, json};

pub fn tool_result_is_error(result: &AgentToolResult) -> bool {
    result.details.get("isError").and_then(Value::as_bool) == Some(true)
}

pub fn marshal_tool_result(result: &AgentToolResult) -> Value {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for part in &result.content {
        match part {
            ContentBlock::Text(content) => texts.push(content.text.as_str()),
            ContentBlock::Image(content) => images.push(json!({"mimeType": content.mime_type, "dataBase64": content.data})),
            ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::ProviderNative(_) => {}
        }
    }
    let details = match result.details.as_object() {
        Some(object) if object.is_empty() => None,
        _ => Some(&result.details),
    };
    let has_error = tool_result_is_error(result);
    let mut marshalled = json!({"text": texts.join("\n")});
    if !images.is_empty() || details.is_some() || has_error {
        if let Some(details) = details {
            marshalled["details"] = details.clone();
        }
        marshalled["images"] = Value::Array(images);
        marshalled["hasError"] = Value::Bool(has_error);
    }
    marshalled
}
