use maho_ai::{
    model::ModelCompat,
    types::{InputModality, ModelCost, ModelThinkingLevel, ThinkingLevelMap},
};
use maho_ext_api::ProviderModelConfig;

// This starter list covers open models through the global OpenAI-compatible endpoint.
// Marketplace partner models are excluded: they are card-billed and promotional credits do not apply.
// Prices are the model card's published MaaS list prices, USD per million tokens
// (https://cloud.google.com/gemini-enterprise-agent-platform/generative-ai/pricing,
// "Deepseek's models" and "GLM's models"); DeepSeek V4.1 Flash is listed at no charge through 2026-10-16.
// These endpoints accept no thinking parameters, so no level may be offered as steerable.
fn no_thinking_controls() -> ThinkingLevelMap {
    ModelThinkingLevel::ALL
        .into_iter()
        .map(|level| (level, None))
        .collect()
}

fn openapi_compat() -> ModelCompat {
    match serde_json::json!({"supportsDeveloperRole": false, "supportsReasoningEffort": false}) {
        serde_json::Value::Object(map) => ModelCompat(map),
        _ => ModelCompat::default(),
    }
}

fn garden(id: &str, name: &str, cost: ModelCost, context_window: u64) -> ProviderModelConfig {
    ProviderModelConfig {
        model_type: None,
        output: None,
        image_cost: None,
        input_limits: None,
        prompt_cache: None,
        supports_assistant_prefill: None,
        default_thinking_level: None,
        id: id.into(),
        name: name.into(),
        upstream_model_id: None,
        api: None,
        base_url: None,
        reasoning: true,
        recover_text_tool_calls: None,
        thinking_level_map: Some(no_thinking_controls()),
        input: vec![InputModality::Text],
        cost,
        context_window,
        max_tokens: 32_000,
        headers: None,
        extra_body: None,
        compat: Some(openapi_compat()),
    }
}

fn cost(input: f64, output: f64, cache_read: f64) -> ModelCost {
    ModelCost {
        input,
        output,
        cache_read,
        cache_write: 0.0,
        ..Default::default()
    }
}

pub fn vertex_model_garden_models() -> Vec<ProviderModelConfig> {
    vec![
        garden(
            "deepseek-ai/deepseek-v4.1-flash-maas",
            "DeepSeek V4.1 Flash (Vertex)",
            cost(0.3, 1.2, 0.03),
            1_000_000,
        ),
        garden(
            "zai-org/glm-5.2-maas",
            "GLM 5.2 (Vertex)",
            cost(1.4, 4.4, 0.14),
            200_000,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_level_is_steerable_and_compat_disables_developer_role() {
        for model in vertex_model_garden_models() {
            let map = model.thinking_level_map.expect("map");
            assert_eq!(map.len(), 7);
            assert!(map.values().all(Option::is_none));
            assert_eq!(
                model.compat.expect("compat").0["supportsDeveloperRole"],
                false
            );
        }
    }
}
