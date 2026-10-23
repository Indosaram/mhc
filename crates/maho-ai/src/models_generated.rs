//! Port of senpi packages/ai/src/models.generated.ts.
//!
//! The catalog is generated data: `crates/maho-ai/data/models.json` is written by
//! `bun tools/golden/gen-models.mjs` from the pinned senpi `MODELS` export and embedded at build time.

use crate::types::Model;
use indexmap::IndexMap;
use std::sync::LazyLock;

pub type ClassifierModelsByProvider =
    IndexMap<String, IndexMap<String, crate::types::ClassifierModel>>;

pub const CLASSIFIER_MODELS_JSON: &str = r#"{"typesafe":{"jev-latest":{"type":"classifier","id":"jev-latest","name":"Jev","api":"typesafe-system-one","provider":"typesafe","baseUrl":"https://api.typesafe.ai/v1/","input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":64000}},"cloudflare-workers-ai":{"typesafe/jev":{"type":"classifier","id":"typesafe/jev","name":"Jev","api":"cloudflare-workers-ai-system-one","provider":"cloudflare-workers-ai","baseUrl":"https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai","input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":32000}},"vercel-ai-gateway":{"baseten/elephant-1":{"type":"classifier","id":"baseten/elephant-1","name":"Elephant-1","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":16384},"baseten/mammoth-1":{"type":"classifier","id":"baseten/mammoth-1","name":"Mammoth-1","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":16384},"convaiinnovations/laya":{"type":"classifier","id":"convaiinnovations/laya","name":"Laya","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":8192},"convaiinnovations/laya-free":{"type":"classifier","id":"convaiinnovations/laya-free","name":"Laya (Free)","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":8192},"liquid/d1":{"type":"classifier","id":"liquid/d1","name":"Liquid d1","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0.02,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":65536},"liquid/d1-ultra":{"type":"classifier","id":"liquid/d1-ultra","name":"Liquid d1 Ultra","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":32768},"microsoft/microsoft-decision-1":{"type":"classifier","id":"microsoft/microsoft-decision-1","name":"Decision-1","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":32768},"openai/gpt-6-luna-decisions":{"type":"classifier","id":"openai/gpt-6-luna-decisions","name":"GPT-6 Luna Decisions","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0.1,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1050000},"perplexity/pplx-decider-flash-v1.2":{"type":"classifier","id":"perplexity/pplx-decider-flash-v1.2","name":"Decider Flash V1.2 4B","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0.01,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":262144},"perplexity/pplx-decider-v1.2":{"type":"classifier","id":"perplexity/pplx-decider-v1.2","name":"Decider V1.2 27B","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0.02,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":262144},"scaledown/scaledown-classify-1":{"type":"classifier","id":"scaledown/scaledown-classify-1","name":"Classify-1","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0.05,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096},"typesafe-ai/jev":{"type":"classifier","id":"typesafe-ai/jev","name":"Jev","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":32000},"typesafe-ai/jev-1.13.0":{"type":"classifier","id":"typesafe-ai/jev-1.13.0","name":"Jev 1.13.0","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":32000},"typesafe-ai/jev-1.14.0":{"type":"classifier","id":"typesafe-ai/jev-1.14.0","name":"Jev 1.14.0","api":"typesafe-system-one","provider":"vercel-ai-gateway","baseUrl":"https://ai-gateway.vercel.sh/typesafe/v1","input":["text"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":32000}},"openrouter":{"~perplexity/pplx-decider-flash-latest":{"type":"classifier","id":"~perplexity/pplx-decider-flash-latest","name":"Perplexity: Decider Flash Latest","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.01,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":262144},"~perplexity/pplx-decider-latest":{"type":"classifier","id":"~perplexity/pplx-decider-latest","name":"Perplexity: Decider Latest","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.02,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":262144},"~typesafe/jev-latest":{"type":"classifier","id":"~typesafe/jev-latest","name":"TypeSafe: Jev Latest","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":262144},"baseten/elephant-1:free":{"type":"classifier","id":"baseten/elephant-1:free","name":"Baseten: Elephant 1 (free)","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":16384},"baseten/mammoth-1:free":{"type":"classifier","id":"baseten/mammoth-1:free","name":"Baseten: Mammoth 1 (free)","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":16384},"cloudflare/clef":{"type":"classifier","id":"cloudflare/clef","name":"Cloudflare: Clef","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":16384},"cloudflare/clef-flash":{"type":"classifier","id":"cloudflare/clef-flash","name":"Cloudflare: Clef Flash","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.038,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":24576},"cloudflare/clef-omni":{"type":"classifier","id":"cloudflare/clef-omni","name":"Cloudflare: Clef Omni","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.15,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":65536},"inception/mercury-decide":{"type":"classifier","id":"inception/mercury-decide","name":"Inception: Mercury Decide","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.02,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":65536},"inception/mercury-decide:free":{"type":"classifier","id":"inception/mercury-decide:free","name":"Inception: Mercury Decide (free)","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":65536},"jaredpalmer/kev-4b":{"type":"classifier","id":"jaredpalmer/kev-4b","name":"Jared Palmer: Kev 4B","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":8192},"liquid/d1":{"type":"classifier","id":"liquid/d1","name":"LiquidAI: d1","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.02,"output":0,"cacheRead":0.02,"cacheWrite":0},"contextWindow":65536},"liquid/d1-ultra":{"type":"classifier","id":"liquid/d1-ultra","name":"LiquidAI: d1-ultra","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.042,"output":0,"cacheRead":0.042,"cacheWrite":0},"contextWindow":32768},"microsoft/microsoft-decision-1":{"type":"classifier","id":"microsoft/microsoft-decision-1","name":"Microsoft: Microsoft-Decision-1","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":32768},"nace-ai/drex-max":{"type":"classifier","id":"nace-ai/drex-max","name":"Nace.AI: Drex Max","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0.06,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":262144},"nace-ai/drex-v1.5":{"type":"classifier","id":"nace-ai/drex-v1.5","name":"Nace.AI: Drex v1.5","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0.04,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":131072},"nace-ai/drex-v2.0":{"type":"classifier","id":"nace-ai/drex-v2.0","name":"Nace.AI: Drex v2.0","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0.06,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":131072},"openai/gpt-6-luna-decisions":{"type":"classifier","id":"openai/gpt-6-luna-decisions","name":"OpenAI: GPT-6 Luna Decisions","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.1,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1050000},"perplexity/pplx-decider-flash-v1.2":{"type":"classifier","id":"perplexity/pplx-decider-flash-v1.2","name":"Perplexity: Decider Flash V1.2","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.01,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":262144},"perplexity/pplx-decider-v1.2":{"type":"classifier","id":"perplexity/pplx-decider-v1.2","name":"Perplexity: Decider V1.2","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.02,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":262144},"respan/span-01":{"type":"classifier","id":"respan/span-01","name":"Respan: Span-01","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0.02,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096},"respan/span-01-lite":{"type":"classifier","id":"respan/span-01-lite","name":"Respan: Span-01 Lite","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096},"respan/span-01-lite:free":{"type":"classifier","id":"respan/span-01-lite:free","name":"Respan: Span-01 Lite (free)","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096},"respan/span-02":{"type":"classifier","id":"respan/span-02","name":"Respan: Span-02","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.01,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":131072},"scaledown/classify-1":{"type":"classifier","id":"scaledown/classify-1","name":"ScaleDown: Classify-1","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.04,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1000000},"togethercomputer/tev1-4b-experimental":{"type":"classifier","id":"togethercomputer/tev1-4b-experimental","name":"Together: Tev1 4B Experimental","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":32768},"typesafe/jev-1.13":{"type":"classifier","id":"typesafe/jev-1.13","name":"TypeSafe: Jev 1.13","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":262144},"typesafe/jev-1.14":{"type":"classifier","id":"typesafe/jev-1.14","name":"TypeSafe: Jev 1.14","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text","image"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":262144},"upstage/solar-decide":{"type":"classifier","id":"upstage/solar-decide","name":"Upstage: Solar Decide","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0.05,"output":0,"cacheRead":0.05,"cacheWrite":0},"contextWindow":524288},"upstage/solar-decide-flash":{"type":"classifier","id":"upstage/solar-decide-flash","name":"Upstage: Solar Decide Flash","api":"typesafe-system-one","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","input":["text"],"cost":{"input":0.05,"output":0,"cacheRead":0.05,"cacheWrite":0},"contextWindow":524288}},"opencode":{"jev-1.13":{"type":"classifier","id":"jev-1.13","name":"Jev 1.13","api":"typesafe-system-one","provider":"opencode","baseUrl":"https://opencode.ai/zen/v1","input":["text"],"cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":32000},"jev-1.13-free":{"type":"classifier","id":"jev-1.13-free","name":"Jev 1.13 Free","api":"typesafe-system-one","provider":"opencode","baseUrl":"https://opencode.ai/zen/v1","input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":32000}}}"#;

