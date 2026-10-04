use regex::Regex;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorCliErrorKind {
    BinaryMissing, InvalidApiKey, KeychainLocked, InvalidModel, RateLimit,
    AuthError, ContextOverflow, Network, MalformedStream, Other,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorCliErrorClassification {
    pub kind: CursorCliErrorKind,
    pub retryable: bool,
    pub block_ms: Option<f64>,
}
#[derive(Default)]
pub struct CursorCliErrorInput {
    pub exit_code: Value,
    pub stderr: Value,
    pub result_event: Value,
    pub thrown: Value,
}
pub const DEFAULT_RATE_LIMIT_BLOCK_MS: f64 = 60_000.0;
pub const MAX_RATE_LIMIT_BLOCK_MS: f64 = 172_800_000.0;
pub const CURSOR_CONTEXT_OVERFLOW_WORDINGS: [&str; 0] = [];

fn collect_text<'a>(value: &'a Value, output: &mut Vec<&'a str>, depth: usize) {
    if output.len() >= 100 || depth > 5 { return; }
    match value {
        Value::String(text) => output.push(text),
        Value::Array(values) => {
            for value in values {
                collect_text(value, output, depth + 1);
                if output.len() >= 100 { break; }
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_text(value, output, depth + 1);
                if output.len() >= 100 { break; }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}
fn positive_number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite() && *number > 0.0).map(f64::ceil)
}
fn find_retry_after_ms(value: &Value, depth: usize) -> Option<f64> {
    if depth > 5 { return None; }
    match value {
        Value::Object(values) => positive_number(&value["retryAfterMs"])
            .or_else(|| positive_number(&value["retry_after_ms"]))
            .or_else(|| values.values().find_map(|value| find_retry_after_ms(value, depth + 1))),
        Value::Array(values) => values.iter().find_map(|value| find_retry_after_ms(value, depth + 1)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}
fn matches(pattern: &str, text: &str) -> bool {
    Regex::new(pattern).is_ok_and(|regex| regex.is_match(text))
}
fn text_retry_after(pattern: &str, text: &str, multiplier: f64) -> Option<f64> {
    let regex = Regex::new(pattern).ok()?;
    let capture = regex.captures(text)?;
    let number = capture.get(1)?.as_str().parse::<f64>().ok()?;
    Some((number * multiplier).ceil())
}

pub fn classify_cursor_cli_error(input: Option<&CursorCliErrorInput>) -> CursorCliErrorClassification {
    use CursorCliErrorKind::*;
    let classification = |kind, retryable| CursorCliErrorClassification { kind, retryable, block_ms: None };
    let Some(input) = input else { return classification(Other, false); };
    if input.thrown.get("kind").and_then(Value::as_str) == Some("binary_missing") {
        return classification(BinaryMissing, false);
    }
    if [&input.thrown, &input.result_event].iter().any(|value| value.get("kind").and_then(Value::as_str) == Some("malformed_stream")) {
        return classification(MalformedStream, false);
    }
    let mut parts = Vec::new();
    for value in [&input.stderr, &input.result_event, &input.thrown] { collect_text(value, &mut parts, 0); }
    let text = parts.join("\n");
    let lines: Vec<_> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    if lines.contains(&"The provided API key is invalid.") && lines.contains(&"The API key was loaded from the CURSOR_API_KEY environment variable.") {
        return classification(InvalidApiKey, false);
    }
    if lines.contains(&"Error: Your macOS login keychain is locked.") { return classification(KeychainLocked, false); }
    if lines.iter().any(|line| matches(r"^(?:Error:\s*)?Invalid model value:\s+\S.*$", line)) { return classification(InvalidModel, false); }
    if matches(r"(?i-u)\b(?:HTTP\s*)?429\b|\brate[ _-]?limit(?:ed|ing)?\b|\btoo many requests\b", &text) {
        let block_ms = find_retry_after_ms(&input.result_event, 0)
            .or_else(|| find_retry_after_ms(&input.thrown, 0))
            .or_else(|| text_retry_after(r"(?i-u)\bretry[-_ ]?after[-_ ]?ms\s*[:=]\s*(\d+(?:\.\d+)?)", &text, 1.0))
            .or_else(|| text_retry_after(r"(?i-u)\bretry[-_ ]?after\s*[:=]\s*(\d+(?:\.\d+)?)", &text, 1000.0))
            .unwrap_or(DEFAULT_RATE_LIMIT_BLOCK_MS).min(MAX_RATE_LIMIT_BLOCK_MS);
        return CursorCliErrorClassification { kind: RateLimit, retryable: true, block_ms: Some(block_ms) };
    }
    if matches(r"(?i-u)\b(?:HTTP\s*)?401\b|\bunauthori[sz]ed\b|\bauthentication (?:failed|required)\b|\bnot logged in\b", &text) { return classification(AuthError, false); }
    if matches(r"(?i-u)\b(?:ECONNRESET|ECONNREFUSED|ETIMEDOUT|ENETUNREACH|EHOSTUNREACH|ENOTFOUND|EAI_AGAIN)\b|\bnetwork error\b|\bconnection (?:reset|refused|lost)\b|\bsocket hang up\b|\bfetch failed\b", &text) { return classification(Network, true); }
    classification(Other, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn stderr(text: &str) -> CursorCliErrorClassification {
        classify_cursor_cli_error(Some(&CursorCliErrorInput { stderr: json!(text), ..Default::default() }))
    }
    fn assert_kind(classification: CursorCliErrorClassification, kind: CursorCliErrorKind, retryable: bool) {
        assert_eq!(classification.kind, kind);
        assert_eq!(classification.retryable, retryable);
    }
    #[test]
    fn closed_error_kind_contract() {
        use CursorCliErrorKind::*;
        assert_eq!([BinaryMissing, InvalidApiKey, KeychainLocked, InvalidModel, RateLimit, AuthError, ContextOverflow, Network, MalformedStream, Other].len(), 10);
    }
    #[test]
    fn binary_missing_structurally() {
        assert_kind(classify_cursor_cli_error(Some(&CursorCliErrorInput { thrown: json!({"kind":"binary_missing", "message":"install cursor-agent"}), ..Default::default() })), CursorCliErrorKind::BinaryMissing, false);
    }
    #[test]
    fn invalid_key_requires_both_observed_lines() {
        assert_kind(stderr("The provided API key is invalid.\nThe API key was loaded from the CURSOR_API_KEY environment variable."), CursorCliErrorKind::InvalidApiKey, false);
        assert_kind(stderr("The provided API key is invalid."), CursorCliErrorKind::Other, false);
    }
    #[test]
    fn keychain_locked() {
        assert_kind(stderr("Error: Your macOS login keychain is locked.\nRun security unlock-keychain and try again."), CursorCliErrorKind::KeychainLocked, false);
    }
    #[test]
    fn invalid_model_result_even_on_success_exit() {
        assert_kind(classify_cursor_cli_error(Some(&CursorCliErrorInput { exit_code: json!(0), result_event: json!({"type":"result", "subtype":"error", "is_error":true, "result":"Invalid model value: bogus"}), ..Default::default() })), CursorCliErrorKind::InvalidModel, false);
    }
    #[test]
    fn default_rate_limit_block() {
        let classified = stderr("HTTP 429: rate limit exceeded");
        assert_kind(classified, CursorCliErrorKind::RateLimit, true);
        assert_eq!(classified.block_ms, Some(DEFAULT_RATE_LIMIT_BLOCK_MS));
    }
    #[test]
    fn server_hints_and_cap() {
        let classified = classify_cursor_cli_error(Some(&CursorCliErrorInput { result_event: json!({"is_error":true, "result":"rate_limit", "retryAfterMs":2500}), ..Default::default() }));
        assert_eq!(classified.block_ms, Some(2500.0));
        assert_eq!(stderr("HTTP 429 retry-after: 200000").block_ms, Some(MAX_RATE_LIMIT_BLOCK_MS));
        assert_eq!(stderr("HTTP 429 retry-after-ms=12.1").block_ms, Some(13.0));
    }
    #[test]
    fn auth_error_without_expiring_block() {
        let classified = classify_cursor_cli_error(Some(&CursorCliErrorInput { result_event: json!({"type":"result", "is_error":true, "result":"HTTP 401 Unauthorized"}), ..Default::default() }));
        assert_kind(classified, CursorCliErrorKind::AuthError, false);
        assert!(classified.block_ms.is_none());
    }
    #[test]
    fn context_overflow_unmatched_without_probe_wording() {
        assert!(CURSOR_CONTEXT_OVERFLOW_WORDINGS.is_empty());
        assert_kind(stderr("context window exceeded for this request"), CursorCliErrorKind::Other, false);
    }
    #[test]
    fn network_retryable() {
        assert_kind(classify_cursor_cli_error(Some(&CursorCliErrorInput { thrown: json!({"message":"read ECONNRESET"}), ..Default::default() })), CursorCliErrorKind::Network, true);
    }
    #[test]
    fn parser_failure_by_kind() {
        assert_kind(classify_cursor_cli_error(Some(&CursorCliErrorInput { thrown: json!({"kind":"malformed_stream", "message":"invalid NDJSON"}), ..Default::default() })), CursorCliErrorKind::MalformedStream, false);
    }
    #[test]
    fn malformed_inputs_and_near_misses() {
        assert_kind(classify_cursor_cli_error(None), CursorCliErrorKind::Other, false);
        assert_kind(classify_cursor_cli_error(Some(&CursorCliErrorInput { exit_code: json!("garbage"), stderr: json!({"nested":null}), result_event: json!(42), thrown: json!(false) })), CursorCliErrorKind::Other, false);
        for text in ["The pirate limit was reached after 4290 attempts", "service overloaded"] { assert_kind(stderr(text), CursorCliErrorKind::Other, false); }
    }
}
