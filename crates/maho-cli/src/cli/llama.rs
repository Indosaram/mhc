//! Port of senpi `packages/coding-agent/src/extensions/llama/` (client + provider + index) at pin
//! `fe8c564bf33a2cbbdbbba99c9bd8b45b21e37407`. Registers the `llama.cpp` provider with a dynamic
//! server catalog and the `/llama` command; the pinned TUI (`ui.ts`) and HuggingFace flow
//! (`huggingface.ts`) depend on the interactive lane.

use std::sync::Arc;

use maho_ai::types::{InputModality, ModelCost};
use maho_ext_api::{
    Extension, ExtensionApi, NotificationType, ProviderConfig, ProviderModelConfig, ProviderRefresh,
};
use serde_json::Value;

pub const LLAMA_PROVIDER_ID: &str = "llama.cpp";
pub const DEFAULT_LLAMA_SERVER_URL: &str = "http://127.0.0.1:8080";

/// senpi `normalizeLlamaServerUrl`: drop a trailing `/` and a trailing `/v1` so the stored value is
/// the host root; `llamaInferenceUrl` re-appends `/v1`.
pub fn normalize_llama_server_url(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    let stripped = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    stripped.trim_end_matches('/').to_owned()
}

/// senpi `llamaInferenceUrl`: the OpenAI-compatible inference root.
pub fn llama_inference_url(server_url: &str) -> String {
    format!("{}/v1", normalize_llama_server_url(server_url))
}

fn configured_server_url() -> String {
    std::env::var("LLAMA_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| normalize_llama_server_url(&value))
        .unwrap_or_else(|| DEFAULT_LLAMA_SERVER_URL.to_owned())
}

/// senpi `LlamaClient.list`: GET the OpenAI-compatible `/v1/models` endpoint and read `data`.
pub async fn list_models(server_url: &str, api_key: Option<&str>) -> Result<Vec<Value>, String> {
    let mut request = reqwest::Client::new()
        .get(format!("{}/models", llama_inference_url(server_url)))
        .header("accept", "application/json");
    if let Some(api_key) = api_key.filter(|key| !key.is_empty()) {
        request = request.header("authorization", format!("Bearer {api_key}"));
    }
    let response = request.send().await.map_err(|error| format!("Could not reach the llama.cpp server: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("llama.cpp server returned HTTP {}", response.status().as_u16()));
    }
    let value: Value = response.json().await.map_err(|error| format!("Invalid llama.cpp catalog response: {error}"))?;
    Ok(value.get("data").and_then(Value::as_array).cloned().unwrap_or_default())
}

/// senpi `toPiModel`: an `openai-completions` model with the reported (or default) context window.
fn model_config(entry: &Value, server_url: &str) -> Option<ProviderModelConfig> {
    let id = entry.get("id").and_then(Value::as_str)?.to_owned();
    let context_window = entry
        .get("meta")
        .and_then(|meta| meta.get("n_ctx").or_else(|| meta.get("n_ctx_train")))
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(128_000);
    let has_image = entry
        .get("architecture")
        .and_then(|architecture| architecture.get("input_modalities"))
        .and_then(Value::as_array)
        .is_some_and(|modalities| modalities.iter().any(|value| value.as_str() == Some("image")));
    let input = if has_image { vec![InputModality::Text, InputModality::Image] } else { vec![InputModality::Text] };
    Some(ProviderModelConfig {
        id: id.clone(),
        name: id,
        upstream_model_id: None,
        api: Some("openai-completions".to_owned()),
        base_url: Some(llama_inference_url(server_url)),
        reasoning: false,
        recover_text_tool_calls: None,
        thinking_level_map: None,
        input,
        cost: ModelCost { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0, tiers: None },
        context_window,
        max_tokens: context_window,
        headers: None,
        extra_body: None,
        compat: None,
    })
}

pub struct LlamaExtension;

impl Extension for LlamaExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let refresh: ProviderRefresh = Arc::new(|context| {
            Box::pin(async move {
                let server_url = context
                    .credential
                    .as_ref()
                    .and_then(|credential| credential.get("env"))
                    .and_then(|env| env.get("LLAMA_BASE_URL"))
                    .and_then(Value::as_str)
                    .map(normalize_llama_server_url)
                    .unwrap_or_else(configured_server_url);
                let api_key = context
                    .credential
                    .as_ref()
                    .and_then(|credential| credential.get("key"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let catalog = list_models(&server_url, api_key.as_deref()).await.unwrap_or_default();
                Ok(catalog.iter().filter_map(|entry| model_config(entry, &server_url)).collect())
            })
        });
        let _ = api.register_provider(
            LLAMA_PROVIDER_ID,
            ProviderConfig {
                name: Some("llama.cpp".to_owned()),
                base_url: Some(llama_inference_url(&configured_server_url())),
                api: Some("openai-completions".to_owned()),
                models: Some(Vec::new()),
                refresh_models: Some(refresh),
                ..ProviderConfig::default()
            },
        );
        api.register_command(
            "llama",
            Some("Manage llama.cpp router models".to_owned()),
            Some("[list]".to_owned()),
            Arc::new(|_args, ctx| {
                Box::pin(async move {
                    let server_url = configured_server_url();
                    match list_models(&server_url, None).await {
                        Ok(models) => {
                            let lines: Vec<String> = models
                                .iter()
                                .filter_map(|entry| entry.get("id").and_then(Value::as_str).map(str::to_owned))
                                .collect();
                            let message = if lines.is_empty() {
                                format!("llama.cpp at {server_url}: no models")
                            } else {
                                format!("llama.cpp at {server_url}:\n{}", lines.join("\n"))
                            };
                            ctx.ui.notify(&message, NotificationType::Info);
                        }
                        Err(error) => ctx.ui.notify(&error, NotificationType::Error),
                    }
                    Ok(())
                })
            }),
        );
    }
}

pub fn llama() -> Box<dyn Extension> {
    Box::new(LlamaExtension)
}
