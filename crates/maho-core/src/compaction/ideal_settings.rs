//! Port of senpi `packages/coding-agent/src/core/compaction/ideal-compaction-settings.ts`.

/// `IdealCompactionSettings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IdealCompactionSettings {
    pub grace_band_enabled: Option<bool>,
    pub tool_admission_enabled: Option<bool>,
    pub reminder_enabled: Option<bool>,
    pub reserve_scaling_enabled: Option<bool>,
    pub speculative_lead_tokens: Option<i64>,
}

/// `DEFAULT_IDEAL_COMPACTION_SETTINGS`.
pub fn default_ideal_compaction_settings() -> IdealCompactionSettings {
    IdealCompactionSettings {
        grace_band_enabled: Some(true),
        tool_admission_enabled: Some(true),
        reminder_enabled: Some(true),
        reserve_scaling_enabled: Some(true),
        speculative_lead_tokens: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ideal_defaults_enable_every_ideal_switch_and_leave_the_lead_unset() {
        let settings = default_ideal_compaction_settings();
        assert_eq!(settings.grace_band_enabled, Some(true));
        assert_eq!(settings.tool_admission_enabled, Some(true));
        assert_eq!(settings.reminder_enabled, Some(true));
        assert_eq!(settings.reserve_scaling_enabled, Some(true));
        assert_eq!(settings.speculative_lead_tokens, None);
    }
}
