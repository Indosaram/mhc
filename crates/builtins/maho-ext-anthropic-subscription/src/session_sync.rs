use std::collections::BTreeMap;
use serde_json::{Value, Map, json};
use sha2::{Digest, Sha256};

/// Writes `value` with object keys sorted, byte-identical to `serde_json::to_vec` of the key-sorted
/// tree, without cloning the tree first.
fn write_stable(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Object(object) => {
            let sorted: BTreeMap<_, _> = object.iter().collect();
            out.push(b'{');
            for (index, (key, value)) in sorted.into_iter().enumerate() {
                if index > 0 { out.push(b','); }
                serde_json::to_writer(&mut *out, key).expect("JSON key"); out.push(b':'); write_stable(value, out);
            }
            out.push(b'}');
        },
        Value::Array(array) => {
            out.push(b'[');
            for (index, value) in array.iter().enumerate() { if index > 0 { out.push(b','); } write_stable(value, out); }
            out.push(b']');
        },
        _ => serde_json::to_writer(&mut *out, value).expect("JSON value"),
    }
}
fn js_stable_digest(value: &Value) -> String {
    let mut bytes = Vec::new();
    write_stable(value, &mut bytes);
    format!("{:x}", Sha256::digest(bytes))
}
pub fn session_sync_digest(value: &Value) -> String {
    js_stable_digest(value)
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
        for key in keys { if let Some(value) = message.get(*key) { projected.insert((*key).into(), if *key == "content" { image_neutral_content(value) } else { value.clone() }); } }
        session_sync_digest(&Value::Object(projected))
    }).collect()
}
fn image_neutral_content(content: &Value) -> Value {
    let Some(blocks) = content.as_array() else { return content.clone(); };
    let mut normalized = Vec::new();
    let mut image_ids = Vec::new();
    for block in blocks {
        if block["type"] == "image" {
            let id = format!("{:x}",Sha256::digest(block["data"].as_str().unwrap_or_default().as_bytes()));
            image_ids.push(id[..12].to_owned());
            normalized.push(json!({"type":"image","id":id[..12].to_owned()}));
        } else if block["type"] == "text" && block["text"].as_str().is_some_and(|text|text.starts_with("[Image elided:")) {
            if let Some((_, suffix)) = block["text"].as_str().and_then(|text|text.split_once(" (image id: ").or_else(||text.split_once(" (image ids: "))) {
                for id in suffix.trim_end_matches(')').split(", ") {
                    image_ids.push(id.to_owned());
                    normalized.push(json!({"type":"image","id":id}));
                }
            }
        } else { normalized.push(block.clone()); }
    }
    if image_ids.is_empty() { return content.clone(); }
    Value::Array(normalized)
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
    let prompt = options.get("systemPrompt").cloned().unwrap_or(Value::Null);
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
    for key in ["cwd", "permissionMode", "settingSources", "settings", "extraArgs", "pathToClaudeCodeExecutable", "includePartialMessages", "longContextRefused"] {
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
    fn image_elision_preserves_sent_message_hash() {
        let image = json!({"role":"user","content":[{"type":"text","text":"before"},{"type":"image","data":"image-data"},{"type":"text","text":"after"}]});
        let id = format!("{:x}",Sha256::digest(b"image-data"));
        let elided = json!({"role":"user","content":[{"type":"text","text":"before"},{"type":"text","text":format!("[Image elided: an older image was removed to keep the request within the provider's size limit.] (image id: {})",&id[..12])},{"type":"text","text":"after"}]});
        assert_eq!(sent_message_hashes(&[image]), sent_message_hashes(&[elided]));
    }
    #[test]
    fn delta_preserves_empty_text_images_and_raw_string_blocks() {
        let blocks = build_delta_prompt_blocks(&[json!({"role":"user","content":""}),json!({"role":"toolResult","toolCallId":"c","toolName":"edit","content":["raw",{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]})], &BTreeMap::new());
        assert_eq!(blocks[0], json!({"type":"text","text":""})); assert_eq!(blocks[3]["text"], "raw"); assert_eq!(blocks[4]["source"]["media_type"], "image/png");
    }
    #[test]
    fn system_prompt_fingerprint_preserves_verbatim_content() {
        let options = json!({"systemPrompt":"prefix\nCurrent date: 2026-10-01\nCurrent working directory: /repo\nappend"});
        let mut tomorrow = options.clone(); tomorrow["systemPrompt"] = json!("prefix\nCurrent date: 2026-10-02\nCurrent working directory: /repo\nappend");
        let fingerprint = |value: &Value| config_fingerprint(value, &json!({}), "ambient", "default");
        assert_ne!(fingerprint(&options).system_prompt_hash, fingerprint(&tomorrow).system_prompt_hash);
        tomorrow["systemPrompt"] = json!("prefix\nCurrent date: 2026-10-02\nCurrent working directory: /other\nappend");
        assert_ne!(fingerprint(&options).system_prompt_hash, fingerprint(&tomorrow).system_prompt_hash);
    }
    #[test]
    fn object_order_is_stable_and_missing_options_are_not_null() {
        assert_eq!(session_sync_digest(&json!({"z":1,"a":2})), session_sync_digest(&json!({"a":2,"z":1})));
        let baseline = config_fingerprint(&json!({}), &json!({}), "ambient", "a");
        let changed = config_fingerprint(&json!({"effort":null}), &json!({}), "ambient", "a");
        assert_ne!(baseline.toolset_hash, changed.toolset_hash);
        assert_ne!(session_sync_digest(&json!({"a":null})), session_sync_digest(&json!({})));
    }
    #[test]
    fn fingerprints_include_compaction_settings_and_long_context_state() {
        let baseline = config_fingerprint(&json!({}), &json!({}), "ambient", "a");
        let compact = config_fingerprint(&json!({"settings":{"autoCompactEnabled":false}}), &json!({}), "ambient", "a");
        let long_context = config_fingerprint(&json!({"longContextRefused":true}), &json!({}), "ambient", "a");
        assert_ne!(baseline.toolset_hash, compact.toolset_hash);
        assert_ne!(baseline.toolset_hash, long_context.toolset_hash);
    }
}
