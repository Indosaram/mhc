//! Module directory for senpi packages/ai/src/providers/.

use crate::api_registry::get_builtin_api_provider;
use crate::env_api_keys::get_env_api_key;
use crate::image_models_generated::IMAGE_MODELS;
use crate::images_models::ResolveImagesAuth;
use crate::model_catalog::flatten_model_catalog;
use crate::models::{AuthResolution, ProviderAuthResult};
use crate::models_generated::get_builtin_provider_models;
use crate::types::{
    AssistantMessageEventStream, Context, DeferredFetchOptions, DeferredHandle, ImagesModel, Model, ProviderStreams,
    SimpleStreamOptions, StreamOptions,
};
use crate::utils::lazy::error_stream;
use indexmap::IndexMap;
use std::sync::Arc;

pub mod alibaba_token_plan;
pub mod alibaba_token_plan_models;
pub mod all;
pub mod amazon_bedrock;
pub mod amazon_bedrock_models;
pub mod ant_ling;
pub mod ant_ling_models;
pub mod anthropic;
pub mod anthropic_models;
pub mod azure_openai_responses;
pub mod azure_openai_responses_models;
pub mod bai;
pub mod bai_stream;
pub mod bai_models;
pub mod baseten;
pub mod baseten_models;
pub mod cerebras;
pub mod cerebras_models;
pub mod chatgpt_subscription;
pub mod chatgpt_subscription_models;
pub mod cloudflare_ai_gateway;
pub mod cloudflare_ai_gateway_models;
pub mod cloudflare_auth;
pub mod cloudflare_stream;
pub mod cloudflare_workers_ai;
pub mod cloudflare_workers_ai_models;
pub mod cursor;
pub mod deepseek;
pub mod deepseek_models;
pub mod devin;
pub mod devin_models;
pub mod faux;
pub mod fireworks;
pub mod fireworks_models;
pub mod github_copilot;
pub mod github_copilot_models;
pub mod google;
pub mod google_shared;
pub mod google_vertex;
pub mod google_vertex_models;
pub mod google_models;
pub mod groq;
pub mod groq_models;
pub mod huggingface;
pub mod huggingface_models;
pub mod images;
pub mod kimi_coding;
pub mod kimi_coding_auth;
pub mod kimi_coding_models;
pub mod minimax;
pub mod minimax_cn;
pub mod minimax_cn_models;
pub mod minimax_models;
pub mod mistral;
pub mod mistral_models;
pub mod moonshotai;
pub mod moonshotai_cn;
pub mod moonshotai_cn_models;
pub mod moonshotai_models;
pub mod nvidia;
pub mod nvidia_models;
pub mod ollama;
pub mod openai;
pub mod openai_completions;
pub mod openai_images;
pub mod openai_responses;
pub mod openai_responses_shared;
pub mod openai_models;
pub mod opencode;
pub mod opencode_go;
pub mod opencode_go_models;
pub mod opencode_headers;
pub mod opencode_models;
pub mod opengateway;
pub mod opengateway_models;
pub mod openrouter;
pub mod openrouter_images;
pub mod openrouter_models;
pub mod qwen_token_plan;
pub mod qwen_token_plan_cn;
pub mod qwen_token_plan_cn_models;
pub mod qwen_token_plan_individual;
pub mod qwen_token_plan_individual_models;
pub mod qwen_token_plan_models;
pub mod radius;
pub mod radius_config;
pub mod register_builtins;
pub mod together;
pub mod together_models;
pub mod venice;
pub mod venice_models;
pub mod vercel_ai_gateway;
pub mod vercel_ai_gateway_models;
pub mod xai;
pub mod xai_models;
pub mod xiaomi;
pub mod xiaomi_token_plan_ams;
pub mod xiaomi_token_plan_ams_models;
pub mod xiaomi_token_plan_cn;
pub mod xiaomi_token_plan_cn_models;
pub mod xiaomi_token_plan_sgp;
pub mod xiaomi_token_plan_sgp_models;
pub mod xiaomi_models;
pub mod zai;
pub mod zai_coding_cn;
pub mod zai_coding_cn_models;
pub mod zai_models;

