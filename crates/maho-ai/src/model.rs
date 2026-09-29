//! Port of senpi packages/ai/src/model.ts.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

use crate::types::{
    AnthropicMessagesCompat, Api, BedrockCompat, CacheRetention, InputModality, ModelCost, ModelThinkingLevel,
    OpenAICompletionsCompat, OpenAIResponsesCompat, ProviderId, ServiceTierPreference, ThinkingLevelMap,
};

/// Model interface for the unified model system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub name: String,
    pub api: Api,
    pub provider: ProviderId,
    pub base_url: String,
    pub reasoning: bool,
    /// In a present map, omitting `xhigh` or `max` disables that extended tier; ordinary missing
    /// levels use provider defaults. `None` (null) marks any level as unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Vec<InputModality>,
    pub cost: ModelCost,
    pub context_window: u64,
    pub max_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTierPreference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recover_text_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<ModelCompat>,
}

/// The TS `compat` field is typed by `api`; it is kept as its JSON object so unknown keys round-trip,
/// with typed views per API family.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelCompat(pub Map<String, Value>);

impl ModelCompat {
    fn view<T: DeserializeOwned + Default>(&self) -> T {
        serde_json::from_value(Value::Object(self.0.clone())).unwrap_or_default()
    }

    pub fn openai_completions(&self) -> OpenAICompletionsCompat {
        self.view()
    }

    pub fn openai_responses(&self) -> OpenAIResponsesCompat {
        self.view()
    }

    pub fn anthropic_messages(&self) -> AnthropicMessagesCompat {
        self.view()
    }

    pub fn bedrock(&self) -> BedrockCompat {
        self.view()
    }

    pub fn cursor_agent(&self) -> CursorAgentCompat {
        self.view()
    }

    pub fn devin_agent(&self) -> DevinAgentCompat {
        self.view()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }
}

/// Devin (Cascade) model metadata the transport branches on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevinAgentCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_router: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_parallel_tool_calls: Option<bool>,
}

/// Cursor agent protocol model metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorAgentCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_max_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_reasoning: Option<CursorReasoning>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorReasoning {
    pub capability_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_mode: Option<bool>,
    pub representative_variant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant_ids: Option<BTreeMap<ModelThinkingLevel, String>>,
}
