//! Port of senpi packages/ai/src/utils/retry-profile/profiles.ts.

use super::classifiers::{classify_kimi_failure, classify_senpi_assistant_failure};
use super::types::{
    FallbackRateLimited, FallbackTerminal, FallbackTransient, RetryBackoffPolicy, RetryFailure, RetryFallbackPolicy, RetryHintCeiling,
    RetryHintExceeded, RetryJitterPolicy, RetryPolicyProfile, RetryServerHintPolicy, RetryStagePolicy,
};
use std::sync::{Arc, LazyLock};

fn extract_normalized_hint(failure: &RetryFailure) -> Option<u64> {
    failure.retry_after_ms
}

fn backoff(base_delay_ms: f64, cap: f64, jitter: RetryJitterPolicy) -> RetryBackoffPolicy {
    RetryBackoffPolicy { base_delay_ms, growth_factor: 2.0, per_attempt_cap_ms: Some(cap), jitter }
}

fn override_hint(accept_zero: bool, max_delay_ms: Option<u64>) -> RetryServerHintPolicy {
    RetryServerHintPolicy::Override { accept_zero, ceiling: RetryHintCeiling { max_delay_ms, on_exceeded: RetryHintExceeded::ErrorWithMarker } }
}

pub static SENPI_DEFAULT_RETRY_PROFILE: LazyLock<RetryPolicyProfile> = LazyLock::new(|| RetryPolicyProfile {
    id: "senpi-default",
    provider_request: RetryStagePolicy {
        enabled: true,
        max_retries: 0,
        backoff: backoff(500.0, 8_000.0, RetryJitterPolicy::Subtractive { ratio: 0.25 }),
        extract_server_hint: extract_normalized_hint,
        server_hint: override_hint(true, Some(60_000)),
        classify: classify_senpi_assistant_failure,
    },
    turn: RetryStagePolicy {
        enabled: true,
        max_retries: 5,
        backoff: backoff(2_000.0, 8_000.0, RetryJitterPolicy::Additive { ratio: 0.25 }),
        extract_server_hint: extract_normalized_hint,
        server_hint: RetryServerHintPolicy::Tiered {
            strategy: Arc::new(|_, _, _, _| Err("senpi-default turn tier strategy must be injected by coding-agent".to_owned())),
        },
        classify: classify_senpi_assistant_failure,
    },
    fallback: RetryFallbackPolicy {
        terminal: FallbackTerminal::ImmediateIfEligible,
        transient: FallbackTransient::AfterTurnBudget,
        rate_limited: FallbackRateLimited::Tiered,
        reset_budget_on_model_change: true,
    },
});

pub static KIMI_CODE_RETRY_PROFILE: LazyLock<RetryPolicyProfile> = LazyLock::new(|| {
    let stage = |enabled, max_retries| RetryStagePolicy {
        enabled,
        max_retries,
        backoff: backoff(500.0, 32_000.0, RetryJitterPolicy::Additive { ratio: 0.25 }),
        extract_server_hint: extract_normalized_hint,
        server_hint: override_hint(false, None),
        classify: classify_kimi_failure,
    };
    RetryPolicyProfile {
        id: "kimi-code",
        provider_request: stage(false, 0),
        turn: stage(true, 9),
        fallback: RetryFallbackPolicy {
            terminal: FallbackTerminal::ImmediateIfEligible,
            transient: FallbackTransient::AfterTurnBudget,
            rate_limited: FallbackRateLimited::AfterTurnBudget,
            reset_budget_on_model_change: true,
        },
    }
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::retry_profile::types::RetryFailureKind;

    #[test]
    fn pins_the_shipped_profile_ids_and_retry_budgets() {
        let senpi = &*SENPI_DEFAULT_RETRY_PROFILE;
        let kimi = &*KIMI_CODE_RETRY_PROFILE;
        assert_eq!(senpi.id, "senpi-default");
        assert_eq!(kimi.id, "kimi-code");
        assert_eq!(senpi.provider_request.max_retries, 0);
        assert_eq!(senpi.turn.max_retries, 5);
        assert!(!kimi.provider_request.enabled);
        assert_eq!(kimi.turn.max_retries, 9);
    }

    #[test]
    fn senpi_default_turn_backoff_caps_at_8s() {
        assert_eq!(SENPI_DEFAULT_RETRY_PROFILE.turn.backoff.per_attempt_cap_ms, Some(8_000.0));
    }

    #[test]
    fn profile_values_match_senpi() {
        let senpi = &*SENPI_DEFAULT_RETRY_PROFILE;
        assert_eq!((senpi.id, senpi.provider_request.max_retries, senpi.turn.max_retries), ("senpi-default", 0, 5));
        assert_eq!(senpi.fallback.rate_limited, FallbackRateLimited::Tiered);
        let RetryServerHintPolicy::Tiered { strategy } = &senpi.turn.server_hint else { panic!("tiered") };
        let failure = RetryFailure {
            origin: "t".into(),
            kind: RetryFailureKind::Unknown,
            message: String::new(),
            status_code: None,
            provider_codes: None,
            finish_reason: None,
            retry_after_ms: Some(3),
            should_retry: None,
        };
        assert_eq!(strategy(&failure, 1, &senpi.turn.backoff, 0).err().as_deref(), Some("senpi-default turn tier strategy must be injected by coding-agent"));
        assert_eq!((senpi.turn.extract_server_hint)(&failure), Some(3));
        let kimi = &*KIMI_CODE_RETRY_PROFILE;
        assert_eq!((kimi.id, kimi.provider_request.enabled, kimi.turn.max_retries), ("kimi-code", false, 9));
        assert_eq!(kimi.turn.backoff.per_attempt_cap_ms, Some(32_000.0));
        assert_eq!(kimi.fallback.rate_limited, FallbackRateLimited::AfterTurnBudget);
    }
}
