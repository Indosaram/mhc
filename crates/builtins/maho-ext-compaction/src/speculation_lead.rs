//! Absolute lead, grace-band and warm-result staleness geometry.
pub const SPECULATION_LEAD_MIN_TOKENS: f64 = 8192.0;
pub const SPECULATION_LEAD_MAX_TOKENS: f64 = 32768.0;
pub const WARM_GENERATION_FLOOR_RATIO: f64 = 0.5;

pub fn resolve_speculation_lead_tokens(threshold_tokens: f64, configured_lead_tokens: Option<f64>) -> f64 {
    configured_lead_tokens.unwrap_or_else(|| (threshold_tokens * 0.125).floor()).clamp(SPECULATION_LEAD_MIN_TOKENS, SPECULATION_LEAD_MAX_TOKENS)
}
pub fn resolve_grace_band_cap_tokens(threshold_tokens: f64, lead_tokens: f64, context_window: f64, reserve_tokens: f64) -> f64 {
    (threshold_tokens + lead_tokens).min(context_window - reserve_tokens)
}
pub fn is_within_grace_band(context_tokens: f64, threshold_tokens: f64, lead_tokens: f64, context_window: f64, reserve_tokens: f64) -> bool {
    context_tokens < resolve_grace_band_cap_tokens(threshold_tokens, lead_tokens, context_window, reserve_tokens)
}
pub fn is_warm_result_stale(armed_at_tokens: f64, current_tokens: f64, keep_recent_tokens: f64) -> bool {
    current_tokens - armed_at_tokens > keep_recent_tokens.max(8192.0)
}
