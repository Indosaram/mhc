use std::collections::BTreeMap;
use serde_json::{Value, Map, json};
use sha2::{Digest, Sha256};

fn stable(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let sorted: BTreeMap<_, _> = object.iter().collect();
            Value::Object(sorted.into_iter().map(|(key, value)| (key.clone(), stable(value))).collect())
        },
        Value::Array(array) => Value::Array(array.iter().map(stable).collect()),
        _ => value.clone(),
    }
}
pub fn session_sync_digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(&stable(value)).expect("JSON value")))
}
pub fn is_transmitted_message(message: &Value) -> bool {
    match message["role"].as_str() {
        Some("user") => !message["content"].as_array().is_some_and(Vec::is_empty),
        Some("toolResult") => true,
        _ => false,
    }
}
pub fn sent_messages(context: &Value) -> Vec<Value> {
    context["messages"].as_array().into_iter().flatten().filter(|message| is_transmitted_message(message)).cloned().collect()
}
pub fn sent_message_hashes(messages: &[Value]) -> Vec<String> {
    messages.iter().filter(|message| is_transmitted_message(message)).map(|message| {
        let mut projected = Map::new();
        let keys: &[&str] = if message["role"] == "user" { &["role", "content"] } else { &["role", "toolCallId", "toolName", "content"] };
        for key in keys { if let Some(value) = message.get(*key) { projected.insert((*key).into(), value.clone()); } }
        session_sync_digest(&Value::Object(projected))
    }).collect()
}
pub fn sent_hash_prefix_digest(hashes: &[String], count: usize) -> String { session_sync_digest(&json!(&hashes[..count.min(hashes.len())])) }
pub fn build_delta_prompt_blocks(messages: &[Value], custom: &BTreeMap<String, String>) -> Vec<Value> {
    let mut blocks = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if index > 0 { blocks.push(json!({"type":"text","text":"\n\n"})); }
        if message["role"] == "toolResult" {
            let name = crate::tools::map_pi_tool_name(message["toolName"].as_str().expect("tool name"), custom);
            blocks.push(json!({"type":"text","text":format!("Tool result ({name}, id={}):\n",message["toolCallId"].as_str().expect("tool id"))}));
        }
        if message["content"] == "" { blocks.push(json!({"type":"text","text":""})); }
        else { crate::content_blocks::append_sdk_content_blocks(&mut blocks, &message["content"]); }
    }
    blocks
}
pub struct ConfigFingerprint { pub system_prompt_hash: String, pub toolset_hash: String }
pub fn config_fingerprint(options: &Value, context: &Value, auth_lane: &str, account_name: &str) -> ConfigFingerprint {
    let mut prompt = options.get("systemPrompt").cloned().unwrap_or(Value::Null);
    if let Some(text) = prompt.as_str() {
        let generated = regex::Regex::new(r"\nCurrent date: [0-9]{4}-[0-9]{2}-[0-9]{2}\nCurrent working directory: ").expect("date pattern");
        prompt = json!(generated.replacen(text, 1, "\nCurrent date: <session-date>\nCurrent working directory: ").as_ref());
    }
    let mut tools = Map::new();
    tools.insert("tools".into(), options.get("tools").cloned().unwrap_or_else(|| json!([])));
    let mut reasoning = Map::new();
    for key in ["thinking", "effort", "maxThinkingTokens"] { if let Some(value) = options.get(key) { reasoning.insert(key.into(), value.clone()); } }
    tools.insert("reasoning".into(), Value::Object(reasoning));
    let context_tools: Vec<_> = context["tools"].as_array().into_iter().flatten().map(|tool| {
        let mut projection = Map::new();
        for key in ["name", "description", "parameters"] { if let Some(value) = tool.get(key) { projection.insert(key.into(), value.clone()); } }
        Value::Object(projection)
    }).collect();
    tools.insert("contextTools".into(), json!(context_tools));
    for key in ["cwd", "permissionMode", "settingSources", "extraArgs", "pathToClaudeCodeExecutable", "includePartialMessages"] {
        if let Some(value) = options.get(key) { tools.insert(key.into(), value.clone()); }
    }
    tools.insert("authLane".into(), json!(auth_lane)); tools.insert("accountName".into(), json!(account_name));
    tools.insert("hostToolPolicy".into(), json!(crate::tools::HOST_TOOL_POLICY_FINGERPRINT));
    ConfigFingerprint { system_prompt_hash: session_sync_digest(&prompt), toolset_hash: session_sync_digest(&Value::Object(tools)) }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_zero_block_users_are_transient() {
        let messages = vec![json!({"role":"user","content":[]}),json!({"role":"user","content":""}),json!({"role":"user","content":[{"type":"text","text":"   "}]}),json!({"role":"toolResult","toolCallId":"c","toolName":"bash","content":[]})];
        assert_eq!(sent_message_hashes(&messages).len(), 3);
        let filtered = sent_messages(&json!({"messages":messages})); assert_eq!(sent_message_hashes(&messages), sent_message_hashes(&filtered));
    }
    #[test]
    fn timestamps_do_not_change_hashes_but_tool_identity_does() {
        let one = json!({"role":"toolResult","toolCallId":"a","toolName":"bash","content":"ok","timestamp":1});
        let mut two = one.clone(); two["timestamp"] = json!(2); assert_eq!(sent_message_hashes(std::slice::from_ref(&one)), sent_message_hashes(std::slice::from_ref(&two)));
        two["toolCallId"] = json!("b"); assert_ne!(sent_message_hashes(&[one]), sent_message_hashes(&[two]));
    }
    #[test]
    fn delta_preserves_empty_text_images_and_raw_string_blocks() {
        let blocks = build_delta_prompt_blocks(&[json!({"role":"user","content":""}),json!({"role":"toolResult","toolCallId":"c","toolName":"edit","content":["raw",{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]})], &BTreeMap::new());
        assert_eq!(blocks[0], json!({"type":"text","text":""})); assert_eq!(blocks[3]["text"], "raw"); assert_eq!(blocks[4]["source"]["media_type"], "image/png");
    }
    #[test]
    fn date_normalization_preserves_cwd_and_extension_appends() {
        let options = json!({"systemPrompt":"prefix\nCurrent date: 2026-10-01\nCurrent working directory: /repo\nappend"});
        let mut tomorrow = options.clone(); tomorrow["systemPrompt"] = json!("prefix\nCurrent date: 2026-10-02\nCurrent working directory: /repo\nappend");
        let fingerprint = |value: &Value| config_fingerprint(value, &json!({}), "ambient", "default");
        assert_eq!(fingerprint(&options).system_prompt_hash, fingerprint(&tomorrow).system_prompt_hash);
        tomorrow["systemPrompt"] = json!("prefix\nCurrent date: 2026-10-02\nCurrent working directory: /other\nappend");
        assert_ne!(fingerprint(&options).system_prompt_hash, fingerprint(&tomorrow).system_prompt_hash);
    }
    #[test]
    fn object_order_is_stable_and_missing_options_are_not_null() {
        assert_eq!(session_sync_digest(&json!({"z":1,"a":2})), session_sync_digest(&json!({"a":2,"z":1})));
        let baseline = config_fingerprint(&json!({}), &json!({}), "ambient", "a");
        let changed = config_fingerprint(&json!({"effort":null}), &json!({}), "ambient", "a");
        assert_ne!(baseline.toolset_hash, changed.toolset_hash);
    }
}
