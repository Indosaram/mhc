//! Inbound bounds and session-entry shape validation.
use serde_json::Value;
pub const MAX_RPC_MESSAGE_CHARACTERS: usize = 1_000_000;
pub const SESSION_CONTEXT_KEYS: usize = 32;
pub const SESSION_CONTEXT_VALUE_BYTES: usize = 16 * 1024;
pub const SESSION_CONTEXT_TOTAL_BYTES: usize = 32 * 1024;

fn string(value: &Value, key: &str) -> bool { value.get(key).is_some_and(Value::is_string) }
fn valid_content(content: &Value) -> bool {
    content.is_string() || content.as_array().is_some_and(|items| items.iter().all(|item| match item.get("type").and_then(Value::as_str) {
        Some("text") => string(item,"text"),
        Some("image") => string(item,"data") && string(item,"mimeType"),
        _ => false,
    }))
}
fn valid_entry(entry: &Value) -> bool {
    if !entry.is_object() || !string(entry,"id") || !string(entry,"timestamp") || !entry.get("parentId").is_some_and(|x| x.is_null() || x.is_string()) { return false; }
    match entry.get("type").and_then(Value::as_str) {
        Some("message") => entry.get("message").is_some_and(|m| matches!(m.get("role").and_then(Value::as_str),Some("user"|"assistant"|"tool")) && m.get("content").is_some_and(valid_content)),
        Some("thinking_level_change") => string(entry,"thinkingLevel"),
        Some("model_change") => string(entry,"provider") && string(entry,"modelId"),
        Some("model_change_rejected") => string(entry,"provider") && string(entry,"modelId") && string(entry,"detail"),
        Some("compaction") => string(entry,"summary") && string(entry,"firstKeptEntryId") && entry.get("tokensBefore").is_some_and(Value::is_number),
        Some("branch_summary") => string(entry,"fromId") && string(entry,"summary"),
        Some("custom") => string(entry,"customType"),
        Some("custom_message") => string(entry,"customType") && entry.get("content").is_some_and(valid_content) && entry.get("display").is_some_and(Value::is_boolean),
        Some("label") => string(entry,"targetId") && entry.get("label").is_none_or(Value::is_string),
        Some("session_info") => entry.get("name").is_none_or(Value::is_string),
        _ => false,
    }
}
pub fn rpc_command_shape_error(command: &Value) -> Option<&'static str> {
    (!command.is_object()).then_some("RPC command must be a JSON object.")
}
pub fn rpc_command_payload_error(command: &Value) -> Option<&'static str> {
    match command.get("type").and_then(Value::as_str) {
        Some("append_user_message") if !command.get("content").is_some_and(valid_content) => Some("append_user_message content must be a string or text/image content array."),
        Some("append_session_entry") if !command.get("entry").is_some_and(valid_entry) => Some("append_session_entry entry is malformed."),
        _ => None,
    }
}
pub fn rpc_message_length_error(command: &Value) -> Option<String> {
    let kind = command.get("type")?.as_str()?;
    if !matches!(kind,"prompt"|"steer"|"follow_up") { return None; }
    let message = command.get("message")?.as_str()?;
    (message.encode_utf16().count() > MAX_RPC_MESSAGE_CHARACTERS).then(|| format!("RPC {kind} message exceeds {MAX_RPC_MESSAGE_CHARACTERS} characters."))
}
pub fn session_context_error(context: Option<&Value>) -> Option<String> {
    let context = context?;
    let Some(entries) = context.as_object() else { return Some("context must be an object of string values.".into()); };
    if entries.len() > SESSION_CONTEXT_KEYS { return Some(format!("context has {} keys, at most {SESSION_CONTEXT_KEYS} are accepted.",entries.len())); }
    for (key,value) in entries {
        let mut chars = key.chars();
        if !chars.next().is_some_and(|c| c.is_ascii_lowercase()) || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Some(format!("context key \"{key}\" must match ^[a-z][a-z0-9_]*$."));
        }
        let Some(text) = value.as_str() else { return Some(format!("context value for key \"{key}\" must be a string.")); };
        if text.len() > SESSION_CONTEXT_VALUE_BYTES { return Some(format!("context value for key \"{key}\" is {} bytes, at most {SESSION_CONTEXT_VALUE_BYTES} bytes are accepted.",text.len())); }
    }
    let total = context.to_string().len();
    (total > SESSION_CONTEXT_TOTAL_BYTES).then(|| format!("context is {total} bytes of JSON, at most {SESSION_CONTEXT_TOTAL_BYTES} bytes are accepted."))
}
pub fn session_kind_error(kind: Option<&Value>) -> Option<&'static str> {
    kind.filter(|v| !matches!(v.as_str(),Some("interactive"|"worker"))).map(|_| "kind must be \"interactive\" or \"worker\".")
}
pub fn session_auto_title_error(value: Option<&Value>) -> Option<&'static str> {
    value.filter(|v| !v.is_boolean()).map(|_| "auto_title must be a boolean.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn rejects_nonobject_input() { for input in [Value::Null,json!([]),json!(42),json!("str"),json!(true)] { assert!(rpc_command_shape_error(&input).is_some()); } }
    #[test] fn accepts_object_input() { assert_eq!(rpc_command_shape_error(&json!({"type":"get_commands"})),None); }
    #[test] fn rejects_oversized_prompt_text() { for kind in ["prompt","steer","follow_up"] { assert_eq!(rpc_message_length_error(&json!({"type":kind,"message":"x".repeat(MAX_RPC_MESSAGE_CHARACTERS+1)})),Some(format!("RPC {kind} message exceeds {MAX_RPC_MESSAGE_CHARACTERS} characters."))); } }
    #[test] fn accepts_refused_model_entry_only_with_detail() { let mut entry = json!({"id":"entry-1","parentId":null,"timestamp":"fixed","type":"model_change_rejected","provider":"faux","modelId":"too-small","detail":"budget"}); assert_eq!(rpc_command_payload_error(&json!({"type":"append_session_entry","entry":entry})),None); entry.as_object_mut().unwrap().remove("detail"); assert!(rpc_command_payload_error(&json!({"type":"append_session_entry","entry":entry})).is_some()); }
    #[test] fn accepts_maximum_and_ignores_nonmessage_command() { assert_eq!(rpc_message_length_error(&json!({"type":"prompt","message":"x".repeat(MAX_RPC_MESSAGE_CHARACTERS)})),None); assert_eq!(rpc_message_length_error(&json!({"type":"get_commands"})),None); }
    #[test] fn context_counts_utf8_bytes() { assert!(session_context_error(Some(&json!({"label":"한".repeat(6000)}))).unwrap().contains("18000 bytes")); }
    #[test] fn context_preserves_absent_versus_null() { assert_eq!(session_context_error(None),None); assert!(session_context_error(Some(&Value::Null)).is_some()); }
    #[test] fn context_rejects_invalid_keys() { assert!(session_context_error(Some(&json!({"Bad":"ok"}))).is_some()); assert_eq!(session_context_error(Some(&json!({"good_2":"ok"}))),None); }
    #[test] fn kind_rejects_unknown_and_null() { assert!(session_kind_error(Some(&json!("typo"))).is_some()); assert!(session_kind_error(Some(&Value::Null)).is_some()); assert_eq!(session_kind_error(None),None); }
    #[test] fn title_preserves_false_and_omitted() { assert_eq!(session_auto_title_error(Some(&json!(false))),None); assert_eq!(session_auto_title_error(None),None); assert!(session_auto_title_error(Some(&json!(0))).is_some()); }
}
