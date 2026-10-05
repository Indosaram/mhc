use super::{registry::JsonRpcError, turn_log::TurnLog};
use serde_json::{Value, json};

pub struct ParsedInput {
    pub text: String,
    pub content: Vec<Value>,
}
pub fn parse_input(input: &[Value]) -> Result<ParsedInput, JsonRpcError> {
    if input.is_empty() { return Err(JsonRpcError::new(-32602,"Invalid params: input must include at least one text item")); }
    let mut text = Vec::new(); let mut content = Vec::new();
    for item in input {
        match item["type"].as_str() {
            Some("text") => {
                let value = item["text"].as_str().ok_or_else(|| JsonRpcError::new(-32602,"Invalid params: text input must not be empty"))?;
                if maho_ai::utils::js::trim(value).is_empty() { return Err(JsonRpcError::new(-32602,"Invalid params: text input must not be empty")); }
                text.push(value);
                content.push(json!({"type":"text","text":value,"text_elements":item.get("text_elements").filter(|value| !value.is_null()).cloned().unwrap_or_else(||json!([]))}));
            },
            Some(kind @ ("image"|"localImage"|"skill"|"mention")) => return Err(JsonRpcError::new(-32602,format!("Invalid params: unsupported input item type {kind}"))),
            _ => return Err(JsonRpcError::new(-32602,"Invalid params: unknown input item type")),
        }
    }
    Ok(ParsedInput { text:text.join("\n"), content })
}
pub fn create_turn_id() -> String { uuid::Uuid::new_v4().to_string() }
pub fn build_turn(id: &str, status: &str, started_ms: f64, completed_ms: Option<f64>, items: &[Value], message: Option<&str>) -> Value {
    json!({"id":id,"items":items,"itemsView":"full","status":status,"error":if status == "failed" { json!({"message":message.unwrap_or("Turn failed"),"codexErrorInfo":"other","additionalDetails":null}) } else { Value::Null },"startedAt":started_ms / 1000.0,"completedAt":completed_ms.map(|time| time / 1000.0),"durationMs":completed_ms.map(|time| time - started_ms)})
}
pub fn build_user_message(client_id: Option<&str>, content: &[Value]) -> Value {
    json!({"type":"userMessage","id":client_id.map(str::to_owned).unwrap_or_else(create_turn_id),"clientId":client_id,"content":content})
}
pub fn read_logged_items(log: &mut TurnLog, thread_id: &str, turn_id: &str) -> Vec<Value> {
    log.read_turns(thread_id).into_iter().find(|turn| turn.turn_id == turn_id).map(|turn| turn.items.into_iter().map(Value::Object).collect()).unwrap_or_default()
}
