use crate::rules::{constants::{DEFAULT_MAX_RULE_CHARS, DEFAULT_MAX_RESULT_CHARS}, types::{EnabledSources, Mode, PiRulesConfig}};

pub fn config_from_environment() -> PiRulesConfig {
    config_from_values(|key| std::env::var(key).ok())
}

pub fn config_from_values(env: impl Fn(&str) -> Option<String>) -> PiRulesConfig {
    let positive = |key| env(key).and_then(|value| {
        let value = value.trim();
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) { return None; }
        value.parse::<usize>().ok().filter(|number| *number > 0 && *number <= 9_007_199_254_740_991)
    });
    let disabled = env("PI_RULES_DISABLED").is_some_and(|value| matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "on"));
    PiRulesConfig {
        disabled,
        mode: Mode::Both,
        max_rule_chars: positive("PI_RULES_MAX_RULE_CHARS").unwrap_or(DEFAULT_MAX_RULE_CHARS),
        max_result_chars: positive("PI_RULES_MAX_RESULT_CHARS").unwrap_or(DEFAULT_MAX_RESULT_CHARS),
        enabled_sources: EnabledSources::Auto,
    }
}
