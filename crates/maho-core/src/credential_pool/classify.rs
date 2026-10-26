//! Port of senpi packages/coding-agent/src/core/credential-pool/classify.ts.
//!
//! deviation: DEFAULT_SLOT_BLOCK_MS / MAX_SLOT_BLOCK_MS live in maho-ai's auth/pool/failover, which
//! is still the todo-13 stub; the two constants are declared here until that module lands.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::credential_pool::failover_consts::{
    DEFAULT_SLOT_BLOCK_MS, MAX_SLOT_BLOCK_MS, PROVIDER_NOT_CONFIGURED_PREFIX,
};

pub const COOLDOWN_BASE_MS: u64 = DEFAULT_SLOT_BLOCK_MS;
pub const COOLDOWN_CAP_MS: u64 = MAX_SLOT_BLOCK_MS;
pub const ACCOUNT_POOL_REFUSAL_NOTE: &str = "No request was sent: the account pool refused it locally.";
pub const RETRY_SAME_MAX_ATTEMPTS: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialBlock {
    AuthError,
    AccountDisabled,
    RateLimit {
        cooldown_ms: u64,
        retry_after_was_capped: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialAction {
    Failover { block: CredentialBlock },
    RetrySame { max_attempts: u32 },
    FailRequest,
}

#[derive(Debug, Clone, Default)]
pub struct CooldownPolicy {
    pub base_ms: Option<u64>,
    pub cap_ms: Option<u64>,
}

/// Per-slot exponential cooldown with the server hint as a FLOOR, never an override.
pub fn rate_limit_cooldown(
    failure_count: u32,
    server_hint_ms: Option<u64>,
    policy: &CooldownPolicy,
) -> (u64, bool) {
    let cap = policy.cap_ms.unwrap_or(COOLDOWN_CAP_MS);
    let base = policy.base_ms.unwrap_or(COOLDOWN_BASE_MS);
    let exponent = u32::min(failure_count, 63);
    let backoff = base.saturating_mul(1u64 << exponent).min(cap);
    let floored = match server_hint_ms {
        None => backoff,
        Some(hint) => backoff.max(hint),
    };
    (floored.min(cap), floored > cap)
}

/// The cooldown of a spent account whose limit names a future reset: exactly until that reset, capped.
pub fn usage_limit_cooldown(reset_ms: u64, policy: &CooldownPolicy) -> (u64, bool) {
    let cap = policy.cap_ms.unwrap_or(COOLDOWN_CAP_MS);
    (reset_ms.min(cap), reset_ms > cap)
}

#[derive(Debug, Clone, Default)]
pub struct ClassifyContext {
    pub failure_count: Option<u32>,
    pub cooldown_base_ms: Option<u64>,
    pub cooldown_cap_ms: Option<u64>,
}

fn regex(pattern: &str) -> Regex {
    Regex::new(&format!("(?i){pattern}")).expect("classify regex")
}

static AUTH_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"invalid[ _-]?(?:api[ _-]?)?key|authentication[_ ]?error|invalid x-api-key|unauthorized")
});
static FORBIDDEN_CREDENTIAL: LazyLock<Regex> =
    LazyLock::new(|| regex(r"account|credential|token|api[ _-]?key|organization|subscription"));
static BILLING: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"billing|credits?[ _-]?(?:required|exhausted|balance)|insufficient[ _-]?(?:funds|quota|credit)|payment[ _-]?required|quota[ _-]?exhausted",
    )
});
static RATE_LIMIT: LazyLock<Regex> =
    LazyLock::new(|| regex(r"rate[ _-]?limit|too many requests|resource_exhausted"));
static FAIL_REQUEST: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"context[ _-]?(?:length|window)|maximum context|invalid[ _-]?model|model[ _-]?not[ _-]?found|malformed[ _-]?stream|premature[ _-]?(?:close|stream)",
    )
});
static WEBSOCKET_POLICY_CLOSE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"websocket closed 100[89]\b"));
static WEBSOCKET_TRANSIENT: LazyLock<Regex> =
    LazyLock::new(|| regex(r"websocket (?:closed|error|connect timeout|liveness timeout)"));
