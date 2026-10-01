use indexmap::IndexMap;
use serde_json::Value;
use super::hint_policy::HintPolicySettings;

pub type FallbackChains = IndexMap<String, Vec<String>>;
pub const DEFAULT_HINTED_WAIT_CAP_MS: f64 = 300_000.0;
pub const DEFAULT_PROBE_BACK_MAX_MS: f64 = 3_600_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackRevertPolicy { CooldownExpiry, Never }

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRetryFallbackSettings {
    pub model_fallback: bool,
    pub chains: FallbackChains,
    pub revert_policy: FallbackRevertPolicy,
}

pub fn default_fallback_chains() -> FallbackChains {
    let long = ["claude-opus-5-5:max", "claude-opus-5:max", "claude-opus-4-8:max", "claude-opus-4-6:max"];
    [("claude-fable-5-1", long.as_slice()), ("claude-fable-5", long.as_slice()), ("claude-opus-5-5", &long[1..])]
        .into_iter().map(|(key, values)| (key.to_owned(), values.iter().map(|s| (*s).to_owned()).collect())).collect()
}

pub fn resolve_retry_fallback_settings(settings: Option<&Value>) -> ResolvedRetryFallbackSettings {
    let mut chains = default_fallback_chains();
    if let Some(value) = settings.and_then(|v| v.get("fallbackChains"))
        && let Ok(overrides) = serde_json::from_value::<FallbackChains>(value.clone()) {
        chains.extend(overrides);
    }
    ResolvedRetryFallbackSettings {
        model_fallback: settings.and_then(|s| s.get("modelFallback")).and_then(Value::as_bool).unwrap_or(true),
        chains,
        revert_policy: if settings.and_then(|s| s.get("fallbackRevertPolicy")).and_then(Value::as_str) == Some("never") {
            FallbackRevertPolicy::Never
        } else { FallbackRevertPolicy::CooldownExpiry },
    }
}

pub fn resolve_abort_server_side_fallback(settings: Option<&Value>) -> bool {
    settings.and_then(|s| s.get("abortServerSideFallback")).and_then(Value::as_bool).unwrap_or(true)
}

pub fn resolve_hint_policy_settings(settings: Option<&Value>) -> HintPolicySettings {
    let hinted_wait_cap_ms = settings.and_then(|s| s.get("hintedWaitCapMs")).and_then(Value::as_f64).filter(|n| *n >= 0.0).unwrap_or(DEFAULT_HINTED_WAIT_CAP_MS);
    let probe_back_max_ms = settings.and_then(|s| s.get("probeBackMaxMs")).and_then(Value::as_f64).filter(|n| *n >= 0.0).unwrap_or(DEFAULT_PROBE_BACK_MAX_MS);
    if probe_back_max_ms <= hinted_wait_cap_ms {
        HintPolicySettings { hinted_wait_cap_ms: DEFAULT_HINTED_WAIT_CAP_MS, probe_back_max_ms: DEFAULT_PROBE_BACK_MAX_MS }
    } else { HintPolicySettings { hinted_wait_cap_ms, probe_back_max_ms } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn malformed_chains_restore_all_defaults() {
        let settings = json!({"fallbackChains":{"custom":["a/b"],"invalid":[1]}});
        assert_eq!(resolve_retry_fallback_settings(Some(&settings)).chains, default_fallback_chains());
    }
    #[test]
    fn unrelated_chain_preserves_defaults() {
        let settings = json!({"fallbackChains":{"a/b":["c/d"]}});
        let resolved = resolve_retry_fallback_settings(Some(&settings));
        assert_eq!(resolved.chains.len(), 4);
        assert_eq!(resolved.chains["a/b"], ["c/d"]);
    }
    #[test]
    fn empty_chain_is_a_tombstone() {
        let settings = json!({"fallbackChains":{"claude-fable-5":[]}});
        assert!(resolve_retry_fallback_settings(Some(&settings)).chains["claude-fable-5"].is_empty());
    }
    #[test]
    fn explicit_never_and_disabled_are_preserved() {
        let settings = json!({"modelFallback":false,"fallbackRevertPolicy":"never"});
        let resolved = resolve_retry_fallback_settings(Some(&settings));
        assert!(!resolved.model_fallback);
        assert_eq!(resolved.revert_policy, FallbackRevertPolicy::Never);
    }
    #[test]
    fn invalid_hint_order_resets_both_thresholds() {
        let settings = json!({"hintedWaitCapMs":10000,"probeBackMaxMs":10000});
        let resolved = resolve_hint_policy_settings(Some(&settings));
        assert_eq!(resolved.hinted_wait_cap_ms, DEFAULT_HINTED_WAIT_CAP_MS);
        assert_eq!(resolved.probe_back_max_ms, DEFAULT_PROBE_BACK_MAX_MS);
    }
    #[test]
    fn zero_hint_cap_is_valid() {
        assert_eq!(resolve_hint_policy_settings(Some(&json!({"hintedWaitCapMs":0}))).hinted_wait_cap_ms, 0.0);
    }
    #[test]
    fn server_abort_defaults_and_explicit_override() {
        assert!(resolve_abort_server_side_fallback(None));
        assert!(!resolve_abort_server_side_fallback(Some(&json!({"abortServerSideFallback":false}))));
        assert!(resolve_abort_server_side_fallback(Some(&json!({"abortServerSideFallback":"no"}))));
    }
}
