//! Port of retry-fallback/hint-policy.ts. Time is supplied by the caller.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintTier {
    NoHintFastFallback,
    Tier1InTurn,
    Tier2FallbackProbeBack,
    Tier3FallbackOnly,
}

#[derive(Debug, Clone, Copy)]
pub struct HintPolicySettings {
    pub hinted_wait_cap_ms: f64,
    pub probe_back_max_ms: f64,
}

pub fn classify_rate_limited_wait(hint_ms: Option<f64>, settings: HintPolicySettings) -> HintTier {
    match hint_ms {
        None => HintTier::NoHintFastFallback,
        Some(hint) if hint <= settings.hinted_wait_cap_ms => HintTier::Tier1InTurn,
        Some(hint) if hint < settings.probe_back_max_ms => HintTier::Tier2FallbackProbeBack,
        Some(_) => HintTier::Tier3FallbackOnly,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbeBackSchedule {
    pub first_at_ms: f64,
    pub deadline_ms: f64,
}

pub fn probe_back_schedule(hint_ms: f64, now_ms: f64) -> ProbeBackSchedule {
    ProbeBackSchedule { first_at_ms: now_ms + (hint_ms / 2.0).ceil(), deadline_ms: now_ms + hint_ms }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbePhase { Idle, HalfUsed, Done }

#[derive(Debug, Clone, Copy)]
pub struct InTurnState {
    pub probe_phase: ProbePhase,
    pub hint_deadline_ms: Option<f64>,
    pub attempt: u32,
    pub cumulative_hinted_wait_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InTurnResult {
    pub delay_ms: f64,
    pub probe_phase: ProbePhase,
    pub hint_deadline_ms: Option<f64>,
    pub cumulative_hinted_wait_ms: f64,
    pub demote_to_probe_back: bool,
}

pub fn next_in_turn_delay_ms(
    state: InTurnState, hint_ms: Option<f64>, base_delay_ms: f64,
    hinted_wait_cap_ms: f64, now_ms: f64,
) -> InTurnResult {
    let floor = base_delay_ms * 2.0_f64.powf(f64::from(state.attempt) - 1.0);
    let (delay, phase, deadline, counted) = match (state.probe_phase, hint_ms) {
        (ProbePhase::HalfUsed, hint) => {
            let deadline = hint.map(|h| now_ms + h).or(state.hint_deadline_ms).unwrap_or(now_ms);
            ((deadline - now_ms).max(0.0).max(floor), ProbePhase::Done, Some(deadline), true)
        }
        (ProbePhase::Idle, Some(hint)) => (
            (hint / 2.0).ceil().max(floor), ProbePhase::HalfUsed, Some(now_ms + hint), true,
        ),
        (ProbePhase::Idle, None) | (ProbePhase::Done, _) => (
            hint_ms.unwrap_or(0.0).max(floor), ProbePhase::Done, state.hint_deadline_ms, false,
        ),
    };
    let cumulative = state.cumulative_hinted_wait_ms + if counted { delay } else { 0.0 };
    InTurnResult {
        delay_ms: delay, probe_phase: phase, hint_deadline_ms: deadline,
        cumulative_hinted_wait_ms: cumulative,
        demote_to_probe_back: counted && cumulative > hinted_wait_cap_ms,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DegradedRateLimitAction { InTurn { delay_ms: f64 }, Fail { hint_ms: f64 } }

pub fn degrade_without_fallback(
    tier: HintTier, hint_ms: Option<f64>, attempt: u32, base_delay_ms: f64, hinted_wait_cap_ms: f64,
) -> DegradedRateLimitAction {
    let floor = base_delay_ms * 2.0_f64.powf(f64::from(attempt) - 1.0);
    match tier {
        HintTier::Tier3FallbackOnly => DegradedRateLimitAction::Fail { hint_ms: hint_ms.unwrap_or(0.0) },
        HintTier::Tier2FallbackProbeBack => DegradedRateLimitAction::InTurn {
            delay_ms: hint_ms.unwrap_or(hinted_wait_cap_ms).min(hinted_wait_cap_ms).max(floor),
        },
        HintTier::NoHintFastFallback | HintTier::Tier1InTurn => DegradedRateLimitAction::InTurn { delay_ms: floor },
    }
}
