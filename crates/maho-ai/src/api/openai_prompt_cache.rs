//! Port of senpi packages/ai/src/api/openai-prompt-cache.ts.

use serde_json::{Map, Value, json};

use crate::types::TranscriptContext;
use crate::utils::hash::short_hash;
use crate::utils::text::get_system_message_text;
use crate::utils::transcript::get_initial_system_message;

pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

const PREFIX_VERSION: &str = "maho-responses-prefix-v1";

/// Cache affinity belongs to the reusable leading prefix, never to a conversation UUID.
pub fn derive_openai_prompt_cache_key(
    model_id: &str,
    context: &TranscriptContext,
) -> Option<String> {
    let initial = get_initial_system_message(&context.messages);
    let mut tools: Vec<&crate::types::Tool> = initial
        .map(|message| message.tools_added.iter().collect())
        .unwrap_or_default();
    tools.sort_by(|a, b| a.name.encode_utf16().cmp(b.name.encode_utf16()));
    let tools: Vec<Value> = tools
        .into_iter()
        .map(|tool| {
            let mut entry = Map::new();
            entry.insert("name".into(), json!(tool.name));
            entry.insert("description".into(), json!(tool.description));
            entry.insert("parameters".into(), tool.parameters.clone());
            if let Some(freeform) = tool
                .freeform
                .as_ref()
                .and_then(|v| serde_json::to_value(v).ok())
            {
                entry.insert("freeform".into(), freeform);
            }
            if let Some(sampling) = tool
                .constrained_sampling
                .as_ref()
                .and_then(|v| serde_json::to_value(v).ok())
            {
                entry.insert("constrainedSampling".into(), sampling);
            }
            Value::Object(entry)
        })
        .collect();
    let system_text = initial
        .map(|message| get_system_message_text(&message.content, &message.sections))
        .unwrap_or_default();
    let prefix = Value::Array(vec![
        json!(PREFIX_VERSION),
        json!(model_id),
        json!(system_text),
        Value::Array(tools),
    ])
    .to_string();
    clamp_openai_prompt_cache_key(Some(&format!("{PREFIX_VERSION}-{}", short_hash(&prefix))))
}

pub fn clamp_openai_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let key = key?;
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        return Some(key.to_owned());
    }
    Some(chars[..OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH].iter().collect())
}

pub fn apply_chat_gpt_subscription_cache_affinity_headers(
    headers: &mut reqwest::header::HeaderMap,
    session_id: Option<&str>,
) {
    let Some(session_id) = session_id.filter(|id| !id.is_empty()) else {
        return;
    };
    for name in ["session-id", "thread-id", "x-client-request-id"] {
        let Ok(value) = reqwest::header::HeaderValue::from_str(session_id) else {
            continue;
        };
        headers.insert(reqwest::header::HeaderName::from_static(name), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_by_code_point_like_array_from() {
        assert_eq!(clamp_openai_prompt_cache_key(None), None);
        assert_eq!(
            clamp_openai_prompt_cache_key(Some("short")),
            Some("short".into())
        );
        let long = "é".repeat(70);
        let clamped = clamp_openai_prompt_cache_key(Some(&long)).expect("clamped");
        assert_eq!(clamped.chars().count(), OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH);
    }

    #[test]
    fn derived_key_is_stable_and_prefix_versioned() {
        let context = TranscriptContext {
            messages: Vec::new(),
            active_tool_names: None,
        };
        let a = derive_openai_prompt_cache_key("gpt-5", &context).expect("key");
        assert_eq!(
            Some(a.clone()),
            derive_openai_prompt_cache_key("gpt-5", &context)
        );
        assert!(a.starts_with("maho-responses-prefix-v1-"));
        assert!(a.chars().count() <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH);
        assert_ne!(Some(a), derive_openai_prompt_cache_key("gpt-6", &context));
    }

    #[test]
    fn affinity_headers_are_skipped_without_a_session_id() {
        let mut headers = reqwest::header::HeaderMap::new();
        apply_chat_gpt_subscription_cache_affinity_headers(&mut headers, None);
        assert!(headers.is_empty());

        apply_chat_gpt_subscription_cache_affinity_headers(&mut headers, Some(""));
        assert!(headers.is_empty());

        apply_chat_gpt_subscription_cache_affinity_headers(&mut headers, Some("s1"));
        assert_eq!(
            headers
                .get("session-id")
                .and_then(|value| value.to_str().ok()),
            Some("s1")
        );
        assert_eq!(
            headers
                .get("thread-id")
                .and_then(|value| value.to_str().ok()),
            Some("s1")
        );
        assert_eq!(
            headers
                .get("x-client-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("s1")
        );
    }
}
