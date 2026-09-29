//! Port of senpi packages/ai/src/utils/retry-hint.ts.

use regex::{Regex, RegexBuilder};
use reqwest::header::HeaderMap;
use serde_json::Value;
use std::sync::LazyLock;

fn ci(pattern: &str) -> Regex {
    RegexBuilder::new(pattern).case_insensitive(true).build().unwrap_or_else(|e| panic!("retry-hint pattern: {e}"))
}

static MARKER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(retry-after-ms: (\d+)\)$").unwrap_or_else(|e| panic!("{e}")));
static RATE_LIMIT_BODY_RE: LazyLock<Regex> = LazyLock::new(|| {
    ci(r"\b(rate_limit_error|rate_limit_exceeded|too_many_requests|resource_exhausted|rate limit (?:exceeded|hit)|too many requests)\b")
});
const RATE_LIMIT_JSON_VALUES: &[&str] = &["rate_limit_error", "rate_limit_exceeded", "too_many_requests", "resource_exhausted"];
static RETRY_AFTER_MS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+(?:\.\d+)?$").unwrap_or_else(|e| panic!("{e}")));
static DELTA_SECONDS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+$").unwrap_or_else(|e| panic!("{e}")));
static RETRY_DELAY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+(?:\.\d+)?)\s*(ms|s|m)$").unwrap_or_else(|e| panic!("{e}")));
static PROSE_MS_RE: LazyLock<Regex> = LazyLock::new(|| ci(r"retry-after-ms:\s*(\d+)"));
static PROSE_S_RE: LazyLock<Regex> = LazyLock::new(|| ci(r"retry-after:\s*(\d+)"));
static PROSE_WAIT_RE: LazyLock<Regex> =
    LazyLock::new(|| ci(r"(?:try again|retry|wait after).*?(?:in\s+)?(\d+(?:\.\d+)?)\s*(ms|s|sec|seconds?|m|min|minutes?)\b"));
static PROSE_RESET_RE: LazyLock<Regex> = LazyLock::new(|| ci(r"resets?\s+at\s+(\S+)"));
static DATA_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^data:\s*").unwrap_or_else(|e| panic!("{e}")));

#[derive(Debug, Clone, Default)]
pub struct RetryHintInput<'a> {
    pub status: Option<u16>,
    pub headers: Option<&'a HeaderMap>,
    pub body_text: &'a str,
}

fn header<'a>(headers: Option<&'a HeaderMap>, name: &str) -> Option<&'a str> {
    headers?.get(name)?.to_str().ok()
}

/// `Date.parse` for the HTTP-date and ISO-8601 forms providers send.
pub(crate) fn date_parse_ms(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    chrono::DateTime::parse_from_rfc2822(raw)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(raw))
        .map(|date| date.timestamp_millis())
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f").ok().map(|d| d.and_utc().timestamp_millis())
        })
}

fn is_rate_limited(status: Option<u16>, body_text: &str) -> bool {
    if status == Some(429) || RATE_LIMIT_BODY_RE.is_match(body_text) {
        return true;
    }
    try_parse_json(body_text).is_some_and(|json| {
        scan_value(&json, None, &mut |value, key| match value {
            Value::String(text) => {
                key.is_some_and(|k| k.to_lowercase() == "retrydelay") || RATE_LIMIT_JSON_VALUES.contains(&text.as_str())
            }
            Value::Number(number) => number.as_f64() == Some(429.0),
            _ => false,
        })
    })
}

fn from_retry_after_ms_header(headers: Option<&HeaderMap>) -> Option<u64> {
    let raw = header(headers, "retry-after-ms")?;
    if !RETRY_AFTER_MS_RE.is_match(raw) {
        return None;
    }
    let value: f64 = raw.parse().ok()?;
    (value.is_finite() && value >= 0.0).then(|| value.ceil() as u64)
}

fn from_retry_after_header(headers: Option<&HeaderMap>, now_ms: i64) -> Option<u64> {
    let raw = header(headers, "retry-after")?;
    if DELTA_SECONDS_RE.is_match(raw) {
        return raw.parse::<u64>().ok().map(|seconds| seconds.saturating_mul(1000));
    }
    if raw.chars().any(|c| c.is_ascii_alphabetic()) {
        let parsed = date_parse_ms(raw)?;
        let base = header(headers, "date").and_then(date_parse_ms).unwrap_or(now_ms);
        return Some(clamp_absolute(parsed, base));
    }
    None
}

fn from_x_rate_limit_reset(headers: Option<&HeaderMap>, now_ms: i64) -> Option<u64> {
    let mut best: Option<u64> = None;
    for key in ["x-ratelimit-reset", "x-ratelimit-reset-requests", "x-ratelimit-reset-tokens"] {
        let Some(raw) = header(headers, key) else { continue };
        let Ok(epoch) = raw.parse::<f64>() else { continue };
        if !epoch.is_finite() || epoch < 0.0 || !RETRY_AFTER_MS_RE.is_match(raw) {
            continue;
        }
        let delta = (epoch * 1000.0 - now_ms as f64).ceil();
        let clamped = delta.max(0.0) as u64;
        best = Some(best.map_or(clamped, |b| b.max(clamped)));
    }
    best
}