pub static CLASSIFIER_MODELS: LazyLock<ClassifierModelsByProvider> = LazyLock::new(|| {
    serde_json::from_str(CLASSIFIER_MODELS_JSON)
        .unwrap_or_else(|error| panic!("embedded classifier catalog is invalid: {error}"))
});

pub fn get_builtin_classifier_model(
    provider: &str,
    model_id: &str,
) -> Result<&'static crate::types::ClassifierModel, CatalogError> {
    let models = CLASSIFIER_MODELS
        .get(provider)
        .ok_or_else(|| CatalogError::UnknownProvider {
            provider: provider.into(),
        })?;
    models.get(model_id).ok_or_else(|| CatalogError::NotFound {
        provider: provider.into(),
        model_id: model_id.into(),
    })
}

pub const MODELS_JSON: &str = include_str!("../data/models.json");

/// provider id -> model id -> model, in senpi's key order.
pub type ModelsByProvider = IndexMap<String, IndexMap<String, Model>>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("Unknown provider: {provider}")]
    UnknownProvider { provider: String },
    #[error("Model not found: {provider}/{model_id}")]
    NotFound { provider: String, model_id: String },
}

pub static MODELS: LazyLock<ModelsByProvider> = LazyLock::new(|| {
    let mut models: ModelsByProvider = serde_json::from_str(MODELS_JSON)
        .unwrap_or_else(|error| panic!("embedded models.json is invalid: {error}"));
    let meta = serde_json::from_str(include_str!("providers/data/meta.json"))
        .unwrap_or_else(|error| panic!("embedded Meta catalog is invalid: {error}"));
    models.insert("meta".into(), meta);
    for model in models.values_mut().flat_map(|entries| entries.values_mut()) {
        if model.provider == "anthropic" && model.api == "anthropic-messages" {
            model
                .prompt_cache
                .get_or_insert(crate::model::ModelPromptCache {
                    short: Some(300),
                    long: Some(3600),
                });
        }
        if model.input.contains(&crate::types::InputModality::Image) {
            let limits = model.input_limits.get_or_insert_with(Default::default);
            let images = limits.images.get_or_insert_with(Default::default);
            let (request_bytes, request_images, message_images) = match model.provider.as_str() {
                "anthropic" => (
                    Some(32 * 1024 * 1024),
                    Some(if model.context_window == 200_000 {
                        100
                    } else {
                        600
                    }),
                    None,
                ),
                "amazon-bedrock" => (None, None, Some(20)),
                "openai" => (Some(512 * 1024 * 1024), Some(1500), None),
                "google" => (Some(20 * 1024 * 1024), Some(3600), None),
                _ => (None, None, None),
            };
            limits.max_request_bytes = limits.max_request_bytes.or(request_bytes);
            images.max_per_request = images.max_per_request.or(request_images);
            images.max_per_message = images.max_per_message.or(message_images);
            let resize = images.resize.get_or_insert_with(Default::default);
            resize.max_width.get_or_insert(2000);
            resize.max_height.get_or_insert(2000);
            resize.max_bytes.get_or_insert(4_718_592);
            resize.jpeg_quality.get_or_insert(80);
        }
    }
    models
});

pub fn get_builtin_providers() -> Vec<&'static str> {
    MODELS.keys().map(String::as_str).collect()
}

pub fn get_builtin_provider_models(provider: &str) -> Result<Vec<&'static Model>, CatalogError> {
    MODELS
        .get(provider)
        .map(|models| models.values().collect())
        .ok_or_else(|| CatalogError::UnknownProvider {
            provider: provider.into(),
        })
}

pub fn get_builtin_model(provider: &str, model_id: &str) -> Result<&'static Model, CatalogError> {
    let models = MODELS
        .get(provider)
        .ok_or_else(|| CatalogError::UnknownProvider {
            provider: provider.into(),
        })?;
    models.get(model_id).ok_or_else(|| CatalogError::NotFound {
        provider: provider.into(),
        model_id: model_id.into(),
    })
}
