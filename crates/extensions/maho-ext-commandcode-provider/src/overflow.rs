use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

pub const CONTEXT_OVERFLOW_PREFIX: &str = "context_length_exceeded:";
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]+").expect("pattern"));
static CREDENTIAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:api[-_ ]?key|apikey|access[-_ ]?token|refresh[-_ ]?token|token|secret|password|authorization)\s*[=:]\s*[^\s,;)]+").expect("pattern")
});
static USER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:user|cc)_[A-Za-z0-9_-]{8,}\b").expect("pattern"));
static QUERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)([?&](?:api[-_ ]?key|apikey|access_token|refresh_token|token|secret|password)=)[^&#\s]+").expect("pattern")
});
static STANDALONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:sk|rk|ghp|github_pat|xox[baprs])[-_A-Za-z0-9]{16,}\b|\beyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b").expect("pattern")
});
static OVERFLOW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:context[_\s-]*(?:length|window)|model[_\s-]*context[_\s-]*window)[_\s-]*(?:exceeded|overflow(?:ed)?|too[_\s-]*(?:large|long))\b|\b(?:context|prompt|input)[_\s-]*(?:length|window|size|tokens?|limit|maximum)\b[\s\S]{0,120}\b(?:exceed(?:ed|s)?|overflow(?:ed|s)?|too\s+(?:large|long)|(?:maximum|limit)\s+(?:reached|exceeded|hit))\b|\b(?:exceed(?:ed|s)?|overflow(?:ed|s)?|too\s+(?:large|long))\b[\s\S]{0,120}\b(?:context|prompt|input)[_\s-]*(?:length|window|size|tokens?|limit|maximum)\b|\b(?:prompt|input|context)\b[\s\S]{0,32}\btoo\s+(?:large|long)\b|\b(?:prompt|input)[_\s-]*too[_\s-]*(?:large|long)\b|\b(?:prompt|input)[_\s-]*tokens?[_\s-]*(?:limit|maximum|max)[_\s-]*(?:exceeded|reached)\b|\b(?:prompt|input)[_\s-]*(?:tokens?|length|size)\b[\s\S]{0,120}\b(?:limit|maximum)\b[\s\S]{0,40}\b(?:exceed(?:ed|s)?|reached|hit)\b|\b(?:maximum|limit)[_\s-]+(?:allowed[_\s-]+)?(?:context|prompt|input)[_\s-]*(?:length|window|size|tokens?)\b").expect("pattern")
});
static PREFIXED_OVERFLOW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)context_length_exceeded").expect("pattern"));
static NON_OVERFLOW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\brate[_\s-]*limit\b|\btoo\s+many\s+requests\b|\b(?:capacity|quota|throttl(?:e|ed|ing)?|concurren(?:cy|t)|overloaded)\b|\b(?:service|temporarily)\s+unavailable\b").expect("pattern")
});
static HTTP_429: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(?:api\s+error|http|status(?:[_\s-]*code)?|status[_\s-]*code)\s*[:(]?\s*429\b|["']?(?:status|status[_\s-]*code)["']?\s*:\s*429\b"#).expect("pattern")
});

pub fn redact_error_text(text: &str) -> String {
    let s = BEARER.replace_all(text, "Bearer [redacted]");
    let s = CREDENTIAL.replace_all(&s, |c: &regex::Captures<'_>| {
        let m = &c[0];
        let i = m.find(['=', ':']).map(|p| p + 1).unwrap_or(m.len());
        format!("{}[redacted]", &m[..i])
    });
    let s = USER.replace_all(&s, "[redacted]");
    let s = QUERY.replace_all(&s, "$1[redacted]");
    STANDALONE.replace_all(&s, "[redacted]").into_owned()
}

fn error_part(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => {
            let mut parts = Vec::new();
            for k in [
                "message",
                "errorMessage",
                "error",
                "detail",
                "details",
                "code",
                "type",
                "reason",
            ] {
                if let Some(v) = o.get(k).and_then(error_part)
                    && !v.is_empty()
                    && !parts.contains(&v)
                {
                    parts.push(v);
                }
            }
            for k in ["status", "statusCode", "httpStatus"] {
                if let Some(v) = o.get(k).filter(|v| v.is_number() || v.is_string()) {
                    let p = format!(
                        "status: {}",
                        v.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| v.to_string())
                    );
                    if !parts.contains(&p) {
                        parts.push(p);
                    }
                }
            }
            (!parts.is_empty()).then(|| parts.join(": "))
        }
        _ => None,
    }
}
pub fn command_code_error_message(value: &Value) -> String {
    redact_error_text(&error_part(value).unwrap_or_default())
}
pub fn normalize_error_message(message: &str) -> Option<String> {
    if message.is_empty()
        || PREFIXED_OVERFLOW.is_match(message)
        || NON_OVERFLOW.is_match(message)
        || HTTP_429.is_match(message)
        || !OVERFLOW.is_match(message)
    {
        return None;
    }
    Some(format!("{CONTEXT_OVERFLOW_PREFIX} {message}"))
}
pub fn normalize_assistant_message(message: &mut maho_ai::types::AssistantMessage) {
    if message.stop_reason == maho_ai::types::StopReason::Error
        && message.provider == "commandcode"
        && message.model.starts_with("commandcode")
        && let Some(error) = message
            .error_message
            .as_deref()
            .and_then(normalize_error_message)
    {
        message.error_message = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefixed_text() {
        assert_eq!(
            normalize_error_message("prompt is too long: 250000 tokens > 200000 maximum context length"),
            Some(
                "context_length_exceeded: prompt is too long: 250000 tokens > 200000 maximum context length"
                    .into()
            )
        );
    }
    #[test]
    fn ignores_unrecognized_text() {
        assert_eq!(normalize_error_message("plain text"), None);
    }
    #[test]
    fn keeps_existing_prefix() {
        assert_eq!(
            normalize_error_message("context_length_exceeded: already"),
            None
        );
    }
    #[test]
    fn excludes_non_overflow() {
        assert_eq!(normalize_error_message("rate limit exceeded"), None);
        assert_eq!(normalize_error_message("HTTP 429"), None);
    }
    #[test]
    fn redacts_bearer() {
        let out = redact_error_text("Bearer secret-token");
        assert!(!out.contains("secret-token"));
    }
}
