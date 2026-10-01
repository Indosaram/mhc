use indexmap::IndexMap;
use maho_ai::types::{CacheRetention, InputModality, ModelCost, ServiceTierPreference, ThinkingLevelMap};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialSlotRef {
    pub env: Option<String>,
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelsJsonCredentialPolicy {
    pub rotation: Option<bool>,
    pub affinity: Option<bool>,
    pub cooldown_base_ms: Option<f64>,
    pub cooldown_cap_ms: Option<f64>,
    pub slots: Option<IndexMap<String, CredentialSlotRef>>,
}

#[derive(Debug, Clone, Copy)]
pub struct CredentialPolicyDefaults {
    pub rotation: bool,
    pub affinity: bool,
    pub cooldown_base_ms: f64,
    pub cooldown_cap_ms: f64,
}

pub const CREDENTIAL_POLICY_DEFAULTS: CredentialPolicyDefaults = CredentialPolicyDefaults {
    rotation: true, affinity: true,
    cooldown_base_ms: maho_ai::auth::pool::failover::DEFAULT_SLOT_BLOCK_MS,
    cooldown_cap_ms: maho_ai::auth::pool::failover::MAX_SLOT_BLOCK_MS,
};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModel {
    pub id: String,
    pub name: Option<String>,
    pub upstream_model_id: Option<String>,
    pub service_tier: Option<ServiceTierPreference>,
    pub prompt_preset: Option<String>,
    pub recover_text_tool_calls: Option<bool>,
    pub api: Option<String>,
    pub base_url: Option<String>,
    pub reasoning: Option<bool>,
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Option<Vec<InputModality>>,
    pub cost: Option<ModelCost>,
    pub context_window: Option<f64>,
    pub max_tokens: Option<f64>,
    pub headers: Option<BTreeMap<String, String>>,
    pub extra_body: Option<Map<String, Value>>,
    pub sampling_params: Option<Map<String, Value>>,
    pub cache_retention: Option<CacheRetention>,
    pub compat: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialModelCost {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub tiers: Option<Vec<maho_ai::types::ModelCostTier>>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevelMapMode { Merge, Replace }

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModelOverride {
    pub name: Option<String>,
    pub prompt_preset: Option<String>,
    pub recover_text_tool_calls: Option<bool>,
    pub reasoning: Option<bool>,
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub thinking_level_map_mode: Option<ThinkingLevelMapMode>,
    pub input: Option<Vec<InputModality>>,
    pub cost: Option<PartialModelCost>,
    pub context_window: Option<f64>,
    pub max_tokens: Option<f64>,
    pub headers: Option<BTreeMap<String, String>>,
    pub extra_body: Option<Map<String, Value>>,
    pub sampling_params: Option<Map<String, Value>>,
    pub cache_retention: Option<CacheRetention>,
    pub compat: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonProvider {
    pub name: Option<String>,
    pub disabled: Option<bool>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub extra_body: Option<Map<String, Value>>,
    pub cache_retention: Option<CacheRetention>,
    pub compat: Option<Map<String, Value>>,
    pub auth_header: Option<bool>,
    pub whitelist: Option<Vec<String>>,
    pub blacklist: Option<Vec<String>>,
    pub models: Option<Vec<ModelsJsonModel>>,
    pub model_overrides: Option<IndexMap<String, ModelsJsonModelOverride>>,
    pub credentials: Option<ModelsJsonCredentialPolicy>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJson {
    pub disabled_providers: Option<Vec<String>>,
    pub providers: IndexMap<String, ModelsJsonProvider>,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelsConfigValidationError {
    #[error("{0}")]
    Shape(#[from] serde_json::Error),
    #[error("{path}: {message}")]
    Constraint { path: String, message: &'static str },
}

pub fn validate_models_config(value: Value) -> Result<ModelsJson, ModelsConfigValidationError> {
    let config: ModelsJson = serde_json::from_value(value)?;
    let nonempty = |value: Option<&str>, path: String| {
        if value == Some("") { Err(ModelsConfigValidationError::Constraint { path, message: "Expected string length greater or equal to 1" }) } else { Ok(()) }
    };
    for id in config.disabled_providers.iter().flatten() { nonempty(Some(id), "disabledProviders".into())?; }
    for (id, provider) in &config.providers {
        let path = format!("providers.{id}");
        for (name, value) in [("name", &provider.name), ("baseUrl", &provider.base_url), ("apiKey", &provider.api_key), ("api", &provider.api)] {
            nonempty(value.as_deref(), format!("{path}.{name}"))?;
        }
        for (name, values) in [("whitelist", &provider.whitelist), ("blacklist", &provider.blacklist)] {
            for value in values.iter().flatten() { nonempty(Some(value), format!("{path}.{name}"))?; }
        }
        if let Some(policy) = &provider.credentials {
            for (name, value) in [("cooldownBaseMs", policy.cooldown_base_ms), ("cooldownCapMs", policy.cooldown_cap_ms)] {
                if value.is_some_and(|v| v < 0.0) {
                    return Err(ModelsConfigValidationError::Constraint { path: format!("{path}.credentials.{name}"), message: "Expected number greater or equal to 0" });
                }
            }
            for (name, slot) in policy.slots.iter().flatten() {
                nonempty(Some(name), format!("{path}.credentials.slots"))?;
                nonempty(slot.env.as_deref(), format!("{path}.credentials.slots.{name}.env"))?;
                nonempty(slot.value.as_deref(), format!("{path}.credentials.slots.{name}.value"))?;
            }
        }
        for (index, model) in provider.models.iter().flatten().enumerate() {
            for (name, value) in [("id", Some(model.id.as_str())), ("name", model.name.as_deref()),
                ("upstreamModelId", model.upstream_model_id.as_deref()), ("promptPreset", model.prompt_preset.as_deref()),
                ("api", model.api.as_deref()), ("baseUrl", model.base_url.as_deref())] {
                nonempty(value, format!("{path}.models.{index}.{name}"))?;
            }
        }
        for (id, model) in provider.model_overrides.iter().flatten() {
            nonempty(model.name.as_deref(), format!("{path}.modelOverrides.{id}.name"))?;
            nonempty(model.prompt_preset.as_deref(), format!("{path}.modelOverrides.{id}.promptPreset"))?;
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn full_credential_policy_is_accepted() {
        assert!(validate_models_config(json!({"providers":{"openai":{"credentials":{
            "rotation":true,"affinity":false,"cooldownBaseMs":30000,"cooldownCapMs":3600000,
            "slots":{"work":{"env":"OPENAI_API_KEY_2"},"personal":{"value":"!op read op://vault/key"}}
        }}}})).is_ok());
    }
    #[test]
    fn absent_policy_is_accepted() { assert!(validate_models_config(json!({"providers":{"openai":{}}})).is_ok()); }
    #[test]
    fn negative_cooldown_is_rejected() { assert!(validate_models_config(json!({"providers":{"openai":{"credentials":{"cooldownBaseMs":-1}}}})).is_err()); }
    #[test]
    fn unknown_policy_key_is_rejected() { assert!(validate_models_config(json!({"providers":{"openai":{"credentials":{"rotate":true}}}})).is_err()); }
    #[test]
    fn literal_policy_key_is_rejected() { assert!(validate_models_config(json!({"providers":{"openai":{"credentials":{"apiKey":"test"}}}})).is_err()); }
    #[test]
    fn unknown_slot_key_is_rejected() { assert!(validate_models_config(json!({"providers":{"openai":{"credentials":{"slots":{"work":{"apiKey":"test"}}}}}})).is_err()); }
    #[test]
    fn empty_slot_reference_is_rejected() { assert!(validate_models_config(json!({"providers":{"openai":{"credentials":{"slots":{"work":{"env":""}}}}}})).is_err()); }
}
