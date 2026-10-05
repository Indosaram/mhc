//! Port of `packages/coding-agent/src/extensions/llama/provider.ts` (pin `fe8c564b`).

use std::sync::Arc;

use maho_ai::model::ModelCompat;
use maho_ai::types::{InputModality, ModelCost};
use maho_ext_api::{ExtensionFuture, ProviderConfig, ProviderModelConfig, ProviderRefresh};
use serde_json::Value;

use crate::client::{LlamaClient, LlamaModelInfo, llama_inference_url, normalize_llama_server_url};

pub const LLAMA_PROVIDER_ID: &str = "llama.cpp";
pub const DEFAULT_LLAMA_SERVER_URL: &str = "http://127.0.0.1:8080";
pub const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

fn credential_server_url(credential: &Value) -> Option<String> {
    let value = credential.get("env")?.get("LLAMA_BASE_URL")?.as_str()?;
    (!value.trim().is_empty()).then(|| normalize_llama_server_url(value).ok()).flatten()
}

fn resolve_server_url(credential: Option<&Value>) -> Option<String> {
    credential.and_then(credential_server_url).or_else(|| {
        std::env::var("LLAMA_BASE_URL").ok().map(|value| value.trim().to_owned()).filter(|value| !value.is_empty()).and_then(|value| normalize_llama_server_url(&value).ok())
    })
}

fn model_is_selectable(model: &LlamaModelInfo, router_autoload: bool) -> bool {
    match model.status.value.as_str() {
        "loaded" | "sleeping" => true,
        "unloaded" => router_autoload && model.status.failed != Some(true) && model.source.as_deref() == Some("preset"),
        _ => false,
    }
}

async fn router_autoload_enabled(client: &LlamaClient, catalog: &[LlamaModelInfo]) -> bool {
    if !catalog.iter().any(|model| model.status.value == "unloaded" && model.source.as_deref() == Some("preset")) {
        return false;
    }
    client.props(None).await.ok().and_then(|props| props.models_autoload) == Some(true)
}

fn to_provider_model_config(model: &LlamaModelInfo, server_url: &str) -> ProviderModelConfig {
    let reported = model.meta.as_ref().and_then(|meta| meta.n_ctx.or(meta.n_ctx_train));
    let context_window = reported.filter(|value| *value > 0).unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let has_image = model
        .architecture
        .as_ref()
        .and_then(|architecture| architecture.input_modalities.as_ref())
        .is_some_and(|modalities| modalities.iter().any(|value| value == "image"));
    ProviderModelConfig {
        id: model.id.clone(),
        name: model.id.clone(),
        upstream_model_id: None,
        api: Some("openai-completions".to_owned()),
        base_url: llama_inference_url(server_url).ok(),
        reasoning: false,
        recover_text_tool_calls: None,
        thinking_level_map: None,
        input: if has_image { vec![InputModality::Text, InputModality::Image] } else { vec![InputModality::Text] },
        cost: ModelCost { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0, tiers: None },
        context_window,
        max_tokens: context_window,
        headers: None,
        extra_body: None,
        compat: Some(ModelCompat { supports_store: Some(false), supports_developer_role: Some(false), supports_reasoning_effort: Some(false), supports_usage_in_streaming: Some(true), supports_strict_mode: Some(false), max_tokens_field: Some("max_tokens".to_owned()), ..Default::default() }),
    }
}

pub async fn refresh_models(credential: Option<&Value>, allow_network: bool) -> Result<Vec<ProviderModelConfig>, String> {
    let Some(server_url) = resolve_server_url(credential) else { return Ok(Vec::new()); };
    if !allow_network {
        return Ok(Vec::new());
    }
    let api_key = credential.and_then(|credential| credential.get("key")).and_then(Value::as_str);
    let client = LlamaClient::new(&server_url, api_key)?;
    let catalog = client.list(false, None).await?;
    let router_autoload = router_autoload_enabled(&client, &catalog).await;
    Ok(catalog.iter().filter(|model| model_is_selectable(model, router_autoload)).map(|model| to_provider_model_config(model, &server_url)).collect())
}

pub fn llama_provider_config() -> ProviderConfig {
    let refresh: ProviderRefresh = Arc::new(|context| {
        let credential = context.credential.clone();
        let allow_network = context.allow_network;
        Box::pin(async move { refresh_models(credential.as_ref(), allow_network).await }) as ExtensionFuture<'static, Vec<ProviderModelConfig>>
    });
    ProviderConfig {
        name: Some("llama.cpp".to_owned()),
        base_url: Some(llama_inference_url(DEFAULT_LLAMA_SERVER_URL).unwrap_or_default()),
        api: Some("openai-completions".to_owned()),
        models: Some(Vec::new()),
        refresh_models: Some(refresh),
        ..ProviderConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{LlamaModelArchitecture, LlamaModelInfo, LlamaModelMeta, LlamaModelState};

    fn model(status: &str, source: Option<&str>) -> LlamaModelInfo {
        LlamaModelInfo { id: "m".to_owned(), status: LlamaModelState { value: status.to_owned(), ..Default::default() }, source: source.map(str::to_owned), ..Default::default() }
    }

    #[test]
    fn selectable_models_follow_the_pinned_rule() {
        assert!(model_is_selectable(&model("loaded", None), false));
        assert!(model_is_selectable(&model("sleeping", None), false));
        assert!(!model_is_selectable(&model("unloaded", None), false));
        assert!(model_is_selectable(&model("unloaded", Some("preset")), true));
        assert!(!model_is_selectable(&model("unloaded", Some("preset")), false));
        let mut failed = model("unloaded", Some("preset"));
        failed.status.failed = Some(true);
        assert!(!model_is_selectable(&failed, true));
    }

    #[test]
    fn context_window_defaults_and_uses_reported_meta() {
        let mut reported = model("loaded", None);
        reported.meta = Some(LlamaModelMeta { n_ctx: Some(4096), ..Default::default() });
        assert_eq!(to_provider_model_config(&reported, "http://x").context_window, 4096);
        assert_eq!(to_provider_model_config(&model("loaded", None), "http://x").context_window, DEFAULT_CONTEXT_WINDOW);
    }

    #[test]
    fn image_input_modality_maps_from_architecture() {
        let mut with_image = model("loaded", None);
        with_image.architecture = Some(LlamaModelArchitecture { input_modalities: Some(vec!["text".to_owned(), "image".to_owned()]), output_modalities: None });
        assert!(to_provider_model_config(&with_image, "http://x").input.contains(&InputModality::Image));
        assert_eq!(to_provider_model_config(&model("loaded", None), "http://x").input, vec![InputModality::Text]);
    }
}
