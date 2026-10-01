//! Port of senpi packages/coding-agent/src/core/http-dispatcher.ts (the policy half).
//!
//! senpi installs an Undici dispatcher and optionally the Undici globals. The Rust port has no
//! process-global fetch to replace: HTTP goes through reqwest, whose pool and timeouts are owned by
//! maho-ai's transport. This module keeps the timeout policy, the choice table and the proxy rules
//! so the same settings drive that transport; the dispatcher install itself is N/A.

use std::collections::HashMap;

pub const DEFAULT_HTTP_IDLE_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_AUTO_SELECT_FAMILY_ATTEMPT_TIMEOUT_MS: u64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpIdleTimeoutChoice {
    pub label: &'static str,
    pub timeout_ms: u64,
}

pub const HTTP_IDLE_TIMEOUT_CHOICES: [HttpIdleTimeoutChoice; 5] = [
    HttpIdleTimeoutChoice { label: "30 sec", timeout_ms: 30_000 },
    HttpIdleTimeoutChoice { label: "1 min", timeout_ms: 60_000 },
    HttpIdleTimeoutChoice { label: "2 min", timeout_ms: 120_000 },
    HttpIdleTimeoutChoice { label: "5 min", timeout_ms: 300_000 },
    HttpIdleTimeoutChoice { label: "disabled", timeout_ms: 0 },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UndiciGlobalsInstallInput {
    /// Whether the runtime reports itself as Bun.
    pub is_bun: bool,
    pub current_fetch_is_original: bool,
    pub current_fetch_is_installed: bool,
    pub installed_fetch_present: bool,
}

/// Whether the global install may replace the runtime's fetch. Bun keeps its native fetch; Node
/// installs the globals unless a caller deliberately replaced fetch after module load.
pub fn should_install_undici_globals(input: UndiciGlobalsInstallInput) -> bool {
    if input.is_bun {
        return false;
    }
    if input.installed_fetch_present { input.current_fetch_is_installed } else { input.current_fetch_is_original }
}

pub fn parse_http_idle_timeout_ms(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.eq_ignore_ascii_case("disabled") {
                return Some(0);
            }
            if trimmed.is_empty() {
                return None;
            }
            trimmed.parse::<f64>().ok().and_then(parse_number)
        }
        serde_json::Value::Number(number) => number.as_f64().and_then(parse_number),
        _ => None,
    }
}

fn parse_number(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some(value.floor() as u64)
}

pub fn format_http_idle_timeout_ms(timeout_ms: u64) -> String {
    if let Some(choice) = HTTP_IDLE_TIMEOUT_CHOICES.iter().find(|choice| choice.timeout_ms == timeout_ms) {
        return choice.label.to_owned();
    }
    format!("{} sec", timeout_ms / 1000)
}

/// Applies the HTTP proxy to the environment. In a multi-session RPC process the proxy is
/// process-global, so a conflicting value is an error rather than a silent override.
pub fn apply_http_proxy_settings(
    http_proxy: Option<&str>,
    is_multi_session_rpc: bool,
    env: &mut HashMap<String, String>,
) -> Result<(), String> {
    let Some(proxy) = http_proxy.map(str::trim).filter(|proxy| !proxy.is_empty()) else { return Ok(()) };
    if is_multi_session_rpc {
        let existing = env.get("HTTP_PROXY").or_else(|| env.get("HTTPS_PROXY"));
        if let Some(existing) = existing
            && existing != proxy
        {
            return Err("Multi-session RPC shares one process-global HTTP proxy; proxy settings must be fixed at process startup.".to_owned());
        }
    }
    env.entry("HTTP_PROXY".to_owned()).or_insert_with(|| proxy.to_owned());
    env.entry("HTTPS_PROXY".to_owned()).or_insert_with(|| proxy.to_owned());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn disabled_parses_to_zero() {
        assert_eq!(parse_http_idle_timeout_ms(&json!("disabled")), Some(0));
        assert_eq!(parse_http_idle_timeout_ms(&json!("DISABLED")), Some(0));
    }

    #[test]
    fn numeric_and_string_numbers_parse() {
        assert_eq!(parse_http_idle_timeout_ms(&json!(30000)), Some(30000));
        assert_eq!(parse_http_idle_timeout_ms(&json!("30000")), Some(30000));
        assert_eq!(parse_http_idle_timeout_ms(&json!(1500.9)), Some(1500));
    }

    #[test]
    fn invalid_values_are_none() {
        assert_eq!(parse_http_idle_timeout_ms(&json!("")), None);
        assert_eq!(parse_http_idle_timeout_ms(&json!(-1)), None);
        assert_eq!(parse_http_idle_timeout_ms(&json!("nope")), None);
        assert_eq!(parse_http_idle_timeout_ms(&json!(null)), None);
    }

    #[test]
    fn formatting_uses_the_choice_label_when_present() {
        assert_eq!(format_http_idle_timeout_ms(30000), "30 sec");
        assert_eq!(format_http_idle_timeout_ms(0), "disabled");
        assert_eq!(format_http_idle_timeout_ms(45000), "45 sec");
    }

    #[test]
    fn bun_never_installs_the_globals() {
        let input = UndiciGlobalsInstallInput { is_bun: true, current_fetch_is_original: true, current_fetch_is_installed: false, installed_fetch_present: false };
        assert!(!should_install_undici_globals(input));
    }

    #[test]
    fn node_installs_unless_fetch_was_replaced() {
        let untouched = UndiciGlobalsInstallInput { is_bun: false, current_fetch_is_original: true, current_fetch_is_installed: false, installed_fetch_present: false };
        assert!(should_install_undici_globals(untouched));
        let replaced = UndiciGlobalsInstallInput { is_bun: false, current_fetch_is_original: false, current_fetch_is_installed: false, installed_fetch_present: false };
        assert!(!should_install_undici_globals(replaced));
    }

    #[test]
    fn a_proxy_is_applied_without_overriding_an_existing_value() {
        let mut env = HashMap::new();
        apply_http_proxy_settings(Some("http://proxy"), false, &mut env).expect("apply");
        assert_eq!(env.get("HTTP_PROXY").map(String::as_str), Some("http://proxy"));
        assert_eq!(env.get("HTTPS_PROXY").map(String::as_str), Some("http://proxy"));
    }

    #[test]
    fn a_conflicting_proxy_in_a_multi_session_process_is_an_error() {
        let mut env = HashMap::from([("HTTP_PROXY".to_owned(), "http://a".to_owned())]);
        assert!(apply_http_proxy_settings(Some("http://b"), true, &mut env).is_err());
        assert!(apply_http_proxy_settings(Some("http://a"), true, &mut env).is_ok());
    }

    #[test]
    fn a_blank_proxy_is_ignored() {
        let mut env = HashMap::new();
        apply_http_proxy_settings(Some("  "), false, &mut env).expect("apply");
        assert!(env.is_empty());
    }
}
