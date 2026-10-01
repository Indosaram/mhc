//! Port of senpi packages/coding-agent/src/core/settings-public-types.ts.
//!
//! The public settings type surface; settings_manager re-exports these so existing importers keep
//! their path.

use serde::{Deserialize, Serialize};

pub use crate::compaction_settings_access::{CompactionModelOverride, CompactionModelSelector, CompactionSettings};
pub use crate::settings_shapes::{
    AskUserSettings, ImageSettings, LookAtSettings, MarkdownSettings, MermaidRenderingMode, PromptCacheKeepAliveSettings,
    PromptCacheSettings, ProviderConcurrencySettings, ResponsesSettings, ServiceTierPreference, ThinkingBudgetsSettings,
};
pub use crate::terminal_settings::{BranchSummarySettings, TerminalSettings};

/// senpi declares ProviderRetrySettings in retry-fallback/settings.ts and re-exports it here; the
/// data-only shape is declared here because retry-fallback/settings.ts is owned by todo 17.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderRetrySettings {
    pub timeout_ms: Option<f64>,
    pub stream_start_timeout_ms: Option<f64>,
    pub stream_retry_timeout_ms: Option<f64>,
    pub max_retries: Option<i64>,
    pub max_retry_delay_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RetrySettings {
    pub enabled: Option<bool>,
    pub max_retries: Option<i64>,
    pub base_delay_ms: Option<f64>,
    pub max_agent_delay_ms: Option<f64>,
    pub provider: Option<ProviderRetrySettings>,
    pub model_fallback: Option<bool>,
    pub fallback_chains: Option<std::collections::BTreeMap<String, Vec<String>>>,
    pub fallback_revert_policy: Option<String>,
    pub abort_server_side_fallback: Option<bool>,
    pub hinted_wait_cap_ms: Option<f64>,
    pub probe_back_max_ms: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_settings_types_are_reexported() {
        let _: Option<CompactionSettings> = None;
        let _: Option<TerminalSettings> = None;
        let _: Option<MarkdownSettings> = None;
        let _: Option<RetrySettings> = None;
    }

    #[test]
    fn retry_settings_deserialize_camel_case() {
        let settings: RetrySettings =
            serde_json::from_str(r#"{"enabled":true,"maxRetries":3,"provider":{"timeoutMs":1000,"maxRetries":2}}"#).expect("settings");
        assert_eq!(settings.max_retries, Some(3));
        assert_eq!(settings.provider.as_ref().and_then(|p| p.max_retries), Some(2));
        assert_eq!(settings.provider.as_ref().and_then(|p| p.timeout_ms), Some(1000.0));
    }
}
