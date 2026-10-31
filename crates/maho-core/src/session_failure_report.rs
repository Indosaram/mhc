//! Port of senpi `packages/coding-agent/src/core/session-failure-report.ts`.

use serde_json::Value;

/// Providers only cache a prompt prefix above a model-specific minimum size, so a zero cache read on a
/// small prompt is not evidence of a lost cache. One fixed threshold for every provider.
pub const MIN_CACHEABLE_PROMPT_TOKENS: u64 = 2048;

/// What provider failures cost a session: every provider response is one request.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SessionFailureReport {
    pub requests: u64,
    pub errored_requests: u64,
    pub aborted_requests: u64,
    pub failure_share: f64,
    /// Wall time from each failed request's start to its recorded end.
    pub failed_duration_ms: u64,
    /// The first successful response after one or more failed requests within the same user turn.
    pub post_failure_requests: u64,
    /// Those retries that read nothing from the prompt cache on a prompt of at least the threshold.
    pub post_failure_full_miss_requests: u64,
    /// Uncached prompt tokens (input + cache writes) of those retries.
    pub post_failure_full_miss_input_tokens: u64,
}

fn request_duration_ms(recorded_at: Option<&str>, started_at: Option<f64>) -> u64 {
    let ended_at = recorded_at
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.timestamp_millis() as f64);
    match (ended_at, started_at) {
        (Some(end), Some(start)) if start.is_finite() && end > start => (end - start) as u64,
        _ => 0,
    }
}

fn usage(message: &Value, key: &str) -> u64 {
    message
        .get("usage")
        .and_then(|usage| usage.get(key))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

pub fn compute_session_failure_report(entries: &[Value]) -> SessionFailureReport {
    let mut report = SessionFailureReport::default();
    let mut previous_failed = false;
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(message) = entry.get("message") else {
            continue;
        };
        match message.get("role").and_then(Value::as_str) {
            Some("user") => {
                previous_failed = false;
                continue;
            }
            Some("assistant") => {}
            _ => continue,
        }
        report.requests += 1;
        let stop_reason = message.get("stopReason").and_then(Value::as_str);
        let failed = matches!(stop_reason, Some("error" | "aborted"));
        if failed {
            if stop_reason == Some("error") {
                report.errored_requests += 1;
            } else {
                report.aborted_requests += 1;
            }
            report.failed_duration_ms += request_duration_ms(
                entry.get("timestamp").and_then(Value::as_str),
                message.get("timestamp").and_then(Value::as_f64),
            );
        } else if previous_failed {
            report.post_failure_requests += 1;
            let uncached = usage(message, "input") + usage(message, "cacheWrite");
            if usage(message, "cacheRead") == 0 && uncached >= MIN_CACHEABLE_PROMPT_TOKENS {
                report.post_failure_full_miss_requests += 1;
                report.post_failure_full_miss_input_tokens += uncached;
            }
        }
        previous_failed = failed;
    }
    report.failure_share = if report.requests > 0 {
        (report.errored_requests + report.aborted_requests) as f64 / report.requests as f64
    } else {
        0.0
    };
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(stop: &str, started: i64, recorded: &str, input: u64, cache_read: u64) -> Value {
        json!({"type":"message","timestamp":recorded,"message":{"role":"assistant","stopReason":stop,"timestamp":started,
            "usage":{"input":input,"output":1,"cacheRead":cache_read,"cacheWrite":0}}})
    }

    #[test]
    fn counts_failures_and_full_miss_retries() {
        let entries = vec![
            json!({"type":"message","message":{"role":"user"}}),
            assistant("error", 0, "1970-01-01T00:00:02.000Z", 0, 0),
            assistant("aborted", 0, "1970-01-01T00:00:01.000Z", 0, 0),
            assistant("stop", 0, "1970-01-01T00:00:03.000Z", 5000, 0),
            assistant("stop", 0, "1970-01-01T00:00:03.000Z", 5000, 0),
        ];
        let report = compute_session_failure_report(&entries);
        assert_eq!(report.requests, 4);
        assert_eq!((report.errored_requests, report.aborted_requests), (1, 1));
        assert_eq!(report.failed_duration_ms, 3000);
        assert_eq!(report.post_failure_requests, 1);
        assert_eq!(report.post_failure_full_miss_requests, 1);
        assert_eq!(report.post_failure_full_miss_input_tokens, 5000);
        assert!((report.failure_share - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn a_user_message_ends_the_retry_chain_and_small_or_cached_prompts_are_not_misses() {
        let entries = vec![
            assistant("error", 0, "bad", 0, 0),
            json!({"type":"message","message":{"role":"user"}}),
            assistant("stop", 0, "bad", 5000, 0),
            assistant("error", 0, "bad", 0, 0),
            assistant("stop", 0, "bad", 100, 0),
            assistant("error", 0, "bad", 0, 0),
            assistant("stop", 0, "bad", 5000, 10),
        ];
        let report = compute_session_failure_report(&entries);
        assert_eq!(report.post_failure_requests, 2);
        assert_eq!(report.post_failure_full_miss_requests, 0);
        assert_eq!(report.failed_duration_ms, 0);
        assert_eq!(compute_session_failure_report(&[]).failure_share, 0.0);
    }
}
