//! `gateway.*` configuration and the `MAHO_GATEWAY` switch (spec §8).

use serde_json::Value;
use std::time::Duration;

/// Environment switch; `0`, `false`, `off`, `no` disable the gateway.
pub const SWITCH_ENV: &str = "MAHO_GATEWAY";
pub const DEFAULT_POLL_MS: u64 = 500;
pub const MIN_POLL_MS: u64 = 50;
pub const DEFAULT_MAX_HOPS: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub poll: Duration,
    pub max_hops: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            poll: Duration::from_millis(DEFAULT_POLL_MS),
            max_hops: DEFAULT_MAX_HOPS,
        }
    }
}

fn switched_off(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "no"
    )
}

impl Settings {
    /// From the merged config view (its `gateway` object) and the environment switch.
    pub fn from_view(view: Option<&Value>, env: &dyn Fn(&str) -> Option<String>) -> Self {
        let mut settings = Self::default();
        if let Some(gateway) = view.and_then(|view| view.get("gateway")) {
            if let Some(enabled) = gateway.get("enabled").and_then(Value::as_bool) {
                settings.enabled = enabled;
            }
            if let Some(ms) = gateway.get("poll_ms").and_then(Value::as_u64) {
                settings.poll = Duration::from_millis(ms.max(MIN_POLL_MS));
            }
            if let Some(hops) = gateway.get("max_hops").and_then(Value::as_u64) {
                settings.max_hops = u32::try_from(hops).unwrap_or(u32::MAX);
            }
        }
        if env(SWITCH_ENV).is_some_and(|value| switched_off(&value)) {
            settings.enabled = false;
        }
        settings
    }

    /// For `cwd`: the native harness view of `omo.jsonc` and the process environment.
    pub fn load(cwd: &std::path::Path) -> Self {
        let env = maho_config::process_env();
        let loaded = maho_config::load_native(cwd, &env);
        Self::from_view(Some(&loaded.view), &|key| env.get(key).cloned())
    }
}
