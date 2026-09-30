//! Port of senpi packages/ai/src/providers/all.ts.

use crate::images_models::{ImagesModels, ImagesProvider, create_images_models};
use crate::models::{CreateModelsOptions, Models, Provider, create_models};
use crate::models_generated::MODELS;
use crate::types::{Model, ModelThinkingLevel};
use indexmap::IndexMap;
use serde_json::{Value, json};
use std::sync::{Arc, LazyLock};

const MODEL_DATA_MANIFEST_JSON: &str = include_str!("data/model_data_manifest.json");

/// Catalogs the fork owns by hand because models.dev does not describe them; a generation run emits
/// neither their shard nor their data file, so they can never appear in MODELS.
static FORK_OWNED_CATALOGS: LazyLock<IndexMap<String, IndexMap<String, Model>>> = LazyLock::new(|| {
    let mut catalogs = IndexMap::new();
    catalogs.insert(
        "kimi-coding".to_owned(),
        super::kimi_coding_models::kimi_coding_models().into_iter().map(|model| (model.id.clone(), model)).collect(),
    );
    catalogs
});

/// Providers present in the generated catalog, plus the fork-owned ones.
static BUILTIN_CATALOGS: LazyLock<IndexMap<String, IndexMap<String, Model>>> = LazyLock::new(|| {
    let mut catalogs = MODELS.clone();
    for (provider, models) in FORK_OWNED_CATALOGS.iter() {
        catalogs.insert(provider.clone(), models.clone());
    }
    catalogs
});

const XIAOMI_MIMO_PROVIDERS: &[&str] =
    &["xiaomi", "xiaomi-token-plan-cn", "xiaomi-token-plan-ams", "xiaomi-token-plan-sgp"];

fn normalize_builtin_model(model: Option<&Model>) -> Option<Model> {
    let model = model?;

    if XIAOMI_MIMO_PROVIDERS.contains(&model.provider.as_str()) && model.id == "mimo-v2.5-pro" {
        let mut compat = model.compat.clone().unwrap_or_default();
        compat.0.insert("requiresReasoningContentOnAssistantMessages".to_owned(), Value::Bool(true));
        compat.0.insert("thinkingFormat".to_owned(), json!("deepseek"));
        compat.0.insert("supportsDisabledThinking".to_owned(), Value::Bool(false));
        return Some(Model { compat: Some(compat), ..model.clone() });
    }

    if model.provider == "anthropic" && model.id == "claude-opus-4-8" {
        let mut map = model.thinking_level_map.clone().unwrap_or_default();
        map.insert(ModelThinkingLevel::Max, Some("max".to_owned()));
        return Some(Model { thinking_level_map: Some(map), ..model.clone() });
    }

    Some(model.clone())
}

/// Typed read of the generated built-in catalog.
pub fn get_builtin_model(provider: &str, model_id: &str) -> Option<Model> {
    normalize_builtin_model(BUILTIN_CATALOGS.get(provider).and_then(|models| models.get(model_id)))
}

pub fn get_builtin_providers() -> Vec<&'static str> {
    BUILTIN_CATALOGS.keys().map(String::as_str).collect()
}

/// Generation timestamp shared by all built-in provider catalogs.
pub fn get_builtin_model_data_generated_at() -> Option<i64> {
    let manifest: Value = serde_json::from_str(MODEL_DATA_MANIFEST_JSON).ok()?;
    let generated_at = manifest.get("generatedAt")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(generated_at).ok().map(|timestamp| timestamp.timestamp_millis())
}

pub fn get_builtin_models(provider: &str) -> Vec<Model> {
    BUILTIN_CATALOGS
        .get(provider)
        .map(|models| models.values().filter_map(|model| normalize_builtin_model(Some(model))).collect())
        .unwrap_or_default()
}

/// All built-in providers, freshly constructed.
pub fn builtin_providers() -> Vec<Arc<dyn Provider>> {
    use super::*;
    vec![
        alibaba_token_plan::alibaba_token_plan_provider(),
        amazon_bedrock::amazon_bedrock_provider(),
        ant_ling::ant_ling_provider(),
        anthropic::anthropic_provider(),
        azure_openai_responses::azure_openai_responses_provider(),
        bai::bai_provider(None),
        baseten::baseten_provider(),
        cerebras::cerebras_provider(),
        cloudflare_ai_gateway::cloudflare_ai_gateway_provider(),
        cloudflare_workers_ai::cloudflare_workers_ai_provider(),
        cursor::cursor_provider(),
        devin::devin_provider(),
        deepseek::deepseek_provider(),
        fireworks::fireworks_provider(),
        github_copilot::github_copilot_provider(),
        google::google_provider(),
        google_vertex::google_vertex_provider(),
        groq::groq_provider(),
        huggingface::huggingface_provider(),
        kimi_coding::kimi_coding_provider(),
        minimax::minimax_provider(),
        minimax_cn::minimax_cn_provider(),
        mistral::mistral_provider(),
        moonshotai::moonshotai_provider(),
        moonshotai_cn::moonshotai_cn_provider(),
        nvidia::nvidia_provider(),
        openai::openai_provider(),
        chatgpt_subscription::chatgpt_subscription_provider(),
        ollama::ollama_provider(None),
        opencode::opencode_provider(),
        opencode_go::opencode_go_provider(),
        opengateway::opengateway_provider(),
        openrouter::openrouter_provider(),
        qwen_token_plan::qwen_token_plan_provider(),
        qwen_token_plan_cn::qwen_token_plan_cn_provider(),
        qwen_token_plan_individual::qwen_token_plan_individual_provider(),
        radius::radius_provider(None),
        together::together_provider(),
        venice::venice_provider(),
        vercel_ai_gateway::vercel_ai_gateway_provider(),
        xai::xai_provider(),
        xiaomi::xiaomi_provider(),
        xiaomi_token_plan_ams::xiaomi_token_plan_ams_provider(),
        xiaomi_token_plan_cn::xiaomi_token_plan_cn_provider(),
        xiaomi_token_plan_sgp::xiaomi_token_plan_sgp_provider(),
        zai::zai_provider(),
        zai_coding_cn::zai_coding_cn_provider(),
    ]
}

/// A Models collection with every built-in provider registered.
pub fn builtin_models(options: Option<CreateModelsOptions>) -> Models {
    let models = create_models(options);
    for provider in builtin_providers() {
        models.set_provider(provider);
    }
    models
}

/// All built-in image-generation providers, freshly constructed.
pub fn builtin_images_providers() -> Vec<Arc<dyn ImagesProvider>> {
    use super::*;
    vec![openrouter_images::openrouter_images_provider(), openai_images::openai_images_provider()]
}

/// An ImagesModels collection with every built-in image-generation provider registered.
pub fn builtin_images_models() -> ImagesModels {
    let models = create_images_models();
    for provider in builtin_images_providers() {
        models.set_provider(provider);
    }
    models
}

pub use super::{ollama::ollama_provider, radius::radius_provider};
