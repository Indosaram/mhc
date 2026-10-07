pub const KIBITZER_BACKOFF_MIN_MS: i64 = 1_000;
pub const KIBITZER_BACKOFF_MAX_MS: i64 = 300_000;

pub fn backoff_delay_ms(
    attempt: usize,
    random: &(dyn Fn() -> f64 + Send + Sync),
    min_ms: i64,
    max_ms: i64,
) -> i64 {
    let exponent = attempt.min(31) as u32;
    let cap = max_ms.min(min_ms.saturating_mul(1_i64 << exponent));
    let jittered = (cap as f64) * (0.5 + 0.5 * clamp_unit(random()));
    (jittered.round() as i64).clamp(min_ms, max_ms)
}

fn clamp_unit(value: f64) -> f64 {
    if value.is_finite() { value.clamp(0.0, 1.0) } else { 0.5 }
}

#[cfg(test)]
#[path = "kibitzer_backoff_tests.rs"]
mod tests;
