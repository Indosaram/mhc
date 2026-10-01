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

    /// Fixed epoch ms for deterministic tests. Mirrors senpi's `test/retry-hint.test.ts` `NOW`.
    const NOW: i64 = 1_700_000_000_000;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().expect("header"));
        }
        map
    }

    fn hint(status: Option<u16>, h: &HeaderMap, body: &str) -> Option<u64> {
        extract_429_retry_after_ms(&RetryHintInput { status, headers: Some(h), body_text: body }, Some(NOW))
    }

    fn hint_default_now(status: Option<u16>, h: &HeaderMap, body: &str) -> Option<u64> {
        extract_429_retry_after_ms(&RetryHintInput { status, headers: Some(h), body_text: body }, None)
    }

    // ---------- 1. Eligibility gate ----------

    /// "429 status with generic body + retry-after header -> hint from header"
    #[test]
    fn eligibility_429_status_with_generic_body_and_retry_after_header() {
        assert_eq!(hint(Some(429), &headers(&[("retry-after", "1258")]), r#"{"error":"rate limited"}"#), Some(1_258_000));
    }

    /// "body rate_limit_error type on 200 SSE -> eligible, but no hint -> undefined"
    #[test]
    fn eligibility_body_rate_limit_error_type_on_200_sse_eligible_no_hint() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(200), &empty, r#"{"error":{"type":"rate_limit_error","message":"All tokens rate limited"}}"#), None);
    }

    /// "500 with 'try again in 20 seconds' prose -> NOT eligible -> undefined"
    #[test]
    fn eligibility_500_with_prose_not_eligible() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(500), &empty, "try again in 20 seconds"), None);
    }

    /// "503 + prose -> undefined"
    #[test]
    fn eligibility_503_plus_prose_undefined() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(503), &empty, "try again in 30 seconds"), None);
    }

    /// "429 status with no hint anywhere -> undefined"
    #[test]
    fn eligibility_429_status_with_no_hint_anywhere() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, "rate limited"), None);
    }

    /// "200 with rate_limit_exceeded type code -> eligible, no hint -> undefined"
    #[test]
    fn eligibility_200_with_rate_limit_exceeded_type_code() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(200), &empty, r#"{"error":{"type":"rate_limit_exceeded"}}"#), None);
    }

    /// "200 with resource_exhausted type -> eligible, no hint -> undefined"
    #[test]
    fn eligibility_200_with_resource_exhausted_type() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(200), &empty, r#"{"error":{"type":"resource_exhausted"}}"#), None);
    }

    /// "200 with too_many_requests code string -> eligible, no hint -> undefined"
    #[test]
    fn eligibility_200_with_too_many_requests_code_string() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(200), &empty, r#"{"error":{"code":"too_many_requests"}}"#), None);
    }

    /// "200 with numeric code 429 -> eligible, no hint -> undefined"
    #[test]
    fn eligibility_200_with_numeric_code_429() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(200), &empty, r#"{"error":{"code":429}}"#), None);
    }

    /// "200 with standalone text marker 'rate limit exceeded' -> eligible, no hint -> undefined"
    #[test]
    fn eligibility_200_with_standalone_text_marker_rate_limit_exceeded() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(200), &empty, "rate limit exceeded"), None);
    }

    // ---------- 2. retry-after-ms header (highest precedence) ----------

    /// "strict integer"
    #[test]
    fn retry_after_ms_header_strict_integer() {
        assert_eq!(hint(Some(429), &headers(&[("retry-after-ms", "5000")]), ""), Some(5000));
    }

    /// "strict decimal ceil"
    #[test]
    fn retry_after_ms_header_strict_decimal_ceil() {
        assert_eq!(hint(Some(429), &headers(&[("retry-after-ms", "2.3")]), ""), Some(3));
    }

    /// "malformed 'later' falls through to retry-after: 12 -> 12000"
    #[test]
    fn retry_after_ms_header_malformed_later_falls_through() {
        assert_eq!(
            hint(Some(429), &headers(&[("retry-after-ms", "later"), ("retry-after", "12")]), ""),
            Some(12_000)
        );
    }

    /// "'12junk' is malformed (strict full-string) -> falls through"
    #[test]
    fn retry_after_ms_header_12junk_is_malformed_falls_through() {
        assert_eq!(hint(Some(429), &headers(&[("retry-after-ms", "12junk"), ("retry-after", "5")]), ""), Some(5_000));
    }

    /// "explicit zero beats lower-precedence positive prose 30s"
    #[test]
    fn retry_after_ms_header_explicit_zero_beats_lower_precedence_prose() {
        assert_eq!(hint(Some(429), &headers(&[("retry-after-ms", "0")]), "try again in 30 seconds"), Some(0));
    }

    // ---------- 3. retry-after header (delta-seconds or HTTP-date) ----------

    /// "integer delta-seconds"
    #[test]
    fn retry_after_header_integer_delta_seconds() {
        assert_eq!(hint(Some(429), &headers(&[("retry-after", "1258")]), "generic"), Some(1_258_000));
    }

    /// "explicit zero beats prose 30s"
    #[test]
    fn retry_after_header_explicit_zero_beats_prose() {
        assert_eq!(hint(Some(429), &headers(&[("retry-after", "0")]), "try again in 30 seconds"), Some(0));
    }

    /// "0.5 is malformed (strict integer) -> falls through"
    #[test]
    fn retry_after_header_0_5_is_malformed_falls_through() {
        assert_eq!(hint(Some(429), &headers(&[("retry-after", "0.5")]), ""), None);
    }

    /// "HTTP-date with skewed local clock using Date header"
    #[test]
    fn retry_after_header_http_date_with_skewed_local_clock() {
        // Server says Date is 2023-11-14T22:13:20.000Z (epoch 1700000000000).
        // Retry-After is 60s after that -> 1700000060000. nowMs is deliberately
        // skewed (1 hour ahead).
        let skewed_now = NOW + 3_600_000;
        let h = headers(&[
            ("date", "Tue, 14 Nov 2023 22:13:20 GMT"),
            ("retry-after", "Tue, 14 Nov 2023 22:14:20 GMT"),
        ]);
        assert_eq!(
            extract_429_retry_after_ms(&RetryHintInput { status: Some(429), headers: Some(&h), body_text: "" }, Some(skewed_now)),
            Some(60_000)
        );
    }

    /// "HTTP-date without Date header uses nowMs, clamps past to 0"
    #[test]
    fn retry_after_header_http_date_without_date_header_clamps_past_to_0() {
        let h = headers(&[("retry-after", "Tue, 14 Nov 2023 22:00:00 GMT")]);
        // NOW = 1700000000000 = 22:13:20 UTC, after 22:00:00.
        assert_eq!(hint(Some(429), &h, ""), Some(0));
    }

    /// "HTTP-date IMF-fixdate format"
    #[test]
    fn retry_after_header_http_date_imf_fixdate_format() {
        let h = headers(&[
            ("date", "Tue, 14 Nov 2023 22:13:20 GMT"),
            ("retry-after", "Tue, 14 Nov 2023 22:14:30 GMT"),
        ]);
        assert_eq!(hint(Some(429), &h, ""), Some(70_000));
    }

    /// "malformed retry-after 'abc' falls through to x-ratelimit-reset"
    #[test]
    fn retry_after_header_malformed_abc_falls_through_to_x_ratelimit_reset() {
        let h = headers(&[("retry-after", "abc"), ("x-ratelimit-reset", &(NOW / 1000 + 10).to_string())]);
        assert_eq!(hint(Some(429), &h, ""), Some(10_000));
    }

    // ---------- 4. x-ratelimit-reset* headers (epoch seconds, max-wins) ----------

    /// "reset-requests +5s and reset-tokens +60s -> 60_000 (max wins)"
    #[test]
    fn x_ratelimit_reset_requests_and_tokens_max_wins() {
        let h = headers(&[
            ("x-ratelimit-reset-requests", &(NOW / 1000 + 5).to_string()),
            ("x-ratelimit-reset-tokens", &(NOW / 1000 + 60).to_string()),
        ]);
        assert_eq!(hint(Some(429), &h, ""), Some(60_000));
    }

    /// "x-ratelimit-reset (bare) epoch seconds"
    #[test]
    fn x_ratelimit_reset_bare_epoch_seconds() {
        let h = headers(&[("x-ratelimit-reset", &(NOW / 1000 + 30).to_string())]);
        assert_eq!(hint(Some(429), &h, ""), Some(30_000));
    }

    /// "past epoch clamps to 0"
    #[test]
    fn x_ratelimit_reset_past_epoch_clamps_to_0() {
        let h = headers(&[("x-ratelimit-reset", &(NOW / 1000 - 100).to_string())]);
        assert_eq!(hint(Some(429), &h, ""), Some(0));
    }

    /// "malformed epoch 'soon' falls through"
    #[test]
    fn x_ratelimit_reset_malformed_epoch_soon_falls_through() {
        let h = headers(&[("x-ratelimit-reset", "soon"), ("x-ratelimit-reset-tokens", &(NOW / 1000 + 5).to_string())]);
        assert_eq!(hint(Some(429), &h, ""), Some(5_000));
    }

    // ---------- 5. JSON retryDelay strings (recursive, max-wins, numeric rejected) ----------

    /// `{"retryDelay":"0.25s"} -> 250`
    #[test]
    fn json_retry_delay_0_25s_to_250() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, r#"{"retryDelay":"0.25s"}"#), Some(250));
    }

    /// `{"retryDelay":"45s"} -> 45000`
    #[test]
    fn json_retry_delay_45s_to_45000() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, r#"{"retryDelay":"45s"}"#), Some(45_000));
    }

    /// `{"retryDelay":"2m"} -> 120000`
    #[test]
    fn json_retry_delay_2m_to_120000() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, r#"{"retryDelay":"2m"}"#), Some(120_000));
    }

    /// `{"retryDelay":"500ms"} -> 500`
    #[test]
    fn json_retry_delay_500ms_to_500() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, r#"{"retryDelay":"500ms"}"#), Some(500));
    }

    /// `{"retryDelay":500} numeric -> rejected, falls through`
    #[test]
    fn json_retry_delay_numeric_rejected_falls_through() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, r#"{"retryDelay":500}"#), None);
    }

    /// `{"retryDelay":"soon"} no unit match -> rejected`
    #[test]
    fn json_retry_delay_soon_no_unit_match_rejected() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, r#"{"retryDelay":"soon"}"#), None);
    }

    /// "nested JSON max-wins: {outer:{retryDelay:5s}, inner:{retryDelay:60s}}"
    #[test]
    fn json_retry_delay_nested_max_wins() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, r#"{"a":{"retryDelay":"5s"},"b":{"retryDelay":"60s"}}"#), Some(60_000));
    }

    /// "retryDelay in array elements"
    #[test]
    fn json_retry_delay_in_array_elements() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, r#"[{"retryDelay":"3s"},{"retryDelay":"10s"}]"#), Some(10_000));
    }

    /// "SSE data: payload with retryDelay 45s -> 45000"
    #[test]
    fn json_retry_delay_sse_data_payload_45s() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(200), &empty, "data: {\"retryDelay\":\"45s\"}\n\n"), Some(45_000));
    }

    /// "SSE data: payload with rate_limit_error body and retryDelay"
    #[test]
    fn json_retry_delay_sse_data_payload_with_rate_limit_error_body() {
        let empty = HeaderMap::new();
        assert_eq!(
            hint(Some(200), &empty, "data: {\"error\":{\"type\":\"rate_limit_error\"},\"retryDelay\":\"45s\"}\n\n"),
            Some(45_000)
        );
    }

    // ---------- 6. Body prose patterns ----------

    /// "ms-marker: 'retry-after-ms: 5000'"
    #[test]
    fn body_prose_ms_marker() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, "error: retry-after-ms: 5000"), Some(5000));
    }

    /// "seconds-marker: 'retry-after: 30'"
    #[test]
    fn body_prose_seconds_marker() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, "retry-after: 30"), Some(30_000));
    }

    /// "relative prose: 'try again in 20 seconds'"
    #[test]
    fn body_prose_relative_try_again_in_20_seconds() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, "try again in 20 seconds"), Some(20_000));
    }

    /// "relative prose: 'retry in 5 seconds'"
    #[test]
    fn body_prose_relative_retry_in_5_seconds() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, "retry in 5 seconds"), Some(5_000));
    }

    /// "relative prose: 'wait after 10 seconds'"
    #[test]
    fn body_prose_relative_wait_after_10_seconds() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, "wait after 10 seconds"), Some(10_000));
    }

    /// "relative prose with minutes: 'try again in 3 minutes'"
    #[test]
    fn body_prose_relative_with_minutes() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, "try again in 3 minutes"), Some(180_000));
    }

    /// "relative prose with ms: 'try again in 500 ms'"
    #[test]
    fn body_prose_relative_with_ms() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, "try again in 500 ms"), Some(500));
    }

    /// "resets at <ISO8601-with-tz>"
    #[test]
    fn body_prose_resets_at_iso8601_with_tz() {
        let empty = HeaderMap::new();
        // 2023-11-14T22:14:20Z = 1700000060000, which is 60s after NOW.
        assert_eq!(hint(Some(429), &empty, "rate limit resets at 2023-11-14T22:14:20Z"), Some(60_000));
    }

    /// "resets at <ISO8601-with-tz> past -> 0"
    #[test]
    fn body_prose_resets_at_iso8601_with_tz_past() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, "rate limit resets at 2023-11-14T22:00:00Z"), Some(0));
    }

    /// "ms-marker beats seconds-marker (precedence)"
    #[test]
    fn body_prose_ms_marker_beats_seconds_marker() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, "retry-after: 30 retry-after-ms: 5000"), Some(5000));
    }

    // ---------- 7. Precedence: explicit zero beats lower-precedence positive ----------

    /// "retry-after: 0 + prose 30s -> 0"
    #[test]
    fn precedence_retry_after_0_plus_prose_30s() {
        assert_eq!(hint(Some(429), &headers(&[("retry-after", "0")]), "try again in 30 seconds"), Some(0));
    }

    /// "retry-after-ms: 0 + retry-after: 60 -> 0 (higher precedence wins with zero)"
    #[test]
    fn precedence_retry_after_ms_0_plus_retry_after_60() {
        assert_eq!(hint(Some(429), &headers(&[("retry-after-ms", "0"), ("retry-after", "60")]), ""), Some(0));
    }

    /// "x-ratelimit-reset epoch now + retryDelay 5s -> 0 (epoch now = 0 delta, beats retryDelay)"
    #[test]
    fn precedence_x_ratelimit_reset_epoch_now_plus_retry_delay_5s() {
        let h = headers(&[("x-ratelimit-reset", &(NOW / 1000).to_string())]);
        assert_eq!(hint(Some(429), &h, r#"{"retryDelay":"5s"}"#), Some(0));
    }

    // ---------- 8. Malformed / adversarial inputs ----------

    /// "garbage body text -> undefined"
    #[test]
    fn malformed_garbage_body_text() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, "asdfghjkl"), None);
    }

    /// "empty body -> undefined"
    #[test]
    fn malformed_empty_body() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, ""), None);
    }

    /// "garbage JSON object -> undefined"
    #[test]
    fn malformed_garbage_json_object() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, r#"{"foo":"bar","baz":[1,2,3]}"#), None);
    }

    /// "bodyText with retryDelay in a string value (not key) -> undefined"
    #[test]
    fn malformed_retry_delay_in_string_value_not_key() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, r#"{"message":"retryDelay is unknown"}"#), None);
    }

    /// "deeply nested retryDelay max-wins"
    #[test]
    fn malformed_deeply_nested_retry_delay_max_wins() {
        let empty = HeaderMap::new();
        assert_eq!(
            hint(Some(429), &empty, r#"{"a":{"b":{"c":{"retryDelay":"1s"}},"d":{"retryDelay":"100s"}}}"#),
            Some(100_000)
        );
    }

    /// "retryDelay with both s and ms: 45s and 500ms -> max 45000"
    #[test]
    fn malformed_retry_delay_with_both_s_and_ms_max_wins() {
        let empty = HeaderMap::new();
        assert_eq!(hint(Some(429), &empty, r#"{"a":{"retryDelay":"45s"},"b":{"retryDelay":"500ms"}}"#), Some(45_000));
    }

    /// "nowMs default (no parameter) returns a number when hint exists"
    #[test]
    fn malformed_now_ms_default_no_parameter_returns_number() {
        assert_eq!(hint_default_now(Some(429), &headers(&[("retry-after", "5")]), ""), Some(5_000));
    }

    // ---------- 9. Marker helpers ----------

    /// "appends marker to message"
    #[test]
    fn append_retry_after_ms_marker_appends_marker_to_message() {
        assert_eq!(append_retry_after_ms_marker("rate limited", 1258000), "rate limited (retry-after-ms: 1258000)");
    }

    /// "appends marker with zero"
    #[test]
    fn append_retry_after_ms_marker_appends_marker_with_zero() {
        assert_eq!(append_retry_after_ms_marker("rate limited", 0), "rate limited (retry-after-ms: 0)");
    }

    /// "parses marker from message"
    #[test]
    fn parse_retry_after_ms_marker_parses_marker_from_message() {
        assert_eq!(parse_retry_after_ms_marker("rate limited (retry-after-ms: 1258000)"), Some(1_258_000));
    }

    /// "parses zero marker"
    #[test]
    fn parse_retry_after_ms_marker_parses_zero_marker() {
        assert_eq!(parse_retry_after_ms_marker("rate limited (retry-after-ms: 0)"), Some(0));
    }

    /// "returns undefined when no marker"
    #[test]
    fn parse_retry_after_ms_marker_returns_undefined_when_no_marker() {
        assert_eq!(parse_retry_after_ms_marker("rate limited"), None);
    }

    /// "round-trip: append then parse"
    #[test]
    fn marker_round_trip_append_then_parse() {
        let msg = append_retry_after_ms_marker("some error", 45000);
        assert_eq!(parse_retry_after_ms_marker(&msg), Some(45_000));
    }
}
