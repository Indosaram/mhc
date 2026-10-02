use serde_json::Value;
use crate::todo_types::TodoPhase;
pub fn native_todo_updates(message:&Value)->Vec<Vec<TodoPhase>> {
    if message.get("role").and_then(Value::as_str)!=Some("assistant") { return vec![]; }
    let Some(content)=message.get("content").and_then(Value::as_array) else { return vec![]; };
    content.iter().filter(|block|block.get("type").and_then(Value::as_str)==Some("toolCall") && block.get("name").and_then(Value::as_str)==Some("todo")).filter(|block|!block.get("arguments").and_then(|arguments|arguments.get("op")).is_some_and(js_truthy)).filter_map(|block|crate::native_todo_mirror::phases_from_cursor_todos(block.get("arguments")?.get("todos")?)).collect()
}
fn js_truthy(value:&Value)->bool { match value { Value::Null=>false,Value::Bool(value)=>*value,Value::Number(value)=>value.as_f64().is_some_and(|value|value!=0.0),Value::String(value)=>!value.is_empty(),Value::Array(_)|Value::Object(_)=>true } }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn native_updates_skip_explicit_ops_and_nonassistant_messages() { let message=serde_json::json!({"role":"assistant","content":[{"type":"toolCall","name":"todo","arguments":{"todos":[]}},{"type":"toolCall","name":"todo","arguments":{"op":"view","todos":[]}}]}); assert_eq!(native_todo_updates(&message),vec![vec![]]); assert!(native_todo_updates(&serde_json::json!({"role":"user","content":message["content"]})).is_empty()); }
    #[test] fn empty_op_is_not_an_explicit_operation() { assert_eq!(native_todo_updates(&serde_json::json!({"role":"assistant","content":[{"type":"toolCall","name":"todo","arguments":{"op":"","todos":[]}}]})),vec![vec![]]); }
}
