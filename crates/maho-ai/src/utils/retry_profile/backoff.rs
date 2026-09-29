//! Port of senpi packages/ai/src/utils/retry-profile/backoff.ts.

use super::types::{RetryBackoffPolicy, RetryJitterPolicy};

pub fn retry_backoff_delay_ms(policy: &RetryBackoffPolicy, retry_number: u32, random: f64) -> f64 {
    let exponential = policy.base_delay_ms * policy.growth_factor.powf(f64::from(retry_number) - 1.0);
    let capped = policy.per_attempt_cap_ms.map_or(exponential, |cap| exponential.min(cap));
    match policy.jitter {
        RetryJitterPolicy::None => capped,
        RetryJitterPolicy::Additive { ratio } => capped + random * ratio * capped,
        RetryJitterPolicy::Subtractive { ratio } => capped - random * ratio * capped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // kimi-code turn policy: 500ms base, x2, 32s per-attempt cap, +0..25%
    // additive jitter. Mirrors test/retry-profile-backoff.test.ts kimiTurnBackoff.
    const KIMI_TURN_BACKOFF: RetryBackoffPolicy = RetryBackoffPolicy {
        base_delay_ms: 500.0,
        growth_factor: 2.0,
        per_attempt_cap_ms: Some(32_000.0),
        jitter: RetryJitterPolicy::Additive { ratio: 0.25 },
    };

    // senpi-default turn policy: 2000ms base, x2, uncapped, no jitter.
    const SENPI_TURN_BACKOFF: RetryBackoffPolicy = RetryBackoffPolicy {
        base_delay_ms: 2000.0,
        growth_factor: 2.0,
        per_attempt_cap_ms: None,
        jitter: RetryJitterPolicy::None,
    };

    // senpi provider-request stage: 500ms base, x2, 8s cap, -0..25%
    // subtractive jitter.
    const SENPI_PROVIDER_BACKOFF: RetryBackoffPolicy = RetryBackoffPolicy {
        base_delay_ms: 500.0,
        growth_factor: 2.0,
        per_attempt_cap_ms: Some(8_000.0),
        jitter: RetryJitterPolicy::Subtractive { ratio: 0.25 },
    };

    #[test]
    fn computes_kimi_like_exponential_ramp_capped_at_32s_when_jitter_adds_zero() {
        assert_eq!(retry_backoff_delay_ms(&KIMI_TURN_BACKOFF, 1, 0.0), 500.0);
        let delays: Vec<f64> = (1..=9).map(|n| retry_backoff_delay_ms(&KIMI_TURN_BACKOFF, n, 0.0)).collect();
        assert_eq!(delays, vec![500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0, 32000.0, 32000.0, 32000.0]);
    }

    #[test]
    fn applies_additive_jitter_after_the_cap() {
        assert_eq!(retry_backoff_delay_ms(&KIMI_TURN_BACKOFF, 1, 1.0), 625.0);
        // Attempt 7: capped base is 32000 (cap BEFORE jitter), jitter adds 25%.
        assert_eq!(retry_backoff_delay_ms(&KIMI_TURN_BACKOFF, 7, 1.0), 40_000.0);
    }

    #[test]
    fn computes_senpi_turn_delays_for_any_random_sample_jitter_none() {
        for random in [0.0, 0.5, 0.999_999] {
            let delays: Vec<f64> = (1..=3).map(|n| retry_backoff_delay_ms(&SENPI_TURN_BACKOFF, n, random)).collect();
            assert_eq!(delays, vec![2000.0, 4000.0, 8000.0]);
        }
        // Uncapped policy keeps growing past the kimi ramp's attempt-7 plateau.
        assert_eq!(retry_backoff_delay_ms(&SENPI_TURN_BACKOFF, 4, 0.25), 16_000.0);
    }

    #[test]
    fn applies_subtractive_jitter() {
        assert_eq!(retry_backoff_delay_ms(&SENPI_PROVIDER_BACKOFF, 1, 1.0), 375.0);
        // Subtractive jitter with random=0 leaves the capped base untouched.
        assert_eq!(retry_backoff_delay_ms(&SENPI_PROVIDER_BACKOFF, 3, 0.0), 2000.0);
    }

    #[test]
    fn treats_per_attempt_cap_ms_zero_as_a_literal_zero_cap_for_every_retry_number() {
        let zero_cap = RetryBackoffPolicy {
            base_delay_ms: 500.0,
            growth_factor: 2.0,
            per_attempt_cap_ms: Some(0.0),
            jitter: RetryJitterPolicy::Additive { ratio: 0.25 },
        };
        let delays: Vec<f64> = (1..=5).map(|n| retry_backoff_delay_ms(&zero_cap, n, 0.5)).collect();
        assert_eq!(delays, vec![0.0, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn honours_a_non_doubling_growth_factor_on_the_1_based_exponent() {
        let policy = RetryBackoffPolicy { base_delay_ms: 100.0, growth_factor: 3.0, per_attempt_cap_ms: None, jitter: RetryJitterPolicy::None };
        assert_eq!(retry_backoff_delay_ms(&policy, 1, 0.0), 100.0);
        assert_eq!(retry_backoff_delay_ms(&policy, 2, 0.0), 300.0);
        assert_eq!(retry_backoff_delay_ms(&policy, 3, 0.0), 900.0);
    }

    #[test]
    fn caps_computed_local_exponential_at_8s_for_high_attempts() {
        let policy = RetryBackoffPolicy { base_delay_ms: 2000.0, growth_factor: 2.0, per_attempt_cap_ms: Some(8_000.0), jitter: RetryJitterPolicy::None };
        let delays: Vec<f64> = (1..=5).map(|n| retry_backoff_delay_ms(&policy, n, 0.0)).collect();
        assert_eq!(delays, vec![2000.0, 4000.0, 8000.0, 8000.0, 8000.0]);
    }

    #[test]
    fn cap_applies_before_jitter() {
        let policy = RetryBackoffPolicy { base_delay_ms: 2000.0, growth_factor: 2.0, per_attempt_cap_ms: Some(8_000.0), jitter: RetryJitterPolicy::Additive { ratio: 0.25 } };
        assert_eq!(retry_backoff_delay_ms(&policy, 4, 1.0), 8_000.0 + 0.25 * 8_000.0);
    }
}
