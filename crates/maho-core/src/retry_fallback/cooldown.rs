use regex::Regex;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Default)]
pub struct SelectorFailure<'a> {
    pub retry_after_ms: Option<f64>,
    pub error_message: Option<&'a str>,
}

pub struct SelectorCooldowns {
    expires_at_by_selector: HashMap<String, f64>,
    now: Box<dyn Fn() -> f64 + Send + Sync>,
    random: Box<dyn Fn() -> f64 + Send + Sync>,
    patterns: Vec<(Regex, f64)>,
}

impl SelectorCooldowns {
    pub fn new(
        now: impl Fn() -> f64 + Send + Sync + 'static,
        random: impl Fn() -> f64 + Send + Sync + 'static,
    ) -> Result<Self, regex::Error> {
        let patterns = [
            (r"usage[- ]limit|quota|insufficient_quota|billing|credits[-_ ]required|credits are required", 1_800_000.0),
            (r"rate[ -]?limit|429|too many requests", 30_000.0),
            (r"overloaded|capacity", 45_000.0),
            (r"5xx|\b5\d\d\b|server|internal error", 20_000.0),
            (r"timed? out|timeout|econnreset|econnrefused|etimedout|socket hang up|socket connection was closed|network.?error|connection.?error|connection.?refused|connection.?lost|other side closed|fetch failed|getaddrinfo|enotfound|eai_again|upstream.?connect|reset before headers|terminated|websocket.?closed|websocket.?error|ended without|stream ended before message_stop|stream ended before a terminal response event|http2 request did not get a response", 60_000.0),
        ].into_iter().map(|(pattern, duration)| Regex::new(pattern).map(|regex| (regex, duration))).collect::<Result<_, _>>()?;
        Ok(Self { expires_at_by_selector: HashMap::new(), now: Box::new(now), random: Box::new(random), patterns })
    }

    pub fn note(&mut self, base_selector: &str, failure: SelectorFailure<'_>) {
        let duration = self.duration_for(failure);
        self.expires_at_by_selector.insert(base_selector.to_owned(), (self.now)() + duration);
    }

    pub fn is_suppressed(&mut self, base_selector: &str) -> bool {
        let Some(expires_at) = self.expires_at_by_selector.get(base_selector) else { return false; };
        if (self.now)() < *expires_at { return true; }
        self.expires_at_by_selector.remove(base_selector);
        false
    }

    pub fn clear(&mut self, base_selector: &str) { self.expires_at_by_selector.remove(base_selector); }
    pub fn clear_all(&mut self) { self.expires_at_by_selector.clear(); }

    fn duration_for(&self, failure: SelectorFailure<'_>) -> f64 {
        if let Some(duration) = failure.retry_after_ms.filter(|n| n.is_finite() && *n > 0.0) { return duration; }
        let message = failure.error_message.unwrap_or_default().to_lowercase();
        for (index, (pattern, duration)) in self.patterns.iter().enumerate() {
            if pattern.is_match(&message) {
                return duration + if index == 2 { ((self.random)().clamp(0.0, 1.0) * 30_000.0).round() } else { 0.0 };
            }
        }
        300_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicU32, Ordering}};

    fn clock(jitter: f64) -> (Arc<AtomicU32>, SelectorCooldowns) {
        let time = Arc::new(AtomicU32::new(0));
        let read = Arc::clone(&time);
        let cooldowns = SelectorCooldowns::new(move || f64::from(read.load(Ordering::SeqCst)), move || jitter).unwrap();
        (time, cooldowns)
    }

    #[test]
    fn positive_retry_after_precedes_quota() {
        let (time, mut cooldowns) = clock(0.0);
        time.store(1_000, Ordering::SeqCst);
        cooldowns.note("a/m", SelectorFailure { retry_after_ms: Some(5_000.0), error_message: Some("quota exceeded") });
        time.store(5_999, Ordering::SeqCst);
        assert!(cooldowns.is_suppressed("a/m"));
        time.store(6_000, Ordering::SeqCst);
        assert!(!cooldowns.is_suppressed("a/m"));
    }

    #[test]
    fn duration_classes_expire_at_their_boundaries() {
        for (message, duration) in [
            ("usage limit reached", 1_800_000), ("insufficient_quota", 1_800_000), ("Billing limit reached", 1_800_000),
            ("429 Usage credits are required for this model.", 1_800_000), ("429 credits_required", 1_800_000),
            ("rate limit exceeded", 30_000), ("HTTP 429", 30_000), ("Too Many Requests", 30_000),
            ("service overloaded", 45_000), ("capacity unavailable", 45_000), ("HTTP 503", 20_000),
            ("server error", 20_000), ("internal error", 20_000), ("Request timed out.", 60_000),
            ("fetch failed", 60_000), ("Network connection lost.", 60_000), ("getaddrinfo ENOTFOUND", 60_000),
            ("HTTP 500 internal error - request timed out", 20_000), ("Stream ended without finish_reason", 60_000),
            ("Anthropic stream ended before message_stop", 60_000), ("stream ended before a terminal response event", 60_000),
            ("http2 request did not get a response", 60_000), ("weird provider hiccup", 300_000), ("", 300_000),
        ] {
            let (time, mut cooldowns) = clock(0.0);
            cooldowns.note("a/m", SelectorFailure { error_message: Some(message), ..Default::default() });
            time.store(duration - 1, Ordering::SeqCst);
            assert!(cooldowns.is_suppressed("a/m"), "{message}");
            time.store(duration, Ordering::SeqCst);
            assert!(!cooldowns.is_suppressed("a/m"), "{message}");
        }
    }

    #[test]
    fn capacity_jitter_reaches_thirty_seconds() {
        let (time, mut cooldowns) = clock(1.0);
        cooldowns.note("a/m", SelectorFailure { error_message: Some("capacity exhausted"), ..Default::default() });
        time.store(74_999, Ordering::SeqCst);
        assert!(cooldowns.is_suppressed("a/m"));
        time.store(75_000, Ordering::SeqCst);
        assert!(!cooldowns.is_suppressed("a/m"));
    }

    #[test]
    fn expired_selectors_are_evicted() {
        let (time, mut cooldowns) = clock(0.0);
        cooldowns.note("a/m", SelectorFailure { retry_after_ms: Some(100.0), ..Default::default() });
        time.store(99, Ordering::SeqCst);
        assert!(cooldowns.is_suppressed("a/m"));
        time.store(100, Ordering::SeqCst);
        assert!(!cooldowns.is_suppressed("a/m"));
        assert!(!cooldowns.is_suppressed("a/m"));
    }

    #[test]
    fn clear_one_or_all_selectors() {
        let (_, mut cooldowns) = clock(0.0);
        for selector in ["a/m", "b/m"] { cooldowns.note(selector, SelectorFailure { retry_after_ms: Some(100.0), ..Default::default() }); }
        cooldowns.clear("a/m");
        assert!(!cooldowns.is_suppressed("a/m"));
        assert!(cooldowns.is_suppressed("b/m"));
        cooldowns.clear_all();
        assert!(!cooldowns.is_suppressed("b/m"));
    }
}