/// Port of `../api/lazy.ts` `lazyApi()`'s error-terminated-stream fallback, specialized to the
/// builtin api-registry: a provider factory here names its api by string id (todos 10-12's wire
/// modules), and this adapter resolves that id through `api_registry::get_builtin_api_provider`
/// at call time so provider construction never depends on another node's module being linked.
/// An unregistered api id yields the same shape of setup-error stream `lazyStream` produces for a
/// failed dynamic import.
struct BuiltinApiStreams {
    api_id: &'static str,
}

impl ProviderStreams for BuiltinApiStreams {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        match get_builtin_api_provider(self.api_id) {
            Some(provider) => provider.streams().stream(model, context, options),
            None => error_stream(model, &format!("No API provider registered for api: {}", self.api_id)),
        }
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        match get_builtin_api_provider(self.api_id) {
            Some(provider) => provider.streams().stream_simple(model, context, options),
            None => error_stream(model, &format!("No API provider registered for api: {}", self.api_id)),
        }
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        get_builtin_api_provider(self.api_id)?.streams().fetch_deferred(model, handle, options)
    }

    fn supports_deferred(&self) -> bool {
        get_builtin_api_provider(self.api_id).is_some_and(|provider| provider.streams().supports_deferred())
    }
}

/// A `ProviderStreams` that resolves `api_id` through the builtin api registry on every call,
/// mirroring how each `providers/*.ts` factory names its api (e.g. `openAICompletionsApi()`)
/// without a hard link to the wire module that eventually registers it.
pub fn builtin_api_streams(api_id: &'static str) -> Arc<dyn ProviderStreams> {
    Arc::new(BuiltinApiStreams { api_id })
}

/// Ports a `*.models.ts` shard: the generated catalog (`models_generated::MODELS`) is already
/// flattened per provider by `tools/golden/gen-models.mjs`, so this simply reads that provider's
/// slice back out in senpi's declared order. Panics only if the embedded catalog is missing the
/// provider, which would mean the generator itself is broken (same invariant TS relies on via
/// `Object.values` on a statically known import).
pub fn builtin_provider_models(provider: &str) -> Vec<Model> {
    get_builtin_provider_models(provider)
        .unwrap_or_else(|error| panic!("embedded models.json is missing provider {provider}: {error}"))
        .into_iter()
        .cloned()
        .collect()
}

/// `ModelGroups`-shaped re-export for a provider's own `*_models.rs`, kept for parity with
/// `flattenModelCatalog`'s call shape even though `models_generated::MODELS` is pre-flattened.
pub fn flatten_provider_models(provider: &str) -> IndexMap<String, Model> {
    let mut groups = IndexMap::new();
    groups.insert(provider.to_owned(), builtin_provider_models(provider).into_iter().map(|m| (m.id.clone(), m)).collect());
    flatten_model_catalog(provider, &groups)
}

/// Ports an images `*.models.ts` read: `Object.values(IMAGE_MODELS[provider])`.
pub fn builtin_images_provider_models(provider: &str) -> Vec<ImagesModel> {
    IMAGE_MODELS
        .get(provider)
        .unwrap_or_else(|| panic!("embedded image-models.json is missing provider {provider}"))
        .values()
        .cloned()
        .collect()
}

/// `envApiKeyAuth(name, envVars)` (auth/helpers.ts, todo 13) as the images-side resolver: the
/// provider's known env vars are the seam todo 5 ported in `env_api_keys`.
pub fn env_api_key_images_auth(provider_id: &'static str) -> ResolveImagesAuth {
    Arc::new(move |overrides| {
        let api_key = overrides.api_key.clone();
        let env = overrides.env.clone();
        Box::pin(async move {
            let resolved = api_key.or_else(|| get_env_api_key(provider_id, env.as_ref()));
            Ok(resolved.map(|api_key| AuthResolution {
                auth: ProviderAuthResult { api_key: Some(api_key), ..ProviderAuthResult::default() },
                env,
            }))
        })
    })
}

#[cfg(test)]
mod tests;
