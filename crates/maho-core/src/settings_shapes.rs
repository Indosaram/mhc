//! Port of senpi packages/coding-agent/src/core/settings-shapes.ts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PromptCacheKeepAliveSettings {
    pub enabled: Option<bool>,
    pub max_requests_per_session: Option<i64>,
    pub max_cost_usd_per_session: Option<f64>,
    pub margin_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PromptCacheSettings {
    pub cache_aware_timeouts: Option<bool>,
    pub safety_buffer_seconds: Option<i64>,
    pub goal_backstop_max_seconds: Option<i64>,
    pub keep_alive: Option<PromptCacheKeepAliveSettings>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImageSettings {
    pub auto_resize: Option<bool>,
    pub block_images: Option<bool>,
    pub max_historical_images: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LookAtSettings {
    pub enabled: Option<bool>,
    pub models: Option<Vec<String>>,
}

pub const ASK_USER_DEFAULT_TIMEOUT_MINUTES: i64 = 30;
pub const ASK_USER_MIN_TIMEOUT_MINUTES: i64 = 1;
pub const ASK_USER_MAX_TIMEOUT_MINUTES: i64 = 120;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AskUserSettings {
    pub enabled: Option<bool>,
    pub bell: Option<bool>,
    pub timeout_minutes: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ThinkingBudgetsSettings {
    pub minimal: Option<i64>,
    pub low: Option<i64>,
    pub medium: Option<i64>,
    pub high: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MermaidRenderingMode {
    Off,
    Final,
    Streaming,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MarkdownSettings {
    pub code_block_indent: Option<String>,
    pub mermaid: Option<MermaidRenderingMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceTierPreference {
    Auto,
    Flex,
    Priority,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResponsesSettings {
    pub service_tier: Option<ServiceTierPreference>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderConcurrencySettings {
    pub max_concurrency: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_user_timeout_bounds_match_senpi() {
        assert_eq!(ASK_USER_DEFAULT_TIMEOUT_MINUTES, 30);
        assert_eq!(ASK_USER_MIN_TIMEOUT_MINUTES, 1);
        assert_eq!(ASK_USER_MAX_TIMEOUT_MINUTES, 120);
    }

    #[test]
    fn prompt_cache_settings_deserialize_camel_case() {
        let settings: PromptCacheSettings = serde_json::from_str(
            r#"{"cacheAwareTimeouts":false,"safetyBufferSeconds":10,"goalBackstopMaxSeconds":200,"keepAlive":{"enabled":true,"maxRequestsPerSession":5}}"#,
        )
        .expect("settings");
        assert_eq!(settings.cache_aware_timeouts, Some(false));
        assert_eq!(settings.safety_buffer_seconds, Some(10));
        assert_eq!(settings.keep_alive.as_ref().and_then(|k| k.max_requests_per_session), Some(5));
    }

    #[test]
    fn mermaid_and_service_tier_use_lowercase_names() {
        let markdown: MarkdownSettings = serde_json::from_str(r#"{"codeBlockIndent":"  ","mermaid":"final"}"#).expect("markdown");
        assert_eq!(markdown.mermaid, Some(MermaidRenderingMode::Final));
        let responses: ResponsesSettings = serde_json::from_str(r#"{"serviceTier":"priority"}"#).expect("responses");
        assert_eq!(responses.service_tier, Some(ServiceTierPreference::Priority));
    }

    #[test]
    fn image_and_lookat_settings_default_empty() {
        assert_eq!(serde_json::from_str::<ImageSettings>("{}").expect("image"), ImageSettings::default());
        assert_eq!(serde_json::from_str::<LookAtSettings>("{}").expect("lookat"), LookAtSettings::default());
        assert_eq!(serde_json::from_str::<ProviderConcurrencySettings>("{}").expect("concurrency"), ProviderConcurrencySettings::default());
        assert_eq!(serde_json::from_str::<ThinkingBudgetsSettings>("{}").expect("budgets"), ThinkingBudgetsSettings::default());
        assert_eq!(serde_json::from_str::<AskUserSettings>("{}").expect("ask"), AskUserSettings::default());
    }
}
