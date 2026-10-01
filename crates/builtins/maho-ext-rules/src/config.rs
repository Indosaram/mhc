use crate::rules::{constants::{DEFAULT_MAX_RESULT_CHARS, DEFAULT_MAX_RULE_CHARS}, types::{EnabledSources, PiRulesConfig, RulesMode}};

pub fn config_from_environment(mut read: impl FnMut(&str) -> Option<String>) -> PiRulesConfig {
    let mut config = PiRulesConfig { disabled: false, mode: RulesMode::Both, max_rule_chars: DEFAULT_MAX_RULE_CHARS, max_result_chars: DEFAULT_MAX_RESULT_CHARS, enabled_sources: EnabledSources::Auto };
    config.disabled = read("PI_RULES_DISABLED").is_some_and(|value| matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "on"));
    for (name, target) in [("PI_RULES_MAX_RULE_CHARS", &mut config.max_rule_chars), ("PI_RULES_MAX_RESULT_CHARS", &mut config.max_result_chars)] {
        if let Some(value) = read(name).and_then(|value| parse_positive_integer(&value)) { *target = value; }
    }
    config
}

fn parse_positive_integer(value: &str) -> Option<usize> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) { return None; }
    let parsed = value.parse::<u64>().ok()?;
    if parsed == 0 || parsed > 9_007_199_254_740_991 { return None; }
    usize::try_from(parsed).ok()
}
