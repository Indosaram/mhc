use serde::Deserialize;
use serde::Serialize;

use crate::fallback_model_object::ThinkingConfig;

/// One rung of a fallback chain: a model id plus the providers that may serve it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FallbackEntry {
    pub providers: Vec<String>,
    pub model: String,
    /// Entry-specific variant (e.g. GPT -> high, Opus -> max).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(
        rename = "reasoningEffort",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(rename = "maxTokens", default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
}

/// Hardcoded model requirement for an agent or category.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelRequirement {
    #[serde(rename = "fallbackChain")]
    pub fallback_chain: Vec<FallbackEntry>,
    /// Default variant (used when an entry does not specify one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// If set, only activates when this model is available (fuzzy match).
    #[serde(
        rename = "requiresModel",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub requires_model: Option<String>,
    /// If true, requires at least one model in the chain to be available.
    #[serde(
        rename = "requiresAnyModel",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub requires_any_model: Option<bool>,
    /// If set, only activates when any of these providers is connected.
    #[serde(
        rename = "requiresProvider",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub requires_provider: Option<Vec<String>>,
}
