//! Copy-on-write transform of inline tool-result media, gated before graph traversal.
use std::borrow::Cow;
use serde_json::{Value,json};
pub fn base64_byte_length(data: &str) -> usize {
    let padding = if data.ends_with("==") { 2 } else if data.ends_with('=') { 1 } else { 0 };
    data.encode_utf16().count().saturating_mul(3).checked_div(4).unwrap_or(0).saturating_sub(padding)
}
fn omit_content_images(content: &Value, tool_call_id: &str) -> Option<Value> {
    let blocks = content.as_array()?;
    let mut replaced: Option<Vec<Value>> = None;
    for (index,block) in blocks.iter().enumerate() {
        if block.get("type").and_then(Value::as_str) != Some("image") { continue; }
        let Some(data) = block.get("data").and_then(Value::as_str) else { continue; };
        let output = replaced.get_or_insert_with(|| blocks.clone());
        output[index] = json!({"type":"image_ref","mimeType":block.get("mimeType").and_then(Value::as_str).unwrap_or("application/octet-stream"),"byteLength":base64_byte_length(data),"ref":{"toolCallId":tool_call_id,"contentIndex":index}});
    }
    replaced.map(Value::Array)
}
fn walk(value: &Value) -> Cow<'_,Value> {
    match value {
        Value::Array(items) => {
            let mut replaced: Option<Vec<Value>> = None;
            for (index,item) in items.iter().enumerate() {
                if let Cow::Owned(next) = walk(item) { replaced.get_or_insert_with(|| items.clone())[index] = next; }
            }
            replaced.map(Value::Array).map_or(Cow::Borrowed(value),Cow::Owned)
        }
        Value::Object(object) => {
            let mut replaced = None;
            let mut scrubbed_content = false;
            if value.get("role").and_then(Value::as_str) == Some("toolResult")
                && let Some(content) = value.get("content").and_then(|c| omit_content_images(c,value.get("toolCallId").and_then(Value::as_str).unwrap_or(""))) {
                    let output = replaced.get_or_insert_with(|| object.clone());
                    output.insert("content".into(),content);
                    scrubbed_content = true;
            }
            for (key,child) in object {
                if key == "content" && scrubbed_content { continue; }
                if let Cow::Owned(next) = walk(child) { replaced.get_or_insert_with(|| object.clone()).insert(key.clone(),next); }
            }
            replaced.map(Value::Object).map_or(Cow::Borrowed(value),Cow::Owned)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Cow::Borrowed(value),
    }
}
pub fn omit_inline_media(record: &Value) -> Cow<'_,Value> {
    let kind = record.get("type").and_then(Value::as_str);
    let carries = matches!(kind,Some("tool_execution_end"|"message_start"|"message_end"|"turn_end"|"agent_end"|"entry_appended")) || (kind == Some("response") && matches!(record.get("command").and_then(Value::as_str),Some("get_messages"|"get_entries"|"get_tree"|"get_state"|"open_session")));
    if !carries { return Cow::Borrowed(record); }
    if kind == Some("tool_execution_end")
        && let Some(content) = record.get("result").and_then(|r| r.get("content")).and_then(|c| omit_content_images(c,record.get("toolCallId").and_then(Value::as_str).unwrap_or(""))) {
            let mut scrubbed = record.clone();
            scrubbed["result"]["content"] = content;
            return Cow::Owned(walk(&scrubbed).into_owned());
    }
    walk(record)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn snapshot_tool_images_become_fetchable_references() { let record = json!({"type":"response","command":"get_messages","data":{"messages":[{"role":"toolResult","toolCallId":"call","content":[{"type":"image","data":"YWJj","mimeType":"image/png"}]}]}}); let output = omit_inline_media(&record); assert_eq!(output["data"]["messages"][0]["content"][0],json!({"type":"image_ref","mimeType":"image/png","byteLength":3,"ref":{"toolCallId":"call","contentIndex":0}})); assert_eq!(record["data"]["messages"][0]["content"][0]["type"],"image"); }
    #[test] fn direct_tool_execution_images_are_scrubbed() { let record = json!({"type":"tool_execution_end","toolCallId":"call","result":{"content":[{"type":"image","data":"YQ=="}]}}); assert_eq!(omit_inline_media(&record)["result"]["content"][0]["byteLength"],1); }
    #[test] fn hot_delta_path_is_never_walked() { let record = json!({"type":"message_update","message":{"role":"toolResult","content":[{"type":"image","data":"YQ=="}]}}); assert!(matches!(omit_inline_media(&record),Cow::Borrowed(_))); }
    #[test] fn user_images_stay_inline() { let record = json!({"type":"message_end","message":{"role":"user","content":[{"type":"image","data":"YQ=="}]}}); assert!(matches!(omit_inline_media(&record),Cow::Borrowed(_))); }
    #[test] fn unchanged_graph_is_borrowed() { let record = json!({"type":"agent_end","messages":[{"role":"toolResult","content":[{"type":"text","text":"ok"}]}]}); assert!(matches!(omit_inline_media(&record),Cow::Borrowed(_))); }
    #[test] fn base64_length_handles_padding() { assert_eq!(base64_byte_length("YQ=="),1); assert_eq!(base64_byte_length("YWI="),2); assert_eq!(base64_byte_length("YWJj"),3); assert_eq!(base64_byte_length(""),0); }
    #[test] fn earlier_changed_sibling_does_not_skip_content_subtree() { let tool = json!({"role":"toolResult","toolCallId":"call","content":[{"type":"image","data":"YQ=="}]}); let record = json!({"type":"agent_end","before":tool,"content":{"nested":tool}}); let output = omit_inline_media(&record); assert_eq!(output["content"]["nested"]["content"][0]["type"],"image_ref"); }
}
