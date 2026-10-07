//! When may the resident Kibitzer be woken? (latest `kibitzer/wake-policy.ts`, gate half.)
//!
//! SUPERSEDES `/tmp/wp.txt`: the backoff band lives in its own module (`kibitzer_backoff.rs`), so
//! this module owns only the wake gate and the accepted-nudge cooldown. `AcceptedNudgeCooldown::new`
//! takes the clock alone; the tunable limit/window form is `with_options`.

use std::collections::BTreeSet;
use std::sync::Arc;

use memory_core::recall::RecallCandidate;

/// Fixed advisory budget: at most two accepted-nudge wakes per ten minutes per main session.
pub const ACCEPTED_NUDGE_COOLDOWN_LIMIT: usize = 2;
pub const ACCEPTED_NUDGE_COOLDOWN_WINDOW_MS: i64 = 600_000;

/// The accepted-nudge cooldown; charged only when a wake actually delivered a nudge.
pub struct AcceptedNudgeCooldown {
    limit: usize,
    window_ms: i64,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    charged_at: Vec<i64>,
}

impl AcceptedNudgeCooldown {
    /// The default band (`ACCEPTED_NUDGE_COOLDOWN_LIMIT` / `_WINDOW_MS`).
    pub fn new(now: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self { limit: ACCEPTED_NUDGE_COOLDOWN_LIMIT, window_ms: ACCEPTED_NUDGE_COOLDOWN_WINDOW_MS, now, charged_at: Vec::new() }
    }

    /// An explicit limit/window (tests and non-default config).
    pub fn with_options(now: Arc<dyn Fn() -> i64 + Send + Sync>, limit: usize, window_ms: i64) -> Self {
        Self { limit, window_ms, now, charged_at: Vec::new() }
    }

    /// True while the window already holds `limit` charges: a fresh candidate buffers instead.
    pub fn exhausted(&mut self) -> bool {
        self.live().len() >= self.limit
    }

    /// Records one wake that delivered at least one accepted nudge.
    pub fn charge(&mut self) {
        let now = (self.now)();
        self.live().push(now);
    }

    /// Charges still inside the window.
    pub fn charges(&mut self) -> usize {
        self.live().len()
    }

    fn live(&mut self) -> &mut Vec<i64> {
        let start = (self.now)() - self.window_ms;
        self.charged_at.retain(|timestamp| *timestamp > start);
        &mut self.charged_at
    }
}

/// Why a batch stayed silent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeSilenceReason {
    MaxItemsZero,
    NoCandidates,
    NoNewCandidate,
    Cooldown,
}

/// The local gate's verdict.
#[derive(Clone, Debug, PartialEq)]
pub enum WakeDecision {
    /// The fresh, nudge-eligible candidates in batch order; the envelope carries exactly these.
    Wake(Vec<RecallCandidate>),
    Silent(WakeSilenceReason),
}

/// No model turn without something new to judge. Freshness is decided before the cooldown, so a
/// throttled session still reports honestly that nothing new arrived.
pub fn decide_wake(
    candidates: &[RecallCandidate],
    offered: &BTreeSet<String>,
    surfaced: &BTreeSet<String>,
    max_items: usize,
    cooldown_exhausted: bool,
) -> WakeDecision {
    if max_items == 0 {
        return WakeDecision::Silent(WakeSilenceReason::MaxItemsZero);
    }
    if candidates.is_empty() {
        return WakeDecision::Silent(WakeSilenceReason::NoCandidates);
    }
    let mut fresh = Vec::new();
    let mut seen = BTreeSet::new();
    for candidate in candidates {
        let path = &candidate.path;
        if seen.contains(path) || offered.contains(path) || surfaced.contains(path) || is_system_memory_path(path) {
            continue;
        }
        seen.insert(path.clone());
        fresh.push(candidate.clone());
    }
    if fresh.is_empty() {
        return WakeDecision::Silent(WakeSilenceReason::NoNewCandidate);
    }
    if cooldown_exhausted {
        return WakeDecision::Silent(WakeSilenceReason::Cooldown);
    }
    WakeDecision::Wake(fresh)
}

/// `system/` memories are never nudge-eligible, so they never wake either.
pub fn is_system_memory_path(path: &str) -> bool {
    path == "system/" || path.starts_with("system/")
}

#[cfg(test)]
#[path = "kibitzer_wake_policy_tests.rs"]
mod tests;
