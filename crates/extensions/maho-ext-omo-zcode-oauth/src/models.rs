use maho_ai::{model::ModelCompat, types::{InputModality, ModelCost, ModelThinkingLevel, ThinkingLevelMap, Model}, models_store::ModelsStoreEntry};
use maho_ext_api::ProviderModelConfig;
use serde_json::{Value, json};

pub const MODELS_DEV_API_URL: &str = "https://models.dev/api.json";
pub const CATALOG_TTL_MS: i64 = 86_400_000;
pub const ULTRA_BASE_URL: &str = "https://zcode.z.ai/api/v1/ultra-zai/anthropic";
pub fn printable_ascii(value: &str) -> String { value.chars().filter(|character| (' '..='~').contains(character)).collect() }
pub fn resolve_base_url() -> String { let value = printable_ascii(&std::env::var("ZCODE_ANTHROPIC_BASE_URL").unwrap_or_default()); if value.is_empty() { ULTRA_BASE_URL.into() } else { value } }
pub fn app_version() -> String { printable_ascii(&std::env::var("ZCODE_APP_VERSION").ok().filter(|value| !value.is_empty()).unwrap_or_else(|| "3.11.2".into())) }
pub fn os_category(platform: &str) -> &'static str { match platform { "darwin" => "macos", "win32" => "windows", _ => "linux" } }
fn anthropic_compat(compat: &ModelCompat) -> ModelCompat {
    let flags = ["supportsEagerToolInputStreaming","supportsLongCacheRetention","sendSessionAffinityHeaders","supportsCacheControlOnTools","supportsDisabledThinking","supportsTemperature","supportsToolChoice","supportsForcedToolChoice","forceAdaptiveThinking","allowEmptySignature","supportsStrictTools","supportsToolReferences","supportsWebSearch"];
    ModelCompat(compat.0.iter().filter(|(key,value)| (flags.contains(&key.as_str()) && value.is_boolean()) || (key.as_str() == "unsignedThinkingReplay" && matches!(value.as_str(),Some("text"|"empty-signature")))).map(|(key,value)|(key.clone(),value.clone())).collect())
}
pub fn thinking_config(toggle: bool) -> (ThinkingLevelMap, Option<ModelCompat>) {
    let map = [(ModelThinkingLevel::Minimal,"low"),(ModelThinkingLevel::Low,"low"),(ModelThinkingLevel::Medium,"low"),(ModelThinkingLevel::High,"high"),(ModelThinkingLevel::Xhigh,"max"),(ModelThinkingLevel::Max,"max")].into_iter().map(|(level, value)| (level, Some(value.into()))).collect();
    let compat = (!toggle).then(|| ModelCompat(json!({"supportsDisabledThinking":false,"forceAdaptiveThinking":true}).as_object().expect("object literal").clone()));
    (map, compat)
}
pub fn model_config(id: String, name: String) -> ProviderModelConfig {
    let (map, compat) = thinking_config(false);
    ProviderModelConfig { id, name, reasoning: true, input: vec![InputModality::Text], cost: ModelCost::default(), context_window: 1_000_000, max_tokens: 131_072,
        thinking_level_map: Some(map), compat, upstream_model_id: None, api: None, base_url: None, recover_text_tool_calls: None, headers: None, extra_body: None }
}
pub fn parse_catalog(payload: &Value) -> Vec<ProviderModelConfig> {
    let Some(models) = payload.get("zai-coding-plan").and_then(|provider| provider.get("models")).and_then(Value::as_object) else { return Vec::new(); };
    models.iter().filter_map(|(id, value)| {
        let context = value.get("limit")?.get("context")?.as_u64().filter(|value| *value > 0)?;
        let mut model = model_config(id.clone(), value.get("name").and_then(Value::as_str).unwrap_or(id).into());
        model.context_window = context;
        model.max_tokens = value.get("limit").and_then(|limit| limit.get("output")).and_then(Value::as_u64).unwrap_or(131_072);
        model.reasoning = value.get("reasoning") == Some(&Value::Bool(true));
        if let Some(input) = value.get("modalities").and_then(|value| value.get("input")).and_then(Value::as_array) {
            model.input = input.iter().filter_map(|value| match value.as_str() { Some("text") => Some(InputModality::Text), Some("image") => Some(InputModality::Image), Some("video") => Some(InputModality::Video), _ => None }).collect();
            if model.input.is_empty() { model.input.push(InputModality::Text); }
        }
        let toggle = value.get("reasoning_options").and_then(Value::as_array).and_then(|options| options.first()).and_then(|option| option.get("type")).and_then(Value::as_str) == Some("toggle");
        let (map, compat) = thinking_config(toggle); model.thinking_level_map = Some(map); model.compat = compat;
        Some(model)
    }).collect()
}
pub fn persisted(models: &[ProviderModelConfig]) -> Vec<Model> {
    models.iter().map(|model| Model { id: model.id.clone(), name: model.name.clone(), api: "anthropic-messages".into(), provider: "glm-zcode".into(), base_url: resolve_base_url(), reasoning: model.reasoning, thinking_level_map: model.thinking_level_map.clone(), input: model.input.clone(), cost: model.cost.clone(), context_window: model.context_window, max_tokens: model.max_tokens, compat: model.compat.as_ref().map(anthropic_compat), headers: None, upstream_model_id: None, sampling_params: None, cache_retention: None, service_tier: None, recover_text_tool_calls: None }).collect()
}
pub fn stored_to_config(stored: Option<&ModelsStoreEntry>) -> Vec<ProviderModelConfig> {
    stored.into_iter().flat_map(|stored| &stored.models).filter(|model| !model.id.is_empty() && !model.name.is_empty() && model.context_window > 0 && model.max_tokens > 0 && [model.cost.input,model.cost.output,model.cost.cache_read,model.cost.cache_write].iter().all(|value| value.is_finite())).map(|model| {
        let mut config = model_config(model.id.clone(), model.name.clone());
        config.reasoning = model.reasoning; config.input = if model.input.is_empty() { vec![InputModality::Text] } else { model.input.clone() };
        config.cost = model.cost.clone(); config.context_window = model.context_window; config.max_tokens = model.max_tokens;
        if let Some(map) = &model.thinking_level_map { config.thinking_level_map = Some(map.clone()); config.compat = model.compat.as_ref().map(anthropic_compat); }
        config
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn catalog_ignores_invalid_context() { assert!(parse_catalog(&json!({"zai-coding-plan":{"models":{"bad":{"limit":{"context":0}}}}})).is_empty()); }
    #[test] fn catalog_filters_modalities() { let models = parse_catalog(&json!({"zai-coding-plan":{"models":{"m":{"limit":{"context":100},"modalities":{"input":["bad","image"]}}}}})); assert_eq!(models[0].input, [InputModality::Image]); }
    #[test] fn catalog_preserves_toggle_thinking() { let models = parse_catalog(&json!({"zai-coding-plan":{"models":{"m":{"limit":{"context":100},"reasoning_options":[{"type":"toggle"}]}}}})); assert!(models[0].compat.is_none()); }
    #[test] fn persisted_uses_provider_base_url() { let models = persisted(&[model_config("m".into(), "M".into())]); assert_eq!(models[0].provider, "glm-zcode"); assert_eq!(models[0].api, "anthropic-messages"); }
    #[test] fn thinking_maps_extended_levels() { let (map, _) = thinking_config(false); assert_eq!(map[&ModelThinkingLevel::Xhigh].as_deref(), Some("max")); }
    #[test] fn ascii_removes_control_and_unicode() { assert_eq!(printable_ascii(" a\n한b\t"), " ab"); }
    #[test] fn categories_match_upstream() { assert_eq!(os_category("darwin"), "macos"); assert_eq!(os_category("win32"), "windows"); assert_eq!(os_category("other"), "linux"); }
    #[test] fn persisted_compat_filters_foreign_and_malformed_flags() { let mut model = model_config("m".into(),"M".into()); model.compat = Some(ModelCompat(json!({"foreign":true,"supportsWebSearch":true,"supportsTemperature":"yes","unsignedThinkingReplay":"text"}).as_object().unwrap().clone())); assert_eq!(persisted(&[model])[0].compat.as_ref().unwrap().0,json!({"supportsWebSearch":true,"unsignedThinkingReplay":"text"}).as_object().unwrap().clone()); }
    #[test] fn stored_model_preserves_thinking_map_and_compat() { let model = model_config("m".into(),"M".into()); let stored = ModelsStoreEntry { models: persisted(std::slice::from_ref(&model)), ..Default::default() }; let restored = stored_to_config(Some(&stored)); assert_eq!(restored[0].thinking_level_map,model.thinking_level_map); assert_eq!(restored[0].compat,model.compat); }
    #[test] fn legacy_stored_model_gets_conservative_thinking_defaults() { let mut models = persisted(&[model_config("m".into(),"M".into())]); models[0].thinking_level_map = None; models[0].compat = None; let restored = stored_to_config(Some(&ModelsStoreEntry { models, ..Default::default() })); assert_eq!(restored[0].compat,thinking_config(false).1); }
    #[test] fn malformed_stored_model_is_not_restored() { let mut models = persisted(&[model_config("m".into(),"M".into())]); models[0].cost.input = f64::NAN; assert!(stored_to_config(Some(&ModelsStoreEntry { models, ..Default::default() })).is_empty()); }
}
