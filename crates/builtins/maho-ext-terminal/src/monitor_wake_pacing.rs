//! Per-monitor wake pacing: a token bucket of `rate` tokens refilled at `rate` an hour, one token
//! per wake. Monitors without a rate are unpaced and always ready.
use std::collections::BTreeMap;

const HOUR_MS: f64 = 3_600_000.0;

struct Bucket {
    tokens: f64,
    at: f64,
}

#[derive(Default)]
pub struct WakePacer {
    buckets: BTreeMap<String, Bucket>,
}


fn valid_rate(rate: Option<f64>) -> Option<f64> {
    rate.filter(|rate| rate.is_finite() && *rate >= 1.0)
}

impl WakePacer {
    pub fn new() -> Self {
        Self::default()
    }

    fn refill(&mut self, id: &str, rate: Option<f64>, now: f64) -> Option<(f64, f64)> {
        let rate = valid_rate(rate)?;
        let bucket = self.buckets.entry(id.to_owned()).or_insert(Bucket {
            tokens: rate,
            at: now,
        });
        bucket.tokens = rate.min(bucket.tokens + (now - bucket.at) * rate / HOUR_MS);
        bucket.at = now;
        Some((bucket.tokens, rate))
    }

    /// When `id` next holds a whole token: `now` for an unpaced monitor or one with a token left.
    pub fn ready_at(&mut self, id: &str, rate: Option<f64>, now: f64) -> f64 {
        match self.refill(id, rate, now) {
            Some((tokens, rate)) if tokens < 1.0 => now + ((1.0 - tokens) * HOUR_MS / rate).ceil(),
            _ => now,
        }
    }

    pub fn spend(&mut self, id: &str, rate: Option<f64>, now: f64) {
        if self.refill(id, rate, now).is_some()
            && let Some(bucket) = self.buckets.get_mut(id)
        {
            bucket.tokens = (bucket.tokens - 1.0).max(0.0);
        }
    }

    pub fn forget(&mut self, id: &str) {
        self.buckets.remove(id);
    }

    pub fn clear(&mut self) {
        self.buckets.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpaced_monitors_are_always_ready() {
        let mut pacer = WakePacer::new();
        for rate in [None, Some(0.5), Some(f64::NAN), Some(f64::INFINITY)] {
            pacer.spend("a", rate, 0.0);
            assert_eq!(pacer.ready_at("a", rate, 10.0), 10.0);
        }
    }

    #[test]
    fn burst_then_refill_at_rate_per_hour() {
        let mut pacer = WakePacer::new();
        for _ in 0..2 {
            assert_eq!(pacer.ready_at("a", Some(2.0), 0.0), 0.0);
            pacer.spend("a", Some(2.0), 0.0);
        }
        assert_eq!(pacer.ready_at("a", Some(2.0), 0.0), 1_800_000.0);
        assert_eq!(pacer.ready_at("a", Some(2.0), 1_800_000.0), 1_800_000.0);
        pacer.forget("a");
        assert_eq!(pacer.ready_at("a", Some(2.0), 0.0), 0.0);
    }
}
