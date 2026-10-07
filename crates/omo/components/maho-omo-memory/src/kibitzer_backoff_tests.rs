use super::*;
use std::sync::Arc;

fn fixed(value: f64) -> Arc<dyn Fn() -> f64 + Send + Sync> {
    Arc::new(move || value)
}

#[test]
fn given_the_first_attempt_when_jitter_is_zero_then_the_floor_is_used() {
    assert_eq!(backoff_delay_ms(0, &*fixed(0.0), KIBITZER_BACKOFF_MIN_MS, KIBITZER_BACKOFF_MAX_MS), 1_000);
}

#[test]
fn given_a_deep_streak_when_computed_then_the_cap_is_never_passed() {
    for attempt in 0..40 {
        let delay = backoff_delay_ms(attempt, &*fixed(1.0), KIBITZER_BACKOFF_MIN_MS, KIBITZER_BACKOFF_MAX_MS);
        assert!((KIBITZER_BACKOFF_MIN_MS..=KIBITZER_BACKOFF_MAX_MS).contains(&delay), "attempt {attempt}: {delay}");
    }
    assert_eq!(backoff_delay_ms(20, &*fixed(1.0), KIBITZER_BACKOFF_MIN_MS, KIBITZER_BACKOFF_MAX_MS), 300_000);
}

#[test]
fn given_a_second_attempt_when_computed_then_the_band_doubled() {
    assert_eq!(backoff_delay_ms(1, &*fixed(1.0), KIBITZER_BACKOFF_MIN_MS, KIBITZER_BACKOFF_MAX_MS), 2_000);
    assert_eq!(backoff_delay_ms(1, &*fixed(0.0), KIBITZER_BACKOFF_MIN_MS, KIBITZER_BACKOFF_MAX_MS), 1_000);
}

#[test]
fn given_a_non_finite_jitter_when_computed_then_it_defaults_to_the_midpoint() {
    assert_eq!(backoff_delay_ms(0, &*fixed(f64::NAN), KIBITZER_BACKOFF_MIN_MS, KIBITZER_BACKOFF_MAX_MS), 1_000);
    assert_eq!(backoff_delay_ms(1, &*fixed(f64::INFINITY), KIBITZER_BACKOFF_MIN_MS, KIBITZER_BACKOFF_MAX_MS), 1_500);
}
