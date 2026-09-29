//! Port of senpi packages/ai/src/utils/retry-profile/planner.ts.

use super::backoff::retry_backoff_delay_ms;
use super::types::{RetryFailure, RetryServerHintPolicy, RetryStagePolicy};

#[derive(Debug, Clone, PartialEq)]
pub enum RetryPlanResult {
    Wait { delay_ms: f64 },
    OverCeiling { requested_ms: u64 },
    Tiered { tier: String, delay_ms: f64 },
}

pub fn plan_retry_delay(stage: &RetryStagePolicy, failure: &RetryFailure, retry_number: u32, random: f64) -> RetryPlanResult {
    let computed = retry_backoff_delay_ms(&stage.backoff, retry_number, random);
    let hint = failure.retry_after_ms;
    match &stage.server_hint {
        RetryServerHintPolicy::Tiered { .. } => match hint {
            None => RetryPlanResult::Wait { delay_ms: computed },
            Some(_) => RetryPlanResult::Tiered { tier: "delegated".into(), delay_ms: computed },
        },
        RetryServerHintPolicy::Override { accept_zero, ceiling } => match hint {
            Some(hint) if hint > 0 || *accept_zero => match ceiling.max_delay_ms {
                Some(max) if hint > max => RetryPlanResult::OverCeiling { requested_ms: hint },
                _ => RetryPlanResult::Wait { delay_ms: hint as f64 },
            },
            _ => RetryPlanResult::Wait { delay_ms: computed },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::retry_profile::types::{RetryClassification, RetryFailureKind, RetryHintCeiling, RetryHintExceeded};

    // kimi-code-like stage: 500ms base, x2, 32s cap, +0..25% additive jitter.
    fn kimi_backoff() -> crate::utils::retry_profile::types::RetryBackoffPolicy {
        crate::utils::retry_profile::types::RetryBackoffPolicy {
            base_delay_ms: 500.0,
            growth_factor: 2.0,
            per_attempt_cap_ms: Some(32_000.0),
            jitter: crate::utils::retry_profile::types::RetryJitterPolicy::Additive { ratio: 0.25 },
        }
    }

    fn extract_hint(failure: &RetryFailure) -> Option<u64> {
        failure.retry_after_ms
    }

    // Kimi stage: override mode, zero hints rejected, no ceiling.
    fn kimi_stage() -> RetryStagePolicy {
        RetryStagePolicy {
            enabled: true,
            max_retries: 5,
            backoff: kimi_backoff(),
            extract_server_hint: extract_hint,
            server_hint: RetryServerHintPolicy::Override {
                accept_zero: false,
                ceiling: RetryHintCeiling { max_delay_ms: None, on_exceeded: RetryHintExceeded::ErrorWithMarker },
            },
            classify: |_| RetryClassification::Transient,
        }
    }

    // Senpi provider-request stage: override mode with a 60s ceiling; hints
    // strictly above it must surface as over-ceiling (error-with-marker).
    fn senpi_provider_stage() -> RetryStagePolicy {
        RetryStagePolicy {
            enabled: true,
            max_retries: 3,
            backoff: crate::utils::retry_profile::types::RetryBackoffPolicy {
                base_delay_ms: 500.0,
                growth_factor: 2.0,
                per_attempt_cap_ms: Some(8_000.0),
                jitter: crate::utils::retry_profile::types::RetryJitterPolicy::Subtractive { ratio: 0.25 },
            },
            extract_server_hint: extract_hint,
            server_hint: RetryServerHintPolicy::Override {
                accept_zero: false,
                ceiling: RetryHintCeiling { max_delay_ms: Some(60_000), on_exceeded: RetryHintExceeded::ErrorWithMarker },
            },
            classify: |_| RetryClassification::RateLimited,
        }
    }

    // Tiered stage: the real tier engine lives in coding-agent's hint-policy
    // module, so the planner just flags the hint's presence as "delegated".
    fn tiered_stage() -> RetryStagePolicy {
        RetryStagePolicy {
            enabled: true,
            max_retries: 3,
            backoff: kimi_backoff(),
            extract_server_hint: extract_hint,
            server_hint: RetryServerHintPolicy::Tiered {
                strategy: std::sync::Arc::new(|_, _, _, _| {
                    Ok(crate::utils::retry_profile::types::RetryTieredHintDecision { tier: "senpi:probe".into(), delay_ms: 1.0 })
                }),
            },
            classify: |_| RetryClassification::RateLimited,
        }
    }

    fn failure_with(retry_after_ms: Option<u64>) -> RetryFailure {
        RetryFailure {
            origin: "test".into(),
            kind: RetryFailureKind::HttpStatus,
            message: "429".into(),
            status_code: Some(429),
            provider_codes: None,
            finish_reason: None,
            retry_after_ms,
            should_retry: None,
        }
    }

    #[test]
    fn kimi_override_stage_positive_hint_replaces_the_computed_delay_entirely() {
        assert_eq!(plan_retry_delay(&kimi_stage(), &failure_with(Some(42)), 1, 0.0), RetryPlanResult::Wait { delay_ms: 42.0 });
    }

    #[test]
    fn kimi_override_stage_accept_zero_false_zero_hint_falls_back_to_computed_backoff() {
        assert_eq!(plan_retry_delay(&kimi_stage(), &failure_with(Some(0)), 1, 0.0), RetryPlanResult::Wait { delay_ms: 500.0 });
    }

    #[test]
    fn kimi_override_stage_absent_hint_falls_back_to_computed_backoff() {
        assert_eq!(plan_retry_delay(&kimi_stage(), &failure_with(None), 1, 0.0), RetryPlanResult::Wait { delay_ms: 500.0 });
    }

    #[test]
    fn kimi_override_stage_with_null_ceiling_an_hour_long_hint_is_honoured_verbatim() {
        assert_eq!(plan_retry_delay(&kimi_stage(), &failure_with(Some(3_600_000)), 1, 0.0), RetryPlanResult::Wait { delay_ms: 3_600_000.0 });
    }

    #[test]
    fn senpi_provider_override_stage_hint_strictly_above_the_60s_ceiling_is_over_ceiling() {
        assert_eq!(plan_retry_delay(&senpi_provider_stage(), &failure_with(Some(90_000)), 1, 0.0), RetryPlanResult::OverCeiling { requested_ms: 90_000 });
    }

    #[test]
    fn senpi_provider_override_stage_hint_within_the_ceiling_is_used_as_is() {
        assert_eq!(plan_retry_delay(&senpi_provider_stage(), &failure_with(Some(5_000)), 1, 0.0), RetryPlanResult::Wait { delay_ms: 5_000.0 });
    }

    #[test]
    fn tiered_stage_present_hint_is_delegated() {
        assert_eq!(
            plan_retry_delay(&tiered_stage(), &failure_with(Some(7_000)), 2, 0.0),
            RetryPlanResult::Tiered { tier: "delegated".into(), delay_ms: 1_000.0 }
        );
    }

    #[test]
    fn tiered_stage_absent_hint_waits_on_the_computed_backoff() {
        assert_eq!(plan_retry_delay(&tiered_stage(), &failure_with(None), 1, 0.0), RetryPlanResult::Wait { delay_ms: 500.0 });
    }

    #[test]
    fn plans_override_ceiling_zero_and_tiered_against_shipped_profiles() {
        let request = &crate::utils::retry_profile::profiles::SENPI_DEFAULT_RETRY_PROFILE.provider_request;
        assert_eq!(plan_retry_delay(request, &failure_with(Some(0)), 1, 0.0), RetryPlanResult::Wait { delay_ms: 0.0 });
        assert_eq!(plan_retry_delay(request, &failure_with(Some(70_000)), 1, 0.0), RetryPlanResult::OverCeiling { requested_ms: 70_000 });
        assert_eq!(plan_retry_delay(request, &failure_with(None), 2, 0.0), RetryPlanResult::Wait { delay_ms: 1000.0 });
        let turn = &crate::utils::retry_profile::profiles::SENPI_DEFAULT_RETRY_PROFILE.turn;
        assert_eq!(
            plan_retry_delay(turn, &failure_with(Some(5)), 1, 0.0),
            RetryPlanResult::Tiered { tier: "delegated".into(), delay_ms: 2000.0 }
        );
        let kimi = &crate::utils::retry_profile::profiles::KIMI_CODE_RETRY_PROFILE.turn;
        assert_eq!(plan_retry_delay(kimi, &failure_with(Some(0)), 1, 0.0), RetryPlanResult::Wait { delay_ms: 500.0 });
        assert_eq!(plan_retry_delay(kimi, &failure_with(Some(999_999)), 1, 0.0), RetryPlanResult::Wait { delay_ms: 999_999.0 });
    }
}
