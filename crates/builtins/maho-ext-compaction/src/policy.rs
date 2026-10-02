//! Adaptive thresholds and reserve geometry from builtin/compaction/policy.ts.
use maho_core::compaction::settings::CompactionSettings;

pub const SPECULATIVE_FRACTION: f64 = 0.75;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompactionYield {
    pub saved_tokens: f64,
    pub tokens_before: f64,
}

fn clamp_threshold_ratio(ratio: f64) -> f64 {
    ratio.clamp(0.4, 0.85)
}

pub fn base_threshold_ratio_for_window(context_window: f64) -> f64 {
    if context_window.is_nan() || context_window <= 0.0 { return 0.5; }
    if context_window <= 16_000.0 { return 0.45; }
    if context_window <= 32_000.0 { return 0.5; }
    if context_window <= 64_000.0 { return 0.55; }
    if context_window <= 128_000.0 { return 0.6; }
    if context_window <= 512_000.0 { return 0.7; }
    0.8
}

pub fn resolve_reserve_tokens(context_window: f64, configured_reserve: f64) -> f64 {
    configured_reserve.max((0.04 * context_window).floor().min(49_152.0))
}

pub fn resolve_effective_reserve_tokens(context_window: f64, reserve_tokens: f64, reserve_scaling_enabled: Option<bool>) -> f64 {
    if reserve_scaling_enabled == Some(false) { reserve_tokens }
    else { resolve_reserve_tokens(context_window, reserve_tokens) }
}

pub fn compute_adaptive_threshold_ratio(context_window: f64, prior_saved_tokens: Option<f64>) -> f64 {
    let ratio = base_threshold_ratio_for_window(context_window);
    let Some(saved) = prior_saved_tokens else { return ratio; };
    if context_window <= 0.0 { return ratio; }
    let saved_ratio = saved / context_window;
    if saved_ratio > 0.5 { clamp_threshold_ratio(ratio - 0.05) }
    else if saved_ratio < 0.1 { ratio.min(clamp_threshold_ratio(ratio + 0.05)) }
    else { ratio }
}

pub fn compute_effective_threshold(context_window: f64, last_yield: Option<CompactionYield>) -> f64 {
    let mut ratio = compute_adaptive_threshold_ratio(context_window, None);
    if let Some(last) = last_yield.filter(|last| last.tokens_before > 0.0) {
        let saved_ratio = last.saved_tokens / last.tokens_before;
        if saved_ratio > 0.5 { ratio -= 0.05; }
        else if saved_ratio < 0.1 { ratio = base_threshold_ratio_for_window(context_window).min(ratio + 0.05); }
    }
    clamp_threshold_ratio(ratio)
}

pub fn compute_effective_keep_recent_tokens(setting: f64, context_window: f64, threshold_ratio: f64, margin: f64) -> f64 {
    let scaled = if context_window > 409_600.0 && setting >= 10_000.0 {
        setting.max((0.05 * context_window).floor().min(60_000.0))
    } else { setting };
    let capped = (context_window * (1.0 - threshold_ratio - margin)).floor();
    scaled.min(capped.max(1024.0))
}

pub fn should_start_speculative_compaction(tokens: Option<f64>, context_window: f64, settings: &CompactionSettings, last_yield: Option<CompactionYield>, lead_tokens: Option<f64>) -> bool {
    let Some(tokens) = tokens else { return false; };
    if settings.speculative_enabled == Some(false) || context_window <= 0.0 { return false; }
    let threshold = context_window * compute_effective_threshold(context_window, last_yield);
    if let Some(lead) = lead_tokens.filter(|lead| lead.is_finite() && *lead > 0.0) {
        return tokens >= (threshold - lead).max(0.0);
    }
    tokens >= threshold * settings.speculative_fraction.unwrap_or(SPECULATIVE_FRACTION)
}

pub fn is_at_hard_limit(tokens: Option<f64>, context_window: f64, reserve_tokens: f64, additional_tokens: f64) -> bool {
    tokens.is_some_and(|tokens| tokens + additional_tokens + reserve_tokens >= context_window)
}

pub fn should_trigger_compaction(tokens: Option<f64>, context_window: f64, settings: &CompactionSettings, last_yield: Option<CompactionYield>) -> bool {
    settings.enabled && context_window > 0.0 && tokens.is_some_and(|tokens| tokens >= context_window * compute_effective_threshold(context_window, last_yield))
}
