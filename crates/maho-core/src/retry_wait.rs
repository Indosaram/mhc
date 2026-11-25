//! Port of senpi `packages/coding-agent/src/core/retry-wait.ts`.

use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryWaitKind {
    RateLimit,
    Retry,
}

/// The wait an agent-level auto-retry sleeps through, published on `session_status` and `list_sessions`
/// so a supervisor can tell "waiting on a provider until T" from work (senpi#7025). It exists only
/// between an `auto_retry_start` and the retried request's `agent_start` (or the retry's end).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryWait {
    pub kind: RetryWaitKind,
    pub attempt: u64,
    pub max_attempts: u64,
    pub delay_ms: u64,
    pub until: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

const ERROR_CHARS: usize = 200;

static RATE_LIMITED: LazyLock<Option<Regex>> = LazyLock::new(|| {
    RegexBuilder::new(
        r"rate.?limit|usage.?limit|quota|too many requests|resource.?exhausted|(?:^|\b(?:HTTP/1\.[01]|HTTP|status(?: code)?|error|code)\s+)429\b",
    )
    .case_insensitive(true)
    .build()
    .ok()
});

pub fn is_rate_limited_error_message(error_message: &str) -> bool {
    RATE_LIMITED
        .as_ref()
        .is_some_and(|re| re.is_match(error_message))
}

#[derive(Debug, Clone)]
pub enum RetryWaitEvent<'a> {
    AutoRetryStart {
        attempt: u64,
        max_attempts: u64,
        delay_ms: u64,
        error_message: &'a str,
    },
    AgentStart,
    AutoRetryEnd,
    Other,
}

fn format_iso_millis(millis: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(millis)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default())
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn next_retry_wait(
    previous: Option<RetryWait>,
    event: RetryWaitEvent<'_>,
    now_ms: i64,
) -> Option<RetryWait> {
    match event {
        RetryWaitEvent::AutoRetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => {
            let truncated: String = error_message.chars().take(ERROR_CHARS).collect();
            let until_ms = now_ms.saturating_add(i64::try_from(delay_ms).unwrap_or(i64::MAX));
            Some(RetryWait {
                kind: if is_rate_limited_error_message(error_message) {
                    RetryWaitKind::RateLimit
                } else {
                    RetryWaitKind::Retry
                },
                attempt,
                max_attempts,
                delay_ms,
                until: format_iso_millis(until_ms),
                error: if truncated.is_empty() {
                    None
                } else {
                    Some(truncated)
                },
            })
        }
        RetryWaitEvent::AgentStart | RetryWaitEvent::AutoRetryEnd => None,
        RetryWaitEvent::Other => previous,
    }
}

/// The wait as a listing publishes it: the typed kind and its timing, never the provider's error text,
/// which stays on `get_state` and on `session_status`.
pub fn redacted_retry_wait(wait: &RetryWait) -> RetryWait {
    RetryWait {
        kind: wait.kind,
        attempt: wait.attempt,
        max_attempts: wait.max_attempts,
        delay_ms: wait.delay_ms,
        until: wait.until.clone(),
        error: None,
    }
}
