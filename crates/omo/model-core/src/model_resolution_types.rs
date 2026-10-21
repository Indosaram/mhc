use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegatedModelConfig {
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(rename = "reasoningEffort", default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(rename = "maxTokens", default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<DelegatedModelThinking>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<IndexMap<String, bool>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegatedModelThinking {
    #[serde(rename = "type")]
    pub thinking_type: DelegatedModelThinkingType,
    #[serde(rename = "budgetTokens", default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DelegatedModelThinkingType {
    Enabled,
    Disabled,
}

pub use crate::model_resolution_pipeline::{
    ModelResolutionRequest, ModelResolutionProvenance, ModelResolutionResult,
};
