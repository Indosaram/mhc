//! Port of senpi `packages/coding-agent/src/core/compaction/compaction-settings.ts`.

use super::ideal_settings::{IdealCompactionSettings, default_ideal_compaction_settings};

/// `CompactionSettings`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: i64,
    pub keep_recent_tokens: i64,
    pub speculative_enabled: Option<bool>,
    pub speculative_fraction: Option<f64>,
    pub speculative_cooldown_ms: Option<i64>,
    pub restoration_enabled: Option<bool>,
    pub restoration_max_items: Option<i64>,
    pub restoration_max_tokens_per_item: Option<i64>,
    pub restoration_max_total_tokens: Option<i64>,
    pub restoration_context_ratio: Option<f64>,
    pub idle_compaction_enabled: Option<bool>,
    pub summarization_max_duration_ms: Option<f64>,
    pub ideal: IdealCompactionSettings,
}

/// `DEFAULT_COMPACTION_SETTINGS`.
pub fn default_compaction_settings() -> CompactionSettings {
    CompactionSettings {
        enabled: true,
        reserve_tokens: 16384,
        keep_recent_tokens: 20000,
        speculative_enabled: Some(true),
        speculative_fraction: Some(0.75),
        speculative_cooldown_ms: Some(30000),
        restoration_enabled: Some(true),
        restoration_max_items: Some(10),
        restoration_max_tokens_per_item: Some(5000),
        restoration_max_total_tokens: Some(50_000),
        restoration_context_ratio: Some(0.15),
        idle_compaction_enabled: Some(true),
        summarization_max_duration_ms: None,
        ideal: default_ideal_compaction_settings(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_match_the_pinned_source() {
        let settings = default_compaction_settings();
        assert!(settings.enabled);
        assert_eq!(settings.reserve_tokens, 16384);
        assert_eq!(settings.keep_recent_tokens, 20000);
        assert_eq!(settings.speculative_enabled, Some(true));
        assert_eq!(settings.speculative_fraction, Some(0.75));
        assert_eq!(settings.speculative_cooldown_ms, Some(30000));
        assert_eq!(settings.restoration_max_items, Some(10));
        assert_eq!(settings.restoration_max_tokens_per_item, Some(5000));
        assert_eq!(settings.restoration_max_total_tokens, Some(50_000));
        assert_eq!(settings.restoration_context_ratio, Some(0.15));
        assert_eq!(settings.idle_compaction_enabled, Some(true));
        assert_eq!(settings.ideal, default_ideal_compaction_settings());
    }
}
