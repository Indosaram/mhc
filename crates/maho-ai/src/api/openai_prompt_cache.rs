//! Port of senpi packages/ai/src/api/openai-prompt-cache.ts.

pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

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
    let Some(session_id) = session_id else { return };
    for name in ["session-id", "thread-id", "x-client-request-id"] {
        let Ok(value) = reqwest::header::HeaderValue::from_str(session_id) else { continue };
        headers.insert(reqwest::header::HeaderName::from_static(name), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_by_code_point_like_array_from() {
        assert_eq!(clamp_openai_prompt_cache_key(None), None);
        assert_eq!(clamp_openai_prompt_cache_key(Some("short")), Some("short".into()));
        let long = "é".repeat(70);
        let clamped = clamp_openai_prompt_cache_key(Some(&long)).expect("clamped");
        assert_eq!(clamped.chars().count(), OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH);
    }

    #[test]
    fn affinity_headers_are_skipped_without_a_session_id() {
        let mut headers = reqwest::header::HeaderMap::new();
        apply_chat_gpt_subscription_cache_affinity_headers(&mut headers, None);
        assert!(headers.is_empty());

        apply_chat_gpt_subscription_cache_affinity_headers(&mut headers, Some("s1"));
        assert_eq!(headers.get("session-id").and_then(|value| value.to_str().ok()), Some("s1"));
        assert_eq!(headers.get("thread-id").and_then(|value| value.to_str().ok()), Some("s1"));
        assert_eq!(headers.get("x-client-request-id").and_then(|value| value.to_str().ok()), Some("s1"));
    }
}
