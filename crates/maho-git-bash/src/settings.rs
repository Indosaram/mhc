//! Timeout bounds and the default timeout (spec §5.1 step 4, §5.2).

use serde_json::Value;

pub const MIN_TIMEOUT_MS: u64 = 1;
pub const MAX_TIMEOUT_MS: u64 = 1_800_000;
pub const FALLBACK_TIMEOUT_MS: u64 = 120_000;

/// Environment variables consulted for the default timeout, in order.
pub const TIMEOUT_ENV_VARS: [&str; 4] = [
    "OMO_CODEX_GIT_BASH_TIMEOUT_MS",
    "OMO_CODEX_EXEC_COMMAND_TIMEOUT_MS",
    "CODEX_EXEC_COMMAND_TIMEOUT_MS",
    "EXEC_COMMAND_TIMEOUT_MS",
];

/// `value` when it lies in 1..=1800000.
pub fn in_range(value: u64) -> Option<u64> {
    (MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS)
        .contains(&value)
        .then_some(value)
}

fn integral(value: f64) -> Option<u64> {
    if !value.is_finite() || value.fract() != 0.0 || value < MIN_TIMEOUT_MS as f64 {
        return None;
    }
    if value > MAX_TIMEOUT_MS as f64 {
        return None;
    }
    in_range(value as u64)
}

/// A `timeout` argument: a JSON number or numeric string holding an integer in range.
pub fn parse_timeout_arg(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => match number.as_u64() {
            Some(value) => in_range(value),
            None => number.as_f64().and_then(integral),
        },
        Value::String(text) => text.trim().parse::<f64>().ok().and_then(integral),
        _ => None,
    }
}

/// An environment value: a decimal integer in range.
pub fn parse_timeout_env(text: &str) -> Option<u64> {
    text.trim().parse::<u64>().ok().and_then(in_range)
}

/// First valid of: the embedding host's value, the environment variables in
/// [`TIMEOUT_ENV_VARS`] order, else [`FALLBACK_TIMEOUT_MS`].
pub fn default_timeout_ms(host_value: Option<u64>, env: impl Fn(&str) -> Option<String>) -> u64 {
    host_value
        .and_then(in_range)
        .or_else(|| {
            TIMEOUT_ENV_VARS
                .iter()
                .find_map(|name| env(name).as_deref().and_then(parse_timeout_env))
        })
        .unwrap_or(FALLBACK_TIMEOUT_MS)
}
