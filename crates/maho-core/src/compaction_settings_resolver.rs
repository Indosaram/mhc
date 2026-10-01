//! Port of senpi packages/coding-agent/src/core/compaction-settings-resolver.ts.

use crate::compaction_settings_access::{
    CompactionModelSelector, CompactionSettings, compaction_keep_recent_tokens, compaction_reserve_tokens,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: i64,
    pub keep_recent_tokens: i64,
    pub speculative_enabled: bool,
    pub speculative_fraction: f64,
    pub speculative_cooldown_ms: f64,
    pub restoration_enabled: bool,
    pub restoration_max_items: f64,
    pub restoration_max_tokens_per_item: f64,
    pub restoration_max_total_tokens: f64,
    pub restoration_context_ratio: f64,
    pub idle_compaction_enabled: bool,
    pub grace_band_enabled: bool,
    pub tool_admission_enabled: bool,
    pub reminder_enabled: bool,
    pub reserve_scaling_enabled: bool,
    pub speculative_lead_tokens: Option<f64>,
    pub summarization_max_duration_ms: Option<f64>,
}

fn finite_number(value: Option<f64>, fallback: f64) -> f64 {
    match value {
        Some(value) if value.is_finite() => value.max(0.0),
        _ => fallback,
    }
}

pub fn resolve_compaction_settings(
    settings: Option<&CompactionSettings>,
    for_model: Option<CompactionModelSelector<'_>>,
) -> Result<ResolvedCompactionSettings, String> {
    let enabled = settings.and_then(|settings| settings.enabled).unwrap_or(true);
    let speculative_lead_tokens = settings
        .and_then(|settings| settings.speculative_lead_tokens)
        .filter(|value| (*value as f64).is_finite())
        .map(|value| (value as f64).max(0.0));
    let summarization_max_duration_ms = settings
        .and_then(|settings| settings.summarization_max_duration_ms)
        .filter(|value| value.is_finite() && *value > 0.0);
    Ok(ResolvedCompactionSettings {
        enabled,
        reserve_tokens: compaction_reserve_tokens(settings, for_model)?,
        keep_recent_tokens: compaction_keep_recent_tokens(settings, for_model)?,
        speculative_enabled: settings.and_then(|settings| settings.speculative_enabled).unwrap_or(true),
        speculative_fraction: finite_number(settings.and_then(|settings| settings.speculative_fraction), 0.75),
        speculative_cooldown_ms: finite_number(settings.and_then(|settings| settings.speculative_cooldown_ms).map(|v| v as f64), 30000.0),
        restoration_enabled: settings.and_then(|settings| settings.restoration_enabled).unwrap_or(true),
        restoration_max_items: finite_number(settings.and_then(|settings| settings.restoration_max_items).map(|v| v as f64), 10.0),
        restoration_max_tokens_per_item: finite_number(settings.and_then(|settings| settings.restoration_max_tokens_per_item).map(|v| v as f64), 5000.0),
        restoration_max_total_tokens: finite_number(settings.and_then(|settings| settings.restoration_max_total_tokens).map(|v| v as f64), 50000.0),
        restoration_context_ratio: finite_number(settings.and_then(|settings| settings.restoration_context_ratio), 0.15),
        idle_compaction_enabled: settings.and_then(|settings| settings.idle_compaction_enabled).unwrap_or(true),
        grace_band_enabled: settings.and_then(|settings| settings.grace_band_enabled).unwrap_or(true),
        tool_admission_enabled: settings.and_then(|settings| settings.tool_admission_enabled).unwrap_or(true),
        reminder_enabled: settings.and_then(|settings| settings.reminder_enabled).unwrap_or(true),
        reserve_scaling_enabled: settings.and_then(|settings| settings.reserve_scaling_enabled).unwrap_or(true),
        speculative_lead_tokens,
        summarization_max_duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_senpi() {
        let resolved = resolve_compaction_settings(None, None).expect("resolved");
        assert!(resolved.enabled);
        assert_eq!(resolved.reserve_tokens, 16384);
        assert_eq!(resolved.keep_recent_tokens, 20000);
        assert!((resolved.speculative_fraction - 0.75).abs() < f64::EPSILON);
        assert!((resolved.speculative_cooldown_ms - 30000.0).abs() < f64::EPSILON);
        assert_eq!(resolved.restoration_max_items, 10.0);
        assert!((resolved.restoration_context_ratio - 0.15).abs() < f64::EPSILON);
        assert!(resolved.grace_band_enabled);
        assert!(resolved.reserve_scaling_enabled);
        assert!(resolved.summarization_max_duration_ms.is_none());
    }

    #[test]
    fn explicit_values_win_and_invalid_ones_clamp_to_the_default() {
        let settings = CompactionSettings {
            enabled: Some(false),
            speculative_fraction: Some(f64::NAN),
            restoration_max_items: Some(3),
            summarization_max_duration_ms: Some(120000.0),
            ..Default::default()
        };
        let resolved = resolve_compaction_settings(Some(&settings), None).expect("resolved");
        assert!(!resolved.enabled);
        assert!((resolved.speculative_fraction - 0.75).abs() < f64::EPSILON);
        assert_eq!(resolved.restoration_max_items, 3.0);
        assert_eq!(resolved.summarization_max_duration_ms, Some(120000.0));
    }

    #[test]
    fn an_invalid_token_budget_surfaces_as_an_error() {
        let settings = CompactionSettings { reserve_tokens: Some(-1), ..Default::default() };
        assert!(resolve_compaction_settings(Some(&settings), None).is_err());
    }
}
