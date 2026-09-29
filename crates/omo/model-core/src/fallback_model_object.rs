use serde::Deserialize;
use serde::Serialize;

/// Whether an explicit thinking block is enabled or disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingType {
    Enabled,
    Disabled,
}

/// `{ type: "enabled" | "disabled"; budgetTokens?: number }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub kind: ThinkingType,
    #[serde(
        rename = "budgetTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub budget_tokens: Option<i64>,
}

/// Object form of a `fallback_models` config entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FallbackModelObject {
    pub model: String,
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

/// One entry of a mixed `fallback_models` array: `string | FallbackModelObject`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FallbackModelEntry {
    Model(String),
    Object(FallbackModelObject),
}

/// The `fallback_models` config value: `string | (string | FallbackModelObject)[]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FallbackModelsConfig {
    Single(String),
    List(Vec<FallbackModelEntry>),
}
