use maho_ext_rules::config::config_from_environment;

#[test]
fn truthy_environment_values_disable_rules() {
    for value in ["1", "true", "yes", "on", " TRUE "] {
        let config = config_from_environment(|key| (key == "PI_RULES_DISABLED").then(|| value.into()));
        assert!(config.disabled);
    }
}
#[test]
fn other_environment_values_keep_rules_enabled() {
    for value in ["0", "false", "off", "maybe", ""] {
        let config = config_from_environment(|key| (key == "PI_RULES_DISABLED").then(|| value.into()));
        assert!(!config.disabled);
    }
}
#[test]
fn positive_integer_limits_override_defaults() {
    let config = config_from_environment(|key| match key { "PI_RULES_MAX_RULE_CHARS" => Some(" 00800 ".into()), "PI_RULES_MAX_RESULT_CHARS" => Some("1200".into()), _ => None });
    assert_eq!((config.max_rule_chars, config.max_result_chars), (800, 1200));
}
#[test]
fn invalid_or_unsafe_integer_limits_preserve_defaults() {
    for value in ["", "0", "-1", "+1", "1.5", "1e3", "Infinity", "9007199254740992", "9999999999999999999999999"] {
        let config = config_from_environment(|_| Some(value.into()));
        assert_eq!((config.max_rule_chars, config.max_result_chars), (12000, 40000));
    }
}
