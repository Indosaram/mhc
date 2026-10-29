use maho_omo_fallback_architect::directive::*;

#[test]
fn persisted_directive_and_reminder_types_are_stable() {
    assert_eq!(FALLBACK_ARCHITECT_DIRECTIVE_TYPE, "omo-fallback-architect:directive");
    assert_eq!(FALLBACK_ARCHITECT_REMINDER_TYPE, "omo-fallback-architect:reminder");
}
