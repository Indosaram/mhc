use serde_json::Value;

fn string(value: &Value, key: &str) -> bool { value.get(key).is_some_and(Value::is_string) }
fn finite(value: &Value, key: &str) -> bool { value.get(key).and_then(Value::as_f64).is_some_and(f64::is_finite) }
fn optional(value: &Value, key: &str, check: impl FnOnce(&Value) -> bool) -> bool { value.get(key).is_none_or(check) }
fn text_only(content: &Value, allow_string: bool) -> bool {
    (allow_string && content.is_string()) || content.as_array().is_some_and(|blocks| blocks.iter().all(|b| b.is_object() && b.get("type").and_then(Value::as_str) == Some("text") && string(b,"text")))
}
fn usage(value: &Value) -> bool {
    value.is_object() && value.get("cost").is_some_and(|cost| cost.is_object() && ["input","output","cacheRead","cacheWrite","total"].iter().all(|key| finite(cost,key)))
        && ["input","output","cacheRead","cacheWrite","totalTokens"].iter().all(|key| finite(value,key))
        && ["cacheWrite1h","reasoning"].iter().all(|key| optional(value,key,|v|v.as_f64().is_some_and(f64::is_finite)))
}
fn opaque(value: &Value) -> bool { value.as_str().is_some_and(|s| !s.is_empty() && s.encode_utf16().count() <= 65536) }
fn signature(value: &Value, google: bool) -> bool {
    if !opaque(value) { return false; }
    if !google { return true; }
    let Some(s) = value.as_str() else { return false; };
    let body = s.trim_end_matches('=');
    s.len() % 4 == 0 && !body.is_empty() && s.len()-body.len() <= 2 && body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}
fn assistant_content(value: &Value, google: bool) -> bool {
    value.as_array().is_some_and(|blocks| blocks.iter().all(|b| {
        if !b.is_object() { return false; }
        match b.get("type").and_then(Value::as_str) {
            Some("text") => string(b,"text") && optional(b,"textSignature",|v|signature(v,google)),
            Some("thinking") => string(b,"thinking") && ["startedAt","endedAt"].iter().all(|key|optional(b,key,|v|v.as_f64().is_some_and(f64::is_finite)))
                && optional(b,"redacted",Value::is_boolean) && optional(b,"thinkingSignature",|v|signature(v,google))
                && (b.get("redacted") != Some(&Value::Bool(true)) || b.get("thinkingSignature").is_some_and(opaque)),
            Some("toolCall") => string(b,"id") && string(b,"name") && b.get("arguments").is_some_and(Value::is_object)
                && optional(b,"incomplete",|v|v == &Value::Bool(true)) && optional(b,"errorMessage",Value::is_string)
                && optional(b,"thoughtSignature",|v|signature(v,google)),
            _ => false,
        }
    }))
}
fn safe_assistant(value: &Value) -> bool {
    let google = matches!(value.get("provider").and_then(Value::as_str),Some("google"|"google-vertex"));
    ["api","provider","model"].iter().all(|key|string(value,key)) && value.get("usage").is_some_and(usage)
        && matches!(value.get("stopReason").and_then(Value::as_str),Some("pending"|"stop"|"length"|"toolUse"|"error"|"aborted"))
        && optional(value,"stopDetails",|v|v.is_object() && (v.get("type").and_then(Value::as_str) == Some("sensitive") || (v.get("type").and_then(Value::as_str) == Some("refusal") && optional(v,"explanation",Value::is_string))))
        && finite(value,"timestamp") && ["responseModel","responseId","errorMessage","rawStopReason"].iter().all(|key|optional(value,key,Value::is_string))
        && optional(value,"diagnostics",Value::is_array) && value.get("content").is_some_and(|v|assistant_content(v,google))
}
fn safe_result(value: &Value) -> bool {
    ["toolCallId","toolName"].iter().all(|key|value.get(key).and_then(Value::as_str).is_some_and(|s|!s.is_empty()))
        && value.get("content").and_then(Value::as_array).is_some_and(|blocks|blocks.iter().all(|b|b.is_object() && match b.get("type").and_then(Value::as_str) {
            Some("text") => string(b,"text"),
            Some("image") => b.get("mimeType").and_then(Value::as_str).is_some_and(|s|s.starts_with("image/")) && b.get("data").and_then(Value::as_str).is_some_and(|s|!s.is_empty()),
            _ => false,
        })) && value.get("isError").is_some_and(Value::is_boolean) && finite(value,"timestamp") && optional(value,"usage",usage)
        && optional(value,"addedToolNames",|v|v.as_array().is_some_and(|a|a.iter().all(Value::is_string)))
}
pub fn has_unsafe_retained_content(messages: &[Value]) -> bool {
    messages.iter().any(|value| {
        if !value.is_object() { return true; }
        let safe = match value.get("role").and_then(Value::as_str) {
            Some("user") => finite(value,"timestamp") && value.get("content").is_some_and(|v|text_only(v,true)),
            Some("assistant") => safe_assistant(value),
            Some("toolResult") => safe_result(value),
            Some("custom") => string(value,"customType") && value.get("display").is_some_and(Value::is_boolean) && finite(value,"timestamp") && value.get("content").is_some_and(|v|text_only(v,true)),
            Some("bashExecution") => string(value,"command") && string(value,"output") && optional(value,"exitCode",|v|v.as_f64().is_some_and(f64::is_finite))
                && ["cancelled","truncated"].iter().all(|key|value.get(key).is_some_and(Value::is_boolean)) && optional(value,"fullOutputPath",Value::is_string) && optional(value,"excludeFromContext",Value::is_boolean) && finite(value,"timestamp"),
            Some("compactionSummary") => string(value,"summary") && finite(value,"tokensBefore") && finite(value,"timestamp"),
            _ => false,
        };
        !safe
    })
}
