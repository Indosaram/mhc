//! Port of senpi `anthropic-subscription/claude-cli-login-outcome.ts` (v2026.11.19-5).
use maho_ai::utils::abort::{AbortSignal, WeakAbortSignal};
use serde_json::Value;
use std::sync::Mutex;

pub struct ClaudeCliLoginOutcome {
    pub account_name: String,
    pub check: Value,
}

static OUTCOMES: Mutex<Vec<(WeakAbortSignal, ClaudeCliLoginOutcome)>> = Mutex::new(Vec::new());

/// A login's outcome is visible only to the operation with the same signal.
pub fn record_claude_cli_login_outcome(signal: &AbortSignal, outcome: ClaudeCliLoginOutcome) {
    let mut outcomes = OUTCOMES.lock().unwrap_or_else(|error| error.into_inner());
    outcomes.retain(|(key, _)| key.upgrade().is_some_and(|key| !key.same_signal(signal)));
    outcomes.push((signal.downgrade(), outcome));
}

pub fn take_claude_cli_login_outcome(
    signal: Option<&AbortSignal>,
) -> Option<ClaudeCliLoginOutcome> {
    let signal = signal?;
    let mut outcomes = OUTCOMES.lock().unwrap_or_else(|error| error.into_inner());
    outcomes.retain(|(key, _)| key.upgrade().is_some());
    let index = outcomes
        .iter()
        .position(|(key, _)| key.upgrade().is_some_and(|key| key.same_signal(signal)))?;
    Some(outcomes.swap_remove(index).1)
}
