//! Idle eligibility is separate from warming and compaction thresholds.
use maho_core::compaction::settings::CompactionSettings;
use maho_ext_api::ExtensionMode;
use crate::policy::{CompactionYield, should_trigger_compaction};
use crate::speculation_lead::WARM_GENERATION_FLOOR_RATIO;

pub const IDLE_COMPACTION_INSTRUCTIONS: &str = "Proactively compact at idle before the next agent turn, while the user is not waiting.";
pub struct IdleCompactionDecision<'a> {
    pub will_retry: bool,
    pub aborted: bool,
    pub settings: &'a CompactionSettings,
    pub tokens: Option<f64>,
    pub context_window: f64,
    pub breaker_tripped: bool,
    pub last_yield: Option<CompactionYield>,
    pub mode: ExtensionMode,
}
fn is_idle_eligible(decision: &IdleCompactionDecision<'_>) -> bool {
    !decision.will_retry && !decision.aborted
        && matches!(decision.mode, ExtensionMode::Tui | ExtensionMode::Rpc | ExtensionMode::AppServer)
        && decision.settings.enabled && decision.settings.idle_compaction_enabled != Some(false)
        && !decision.breaker_tripped && decision.tokens.is_some()
}
pub fn should_run_idle_compaction(decision: &IdleCompactionDecision<'_>) -> bool {
    is_idle_eligible(decision) && should_trigger_compaction(decision.tokens, decision.context_window, decision.settings, decision.last_yield)
}
pub fn should_warm_at_idle(decision: &IdleCompactionDecision<'_>) -> bool {
    is_idle_eligible(decision) && decision.tokens.is_some_and(|tokens| tokens >= decision.context_window * WARM_GENERATION_FLOOR_RATIO)
}