static OVERLOADED: LazyLock<Regex> = LazyLock::new(|| regex(r"overloaded"));
static NETWORK: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"econnreset|econnrefused|etimedout|enotfound|socket hang up|fetch failed|network error|request timed out",
    )
});
// An account-scoped usage limit that reaches the pool as text alone (senpi#1768); anchored on exhaustion verbs.
static ACCOUNT_USAGE_LIMIT: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)usage_limit_reached|usage_not_included|usage limit has been reached|\b(?:hit|reached)\s+your\b[^.]*?\blimit\b|\busage\s+limit\s+reached\b|\b(?:Go|Free)UsageLimitError\b|\bblocking_limit\b|\brapid_refill_breaker\b|\bquota\s+exceeded\b",
    )
});
static TEMPORARY_FAULT: LazyLock<Regex> = LazyLock::new(|| regex(r"time[ds]?[ _-]?out|timed out|disconnect|reset before|connection reset|gateway|temporarily|unavailable"));
static ABORT: LazyLock<Regex> = LazyLock::new(|| regex(r"\baborted?\b"));

fn thrown_error(error: &Value) -> maho_ai::utils::error_body::ThrownProviderError {
    use maho_ai::utils::error_body::{SdkErrorShape, ThrownProviderError};
    let Some(object) = error.as_object() else {
        return ThrownProviderError::Other(error.clone());
    };
    if !object.contains_key("message")
        && !object.contains_key("status")
        && !object.contains_key("statusCode")
    {
        return ThrownProviderError::Other(error.clone());
    }
    ThrownProviderError::Error(Box::new(SdkErrorShape {
        message: object
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        status_code: object.get("statusCode").cloned(),
        status: object.get("status").cloned(),
        body: object.get("body").cloned(),
        error: object
            .get("error")
            .cloned()
            .map(maho_ai::utils::error_body::SdkFieldValue::Plain),
        metadata_http_status_code: object.get("metadataHttpStatusCode").cloned(),
        response_status_code: object.get("responseStatusCode").cloned(),
        response_body: object
            .get("responseBody")
            .cloned()
            .map(maho_ai::utils::error_body::SdkResponseBody::Value),
        ..SdkErrorShape::default()
    }))
}

