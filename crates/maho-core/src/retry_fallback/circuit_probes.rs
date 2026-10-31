//! Port of senpi packages/coding-agent/src/core/retry-fallback/circuit-probes.ts.

use std::sync::LazyLock;

use regex::{Regex, RegexBuilder};

use super::billing::is_billing_error_message;
use super::circuit::{
    CircuitAdmission, CircuitFailureInput, CircuitLane, FallbackCircuitAccess, ProbeToken,
};

/// senpi ai `QUOTA_EXHAUSTION_PATTERNS` (`utils/retry.ts`): account quota, budget, credit, and billing
/// exhaustion, matched after provider request ids are stripped.
static QUOTA_EXHAUSTION_PATTERN: LazyLock<Option<Regex>> = LazyLock::new(|| {
    let mut patterns: Vec<String> = [
        "GoUsageLimitError",
        "FreeUsageLimitError",
        "Monthly usage limit reached",
        "available balance",
        "insufficient_quota",
        "out of budget",
        "quota exceeded",
        "billing",
        "credits_required",
        "credits are required",
    ]
    .map(str::to_owned)
    .to_vec();
    patterns.extend(
        maho_ai::utils::retry::USAGE_LIMIT_EXHAUSTION
            .markers
            .iter()
            .map(|marker| (*marker).to_owned()),
    );
    patterns.push(regex::escape(
        maho_ai::utils::retry::ANTHROPIC_THIRD_PARTY_BILLING_MARKER,
    ));
    RegexBuilder::new(&patterns.join("|"))
        .case_insensitive(true)
        .build()
        .ok()
});

/// senpi ai `isQuotaExhaustionMessage`.
pub fn is_quota_exhaustion_message(error_message: Option<&str>) -> bool {
    let Some(error_message) = error_message else {
        return false;
    };
    let stripped = maho_ai::utils::retry::strip_provider_request_ids(error_message);
    QUOTA_EXHAUSTION_PATTERN
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(&stripped))
}

/// Non-retryable failures that still say the entry is unusable for everyone: billing and account
/// quota/budget exhaustion. Auth and request-shape rejections are not outages and never open a circuit.
pub fn is_health_exhaustion_failure(error_message: Option<&str>) -> bool {
    is_billing_error_message(error_message) || is_quota_exhaustion_message(error_message)
}

/// One session's side of the shared breaker: at most one probe token, taken by atomic admission before
/// the session switches to (or starts on) an entry, and given back on every terminal path.
pub struct CircuitProbes {
    circuits: Option<FallbackCircuitAccess>,
    token: Option<ProbeToken>,
}

impl CircuitProbes {
    pub fn new(circuits: Option<FallbackCircuitAccess>) -> Self {
        Self {
            circuits,
            token: None,
        }
    }

    pub fn is_open(&self, selector: &str) -> bool {
        self.circuits
            .as_ref()
            .is_some_and(|circuits| circuits.is_open(selector, CircuitLane::Turn))
    }

    pub fn governs(&self, selector: &str) -> bool {
        self.circuits
            .as_ref()
            .is_some_and(|circuits| circuits.governs(selector))
    }

    pub fn is_exhausted(&self, selector: &str) -> bool {
        self.circuits
            .as_ref()
            .is_some_and(|circuits| circuits.is_exhausted(selector))
    }

    pub fn admit(&mut self, selector: &str) -> &'static str {
        let admission = self
            .circuits
            .as_ref()
            .map_or(CircuitAdmission::Closed, |circuits| {
                circuits.admit(selector, CircuitLane::Turn)
            });
        let kind = admission.kind();
        if let CircuitAdmission::Probe { token } = admission
            && self.token.as_ref().map(|held| held.selector.as_str()) != Some(selector)
        {
            self.release();
            self.token = Some(token);
        }
        kind
    }

    pub fn holds(&self, selector: &str) -> bool {
        self.probing() == Some(selector)
    }

    pub fn probing(&self) -> Option<&str> {
        self.token.as_ref().map(|token| token.selector.as_str())
    }

    pub fn note_failure(&mut self, selector: &str, failure: CircuitFailureInput<'_>) {
        if let Some(circuits) = &self.circuits {
            circuits.note_failure(selector, failure);
        }
        if self.holds(selector) {
            self.token = None;
        }
    }

    pub fn accept(&mut self, selector: &str) {
        if let Some(circuits) = &self.circuits {
            circuits.close(selector);
        }
        if self.holds(selector) {
            self.token = None;
        }
    }

    pub fn release(&mut self) {
        if let (Some(token), Some(circuits)) = (&self.token, &self.circuits) {
            circuits.release(token);
        }
        self.token = None;
    }

    pub fn release_all(&mut self) {
        self.token = None;
        if let Some(circuits) = &self.circuits {
            circuits.release_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retry_fallback::circuit::{
        FallbackCircuitAccessDeps, FallbackCircuitBreaker, ResolvedFallbackCircuitSettings,
    };
    use std::sync::{Arc, Mutex};

    #[test]
    fn treats_billing_and_quota_exhaustion_as_health_failures_but_not_rate_limits() {
        assert!(is_health_exhaustion_failure(Some("insufficient_quota")));
        assert!(is_health_exhaustion_failure(Some(
            "Your credit balance is too low"
        )));
        assert!(is_health_exhaustion_failure(Some(
            "The usage limit has been reached"
        )));
        assert!(!is_health_exhaustion_failure(Some("429 rate limit")));
        assert!(!is_health_exhaustion_failure(Some("401 unauthorized")));
        assert!(!is_health_exhaustion_failure(None));
    }

    fn probes(now: Arc<Mutex<f64>>) -> CircuitProbes {
        let breaker = Arc::new(Mutex::new(FallbackCircuitBreaker::new()));
        CircuitProbes::new(Some(FallbackCircuitAccess::new(
            FallbackCircuitAccessDeps {
                breaker,
                owner: Arc::new(|| "session".to_owned()),
                now: Arc::new(move || now.lock().map(|now| *now).unwrap_or(0.0)),
                wall_now: None,
                settings: Arc::new(|| ResolvedFallbackCircuitSettings {
                    cooldown_ms: 1_000.0,
                    max_cooldown_ms: 3_000.0,
                }),
                logger: None,
            },
        )))
    }

    #[test]
    fn holds_one_probe_token_and_gives_it_back_on_accept() {
        let now = Arc::new(Mutex::new(0.0));
        let mut probes = probes(now.clone());
        assert_eq!(probes.admit("p/a"), "closed");
        probes.note_failure("p/a", CircuitFailureInput::default());
        assert!(probes.is_open("p/a"));
        assert!(probes.governs("p/a"));
        if let Ok(mut now) = now.lock() {
            *now = 1_000.0;
        }
        assert_eq!(probes.admit("p/a"), "probe");
        assert!(probes.holds("p/a"));
        probes.accept("p/a");
        assert_eq!(probes.probing(), None);
        assert!(!probes.governs("p/a"));
    }

    #[test]
    fn without_a_breaker_every_entry_is_closed() {
        let mut probes = CircuitProbes::new(None);
        assert_eq!(probes.admit("p/a"), "closed");
        assert!(!probes.is_open("p/a"));
        assert!(!probes.is_exhausted("p/a"));
    }
}
