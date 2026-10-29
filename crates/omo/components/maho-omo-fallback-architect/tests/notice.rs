use maho_omo_fallback_architect::notice::*;

#[test]
fn friendly_model_selectors_use_known_mapping_or_last_id() {
    for (selector, expected) in [
        ("apitopia/kimi-k3-unlocked", "Kimi K3 (max)"),
        ("anthropic/claude-fable-5", "Fable 5"),
        ("anthropic/claude-opus-5", "Opus 5"),
        ("provider/GLM-5-plus", "GLM 5.2"),
        ("unknown/nested/model-id", "model-id"),
        ("provider/", "provider/"),
    ] {
        assert_eq!(friendly_model_name(selector), expected);
    }
}

#[test]
fn persisted_notice_type_is_stable() {
    assert_eq!(FALLBACK_ARCHITECT_NOTICE_TYPE, "omo-fallback-architect:notice");
}
