use maho_ext_api::{CompactionSettings, ResolvedCompactionSettings};
use maho_ext_compaction::{extension_wiring::resolved_settings, orchestration::resolve_compaction_geometry};

fn nondefault() -> ResolvedCompactionSettings {
    ResolvedCompactionSettings {
        enabled: true, reserve_tokens: 123, keep_recent_tokens: 456,
        speculative_enabled: false, speculative_fraction: 0.42, speculative_cooldown_ms: 321.,
        restoration_enabled: false, restoration_max_items: 2., restoration_max_tokens_per_item: 11.,
        restoration_max_total_tokens: 22., restoration_context_ratio: 0.03,
        idle_compaction_enabled: false, grace_band_enabled: false, tool_admission_enabled: false,
        reminder_enabled: false, reserve_scaling_enabled: false, speculative_lead_tokens: Some(12000.),
        summarization_max_duration_ms: Some(888.),
    }
}

#[test]
fn supplied_policy_controls_geometry_and_automatic_consumers() {
    // Given a resolved record that differs from all legacy defaults.
    let legacy = CompactionSettings { enabled: false, reserve_tokens: 9999, keep_recent_tokens: 8888 };
    let supplied = nondefault();
    // When the builtin resolves the host policy.
    let settings = resolved_settings(&legacy, Some(&supplied)).unwrap();
    // Then resolved geometry and each optional consumer preserve the host's policy.
    let geometry = resolve_compaction_geometry(100_000., &settings, None);
    assert_eq!(geometry.reserve_tokens, 123.);
    assert_eq!(geometry.lead_tokens, 12000.);
    assert!(settings.enabled);
    assert_eq!(settings.keep_recent_tokens, 456);
    assert_eq!(settings.speculative_enabled, Some(false));
    assert_eq!(settings.speculative_fraction, Some(0.42));
    assert_eq!(settings.speculative_cooldown_ms, Some(321));
    assert_eq!(settings.restoration_enabled, Some(false));
    assert_eq!(settings.restoration_max_items, Some(2));
    assert_eq!(settings.restoration_max_tokens_per_item, Some(11));
    assert_eq!(settings.restoration_max_total_tokens, Some(22));
    assert_eq!(settings.restoration_context_ratio, Some(0.03));
    assert_eq!(settings.idle_compaction_enabled, Some(false));
    assert_eq!(settings.ideal.grace_band_enabled, Some(false));
    assert_eq!(settings.ideal.tool_admission_enabled, Some(false));
    assert_eq!(settings.ideal.reminder_enabled, Some(false));
    assert_eq!(settings.summarization_max_duration_ms, Some(888.));
}

#[test]
fn legacy_host_does_not_fabricate_optional_policy() {
    // Given a host exposing only the three legacy fields.
    let legacy = CompactionSettings { enabled: true, reserve_tokens: 123, keep_recent_tokens: 456 };
    // When the resolved companion is unavailable.
    let settings = resolved_settings(&legacy, None).unwrap();
    // Then automatic optional consumers stand down rather than assert host defaults.
    assert_eq!(settings.reserve_tokens, 123);
    assert_eq!(settings.keep_recent_tokens, 456);
    assert_eq!(settings.speculative_enabled, Some(false));
    assert_eq!(settings.idle_compaction_enabled, Some(false));
    assert_eq!(settings.restoration_enabled, Some(false));
    assert_eq!(settings.ideal.reminder_enabled, Some(false));
    assert_eq!(settings.ideal.tool_admission_enabled, Some(false));
    assert_eq!(settings.ideal.reserve_scaling_enabled, Some(false));
}