pub fn classify_credential_failure(error: &Value, context: &ClassifyContext) -> CredentialAction {
    let normalized = maho_ai::utils::error_body::normalize_provider_error(&thrown_error(error));
    let text = format!(
        "{} {}",
        normalized.message,
        normalized.body.clone().unwrap_or_default()
    );
    let status = normalized.status;
    let failure_count = context.failure_count.unwrap_or(0);

    if is_abort(error, &text) || text.contains(ACCOUNT_POOL_REFUSAL_NOTE) {
        return CredentialAction::FailRequest;
    }
    let provider_fault = status == Some(408)
        || matches!(status, Some(code) if (500..600).contains(&code))
        || (status.is_none() && (TEMPORARY_FAULT.is_match(&text) || NETWORK.is_match(&text) || OVERLOADED.is_match(&text)));
    let usage_limit = ACCOUNT_USAGE_LIMIT.is_match(&text) && !is_overflow_text(&text);
    let limit_text = RATE_LIMIT.is_match(&text) || usage_limit;
    if text.contains(PROVIDER_NOT_CONFIGURED_PREFIX) {
        return CredentialAction::Failover {
            block: CredentialBlock::AuthError,
        };
    }
    if status == Some(401) || (!provider_fault && status != Some(429) && AUTH_ERROR.is_match(&text)) {
        return CredentialAction::Failover {
            block: CredentialBlock::AuthError,
        };
    }
    if status == Some(403) && !limit_text {
        return if FORBIDDEN_CREDENTIAL.is_match(&text) {
            CredentialAction::Failover {
                block: CredentialBlock::AuthError,
            }
        } else {
            CredentialAction::FailRequest
        };
    }
    if status == Some(402) || (!provider_fault && BILLING.is_match(&text)) {
        return CredentialAction::Failover {
            block: CredentialBlock::AccountDisabled,
        };
    }
    if status == Some(429) || limit_text {
        let mut hint = maho_ai::utils::retry_hint::extract_429_retry_after_ms(
            &maho_ai::utils::retry_hint::RetryHintInput {
                status: Some(status.unwrap_or(429)),
                headers: None,
                body_text: &text,
            },
            None,
        );
        if usage_limit && hint.is_none() {
            hint = normalized.retry_after_ms;
        }
        // A spent account names when it comes back in a JSON reset field or reset prose (senpi#3771).
        if usage_limit && hint.is_none() {
            let now_ms = maho_ai::utils::diagnostics::now_ms();
            hint = crate::credential_pool::reset_time::usage_limit_reset_ms(&text, now_ms);
        }
        let policy = CooldownPolicy {
            base_ms: context.cooldown_base_ms,
            cap_ms: context.cooldown_cap_ms,
        };
        // A spent account comes back at the reset it names, so that reset is the cooldown: an exponential
        // backoff grown from earlier failures would keep it blocked past it (senpi#3771). A reset already
        // past proves nothing and keeps the default floor.
        let (cooldown_ms, retry_after_was_capped) = match hint {
            Some(hint) if usage_limit && hint > 0 => usage_limit_cooldown(hint, &policy),
            _ => rate_limit_cooldown(failure_count, hint, &policy),
        };
        return CredentialAction::Failover {
            block: CredentialBlock::RateLimit {
                cooldown_ms,
                retry_after_was_capped,
            },
        };
    }
    if is_overflow_text(&text)
        || status == Some(400)
        || status == Some(404)
        || FAIL_REQUEST.is_match(&text)
    {
        return CredentialAction::FailRequest;
    }
    if WEBSOCKET_POLICY_CLOSE.is_match(&text) {
        return CredentialAction::FailRequest;
    }
    if WEBSOCKET_TRANSIENT.is_match(&text) {
        return CredentialAction::RetrySame {
            max_attempts: RETRY_SAME_MAX_ATTEMPTS,
        };
    }
    if status == Some(529)
        || OVERLOADED.is_match(&text)
        || matches!(status, Some(code) if (500..600).contains(&code))
    {
        return CredentialAction::RetrySame {
            max_attempts: RETRY_SAME_MAX_ATTEMPTS,
        };
    }
    if status == Some(408) || NETWORK.is_match(&text) {
        return CredentialAction::RetrySame {
            max_attempts: RETRY_SAME_MAX_ATTEMPTS,
        };
    }
    CredentialAction::FailRequest
}

fn is_abort(error: &Value, message: &str) -> bool {
    if error.get("name").and_then(Value::as_str) == Some("AbortError") {
        return true;
    }
    ABORT.is_match(message)
}