fn from_json_retry_delay(body_text: &str) -> Option<u64> {
    let json = try_parse_json(body_text)?;
    let mut best: Option<u64> = None;
    scan_value(&json, None, &mut |value, _| {
        if let Value::String(text) = value
            && let Some(captures) = RETRY_DELAY_RE.captures(text)
        {
            let number: f64 = captures[1].parse().unwrap_or(0.0);
            let ms = match &captures[2] {
                "ms" => number,
                "s" => number * 1000.0,
                _ => number * 60_000.0,
            };
            let int_ms = ms.ceil() as u64;
            best = Some(best.map_or(int_ms, |b| b.max(int_ms)));
        }
        false
    });
    best
}

fn from_body_prose(body_text: &str, now_ms: i64) -> Option<u64> {
    if let Some(m) = PROSE_MS_RE.captures(body_text) {
        return m[1].parse().ok();
    }
    if let Some(m) = PROSE_S_RE.captures(body_text) {
        return m[1].parse::<u64>().ok().map(|s| s.saturating_mul(1000));
    }
    if let Some(m) = PROSE_WAIT_RE.captures(body_text) {
        return Some(convert_unit(m[1].parse().unwrap_or(0.0), &m[2]));
    }
    let m = PROSE_RESET_RE.captures(body_text)?;
    date_parse_ms(&m[1]).map(|parsed| clamp_absolute(parsed, now_ms))
}

fn clamp_absolute(target_ms: i64, base_ms: i64) -> u64 {
    target_ms.saturating_sub(base_ms).max(0) as u64
}

fn convert_unit(number: f64, unit: &str) -> u64 {
    match unit.to_lowercase().as_str() {
        "ms" => number.ceil() as u64,
        "s" | "sec" | "second" | "seconds" => (number * 1000.0).ceil() as u64,
        _ => (number * 60_000.0).ceil() as u64,
    }
}

fn scan_value(value: &Value, key: Option<&str>, visit: &mut dyn FnMut(&Value, Option<&str>) -> bool) -> bool {
    match value {
        Value::Array(items) => items.iter().any(|item| scan_value(item, key, visit)),
        Value::Object(map) => map.iter().any(|(k, v)| scan_value(v, Some(k), visit)),
        scalar => visit(scalar, key),
    }
}

fn try_parse_json(text: &str) -> Option<Value> {
    let cleaned = DATA_PREFIX_RE.replace_all(text, "");
    let cleaned = cleaned.trim();
    if !cleaned.starts_with('{') && !cleaned.starts_with('[') {
        return None;
    }
    serde_json::from_str(cleaned).ok()
}

/// Server-requested delay of a rate-limited response, or `None` when it is not rate limited.
pub fn extract_429_retry_after_ms(input: &RetryHintInput<'_>, now_ms: Option<i64>) -> Option<u64> {
    let now = now_ms.unwrap_or_else(crate::utils::diagnostics::now_ms);
    if !is_rate_limited(input.status, input.body_text) {
        return None;
    }
    from_retry_after_ms_header(input.headers)
        .or_else(|| from_retry_after_header(input.headers, now))
        .or_else(|| from_x_rate_limit_reset(input.headers, now))
        .or_else(|| from_json_retry_delay(input.body_text))
        .or_else(|| from_body_prose(input.body_text, now))
}

pub fn append_retry_after_ms_marker(message: &str, hint_ms: u64) -> String {
    format!("{message} (retry-after-ms: {hint_ms})")
}

pub fn parse_retry_after_ms_marker(message: &str) -> Option<u64> {
    MARKER_RE.captures(message)?[1].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().expect("header"));
        }
        map
    }

    fn hint(status: Option<u16>, h: &HeaderMap, body: &str) -> Option<u64> {
        extract_429_retry_after_ms(&RetryHintInput { status, headers: Some(h), body_text: body }, Some(1_000_000))
    }

    #[test]
    fn header_precedence_and_rate_limit_detection() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(500), &headers(&[("retry-after", "3")]), ""), None);
        assert_eq!(hint(Some(429), &headers(&[("retry-after-ms", "1500.2"), ("retry-after", "3")]), ""), Some(1501));
        assert_eq!(hint(Some(429), &headers(&[("retry-after", "3")]), ""), Some(3000));
        assert_eq!(hint(Some(429), &headers(&[("x-ratelimit-reset", "1002.5")]), ""), Some(2500));
        assert_eq!(hint(None, &empty, r#"{"error":{"status":"RESOURCE","details":[{"retryDelay":"1.5s"}]}}"#), Some(1500));
        assert_eq!(hint(None, &empty, "rate limit exceeded, try again in 2 minutes"), Some(120_000));
        assert_eq!(hint(Some(429), &empty, "slow down"), None);
    }

    #[test]
    fn http_date_retry_after_uses_the_date_header() {
        let h = headers(&[("retry-after", "Wed, 21 Oct 2015 07:28:10 GMT"), ("date", "Wed, 21 Oct 2015 07:28:00 GMT")]);
        assert_eq!(hint(Some(429), &h, ""), Some(10_000));
    }

    #[test]
    fn marker_round_trip() {
        let message = append_retry_after_ms_marker("Server busy", 4200);
        assert_eq!(message, "Server busy (retry-after-ms: 4200)");
        assert_eq!(parse_retry_after_ms_marker(&message), Some(4200));
        assert_eq!(parse_retry_after_ms_marker("no marker"), None);
    }
}
