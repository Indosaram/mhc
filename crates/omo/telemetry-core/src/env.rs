use crate::constants::UNCONFIGURED_POSTHOG_API_KEY;
use crate::types::TelemetryEnv;

const TRUTHY_DISABLE_VALUES: [&str; 3] = ["1", "true", "yes"];
const SEND_OPT_OUT_VALUES: [&str; 4] = ["0", "false", "no", "yes"];
const DEFAULT_GLOBAL_ENV_PREFIX: &str = "OMO";

#[derive(Debug, Clone, Copy, Default)]
pub struct ShouldDisableTelemetryInput<'a> {
    /// `None` reads the process environment.
    pub env: Option<&'a TelemetryEnv>,
    /// `None` means `"OMO"`.
    pub global_env_prefix: Option<&'a str>,
    pub product_env_prefix: &'a str,
}

pub(crate) fn read_env(env: Option<&TelemetryEnv>, key: &str) -> Option<String> {
    match env {
        Some(env) => env.get(key).cloned(),
        None => std::env::var(key).ok(),
    }
}

fn includes_value(values: &[&str], value: Option<String>) -> bool {
    value.is_some_and(|value| values.contains(&value.trim().to_lowercase().as_str()))
}

pub fn should_disable_telemetry(input: &ShouldDisableTelemetryInput<'_>) -> bool {
    let global_prefix = input.global_env_prefix.unwrap_or(DEFAULT_GLOBAL_ENV_PREFIX);
    let mut prefixes = vec![global_prefix];
    if input.product_env_prefix != global_prefix {
        prefixes.push(input.product_env_prefix);
    }

    if includes_value(&TRUTHY_DISABLE_VALUES, read_env(input.env, "DO_NOT_TRACK")) {
        return true;
    }

    prefixes.into_iter().any(|prefix| {
        includes_value(
            &TRUTHY_DISABLE_VALUES,
            read_env(input.env, &format!("{prefix}_DISABLE_POSTHOG")),
        ) || includes_value(
            &SEND_OPT_OUT_VALUES,
            read_env(input.env, &format!("{prefix}_SEND_ANONYMOUS_TELEMETRY")),
        )
    })
}

/// A set `POSTHOG_API_KEY` wins even when blank (it trims to `""`), exactly like the TS `??`.
pub fn get_telemetry_api_key(env: Option<&TelemetryEnv>, default_api_key: &str) -> String {
    read_env(env, "POSTHOG_API_KEY")
        .map_or_else(|| default_api_key.to_string(), |key| key.trim().to_string())
}

pub fn is_configured_telemetry_api_key(api_key: &str) -> bool {
    let normalized = api_key.trim();
    !normalized.is_empty() && normalized != UNCONFIGURED_POSTHOG_API_KEY
}

pub fn has_telemetry_api_key(env: Option<&TelemetryEnv>, default_api_key: &str) -> bool {
    is_configured_telemetry_api_key(&get_telemetry_api_key(env, default_api_key))
}

/// A blank `POSTHOG_HOST` falls back to the default, exactly like the TS `||`.
pub fn get_telemetry_host(env: Option<&TelemetryEnv>, default_host: &str) -> String {
    read_env(env, "POSTHOG_HOST")
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| default_host.to_string())
}