fn is_overflow_text(text: &str) -> bool {
    maho_ai::utils::overflow::get_overflow_patterns()
        .iter()
        .any(|pattern| pattern.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error(status: u64, message: &str) -> Value {
        json!({ "status": status, "message": message })
    }

    #[test]
    fn provider_fault_auth_words_do_not_disable_credentials() {
        let context = ClassifyContext::default();
        assert!(matches!(classify_credential_failure(&error(503, "unauthorized billing gateway"), &context), CredentialAction::RetrySame { .. }));
        assert!(matches!(classify_credential_failure(&error(429, "unauthorized"), &context), CredentialAction::Failover { block: CredentialBlock::RateLimit { .. } }));
        assert_eq!(classify_credential_failure(&error(401, ACCOUNT_POOL_REFUSAL_NOTE), &context), CredentialAction::FailRequest);
    }

    #[test]
    fn an_auth_failure_blocks_the_slot_permanently() {
        assert_eq!(
            classify_credential_failure(
                &error(401, "invalid api key"),
                &ClassifyContext::default()
            ),
            CredentialAction::Failover {
                block: CredentialBlock::AuthError
            }
        );
        assert_eq!(
            classify_credential_failure(
                &error(403, "this account is not allowed"),
                &ClassifyContext::default()
            ),
            CredentialAction::Failover {
                block: CredentialBlock::AuthError
            }
        );
        assert_eq!(
            classify_credential_failure(
                &error(403, "forbidden for another reason"),
                &ClassifyContext::default()
            ),
            CredentialAction::FailRequest
        );
    }

    #[test]
    fn billing_failures_disable_the_account() {
        assert_eq!(
            classify_credential_failure(
                &error(402, "payment required"),
                &ClassifyContext::default()
            ),
            CredentialAction::Failover {
                block: CredentialBlock::AccountDisabled
            }
        );
    }

    #[test]
    fn a_rate_limit_earns_an_exponential_cooldown_that_caps() {
        let (cooldown, capped) = rate_limit_cooldown(0, None, &CooldownPolicy::default());
        assert_eq!(cooldown, COOLDOWN_BASE_MS);
        assert!(!capped);
        let (cooldown, capped) = rate_limit_cooldown(3, None, &CooldownPolicy::default());
        assert_eq!(cooldown, COOLDOWN_BASE_MS * 8);
        assert!(!capped);
        let (cooldown, capped) =
            rate_limit_cooldown(0, Some(COOLDOWN_CAP_MS * 2), &CooldownPolicy::default());
        assert_eq!(cooldown, COOLDOWN_CAP_MS);
        assert!(capped);
        let (cooldown, _) = rate_limit_cooldown(
            0,
            Some(5),
            &CooldownPolicy {
                base_ms: Some(10),
                cap_ms: Some(20),
            },
        );
        assert_eq!(cooldown, 10);
    }

    #[test]
    fn a_usage_limit_cooldown_is_the_exact_reset_capped() {
        assert_eq!(
            usage_limit_cooldown(5_000, &CooldownPolicy::default()),
            (5_000, false)
        );
        assert_eq!(
            usage_limit_cooldown(COOLDOWN_CAP_MS + 1, &CooldownPolicy::default()),
            (COOLDOWN_CAP_MS, true)
        );
        assert_eq!(
            usage_limit_cooldown(
                50,
                &CooldownPolicy {
                    base_ms: None,
                    cap_ms: Some(20)
                }
            ),
            (20, true)
        );
    }

    #[test]
    fn a_usage_limit_with_a_named_reset_uses_it_exactly_without_backoff() {
        let context = ClassifyContext {
            failure_count: Some(4),
            ..ClassifyContext::default()
        };
        match classify_credential_failure(
            &error(
                429,
                "You've hit your usage limit. Please try again in 30 seconds",
            ),
            &context,
        ) {
            CredentialAction::Failover {
                block: CredentialBlock::RateLimit { cooldown_ms, .. },
            } => assert_eq!(cooldown_ms, 30_000),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_rate_limited_slot_fails_over_with_a_cooldown() {
        match classify_credential_failure(
            &error(429, "too many requests"),
            &ClassifyContext::default(),
        ) {
            CredentialAction::Failover {
                block:
                    CredentialBlock::RateLimit {
                        cooldown_ms,
                        retry_after_was_capped,
                    },
            } => {
                assert_eq!(cooldown_ms, COOLDOWN_BASE_MS);
                assert!(!retry_after_was_capped);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn provider_scoped_faults_retry_the_same_slot() {
        assert_eq!(
            classify_credential_failure(&error(503, "overloaded"), &ClassifyContext::default()),
            CredentialAction::RetrySame {
                max_attempts: RETRY_SAME_MAX_ATTEMPTS
            }
        );
        assert_eq!(
            classify_credential_failure(
                &error(408, "request timed out"),
                &ClassifyContext::default()
            ),
            CredentialAction::RetrySame {
                max_attempts: RETRY_SAME_MAX_ATTEMPTS
            }
        );
        assert_eq!(
            classify_credential_failure(
                &json!({ "message": "websocket closed 1006" }),
                &ClassifyContext::default()
            ),
            CredentialAction::RetrySame {
                max_attempts: RETRY_SAME_MAX_ATTEMPTS
            }
        );
    }

    #[test]
    fn request_scoped_faults_fail_the_request() {
        assert_eq!(
            classify_credential_failure(
                &error(400, "maximum context length exceeded"),
                &ClassifyContext::default()
            ),
            CredentialAction::FailRequest
        );
        assert_eq!(
            classify_credential_failure(
                &error(404, "model not found"),
                &ClassifyContext::default()
            ),
            CredentialAction::FailRequest
        );
        assert_eq!(
            classify_credential_failure(
                &json!({ "name": "AbortError", "message": "aborted" }),
                &ClassifyContext::default()
            ),
            CredentialAction::FailRequest
        );
        assert_eq!(
            classify_credential_failure(
                &json!({ "message": "websocket closed 1008" }),
                &ClassifyContext::default()
            ),
            CredentialAction::FailRequest
        );
    }
}
