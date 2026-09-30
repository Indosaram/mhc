//! Port of senpi packages/ai/src/providers/ollama.ts.

use crate::model::ModelCompat;
use crate::models::{CreateProviderOptions, FetchModels, ModelsError, ModelsErrorCode, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use crate::models::RefreshModelsContext;
use crate::types::{InputModality, Model, ModelCost};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use url::Url;

pub const DEFAULT_OLLAMA_CLOUD_URL: &str = "https://ollama.com";
pub const DEFAULT_OLLAMA_CONTEXT_WINDOW: u64 = 128000;
pub const DEFAULT_OLLAMA_MAX_TOKENS: u64 = 16384;
const OLLAMA_SHOW_CONCURRENCY: usize = 6;

#[derive(Clone, Default)]
pub struct OllamaProviderOptions {
    pub base_url: Option<String>,
    pub fetch: Option<reqwest::Client>,
}

fn normalize_ollama_host(value: &str) -> String {
    let mut host = Url::parse(value).unwrap_or_else(|error| panic!("invalid Ollama base URL {value}: {error}"));
    let path = host.path().to_owned();
    let trimmed = path
        .strip_suffix("/v1/")
        .or_else(|| path.strip_suffix("/v1"))
        .map(str::to_owned)
        .unwrap_or(path);
    host.set_path(&trimmed);
    let text = host.to_string();
    text.strip_suffix('/').map(str::to_owned).unwrap_or(text)
}

async fn fetch_ollama_json(
    url: Url,
    api_key: &str,
    fetch: &reqwest::Client,
    body: Option<Value>,
) -> Result<Value, String> {
    let mut request = fetch
        .request(if body.is_some() { reqwest::Method::POST } else { reqwest::Method::GET }, url.clone())
        .header("accept", "application/json")
        .header("authorization", format!("Bearer {api_key}"));
    if let Some(body) = body {
        request = request.header("content-type", "application/json").json(&body);
    }
    let response = request.send().await.map_err(|error| format!("Could not load Ollama catalog from {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("Could not load Ollama catalog from {url}: {}", response.status().as_u16()));
    }
    response.json::<Value>().await.map_err(|error| format!("Could not load Ollama catalog from {url}: {error}"))
}

fn parse_tags_response(value: &Value) -> Result<Vec<String>, String> {
    let Some(models) = value.get("models").and_then(Value::as_array) else {
        return Err("Invalid Ollama tags response".to_owned());
    };
    let mut names = Vec::new();
    for entry in models {
        let Some(entry) = entry.as_object() else { continue };
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| entry.get("model").and_then(Value::as_str));
        if let Some(name) = name {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

struct OllamaShowResponse {
    capabilities: Vec<String>,
    model_info: Map<String, Value>,
}

fn parse_show_response(value: &Value) -> Result<OllamaShowResponse, String> {
    let Some(capabilities) = value.get("capabilities").and_then(Value::as_array) else {
        return Err("Invalid Ollama show response".to_owned());
    };
    Ok(OllamaShowResponse {
        capabilities: capabilities.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        model_info: value.get("model_info").and_then(Value::as_object).cloned().unwrap_or_default(),
    })
}

fn number_value(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok().filter(|parsed| parsed.is_finite()),
        Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        Value::Null => Some(0.0),
        _ => None,
    }
}

fn get_context_window(model_info: &Map<String, Value>) -> u64 {
    let preferred = model_info
        .get("general.architecture")
        .and_then(Value::as_str)
        .and_then(|architecture| model_info.get(&format!("{architecture}.context_length")));
    let candidate = preferred
        .or_else(|| model_info.iter().find(|(key, _)| key.ends_with(".context_length")).map(|(_, value)| value));
    match candidate.and_then(number_value) {
        Some(parsed) if parsed > 0.0 => parsed as u64,
        _ => DEFAULT_OLLAMA_CONTEXT_WINDOW,
    }
}

fn ollama_compat() -> ModelCompat {
    let mut compat = Map::new();
    compat.insert("supportsStore".to_owned(), Value::Bool(false));
    compat.insert("supportsDeveloperRole".to_owned(), Value::Bool(false));
    compat.insert("supportsReasoningEffort".to_owned(), Value::Bool(true));
    compat.insert("maxTokensField".to_owned(), Value::String("max_tokens".to_owned()));
    compat.insert("supportsStrictMode".to_owned(), Value::Bool(false));
    compat.insert("supportsLongCacheRetention".to_owned(), Value::Bool(false));
    ModelCompat(compat)
}

fn to_ollama_model(host: &str, name: &str, show: &OllamaShowResponse) -> Option<Model> {
    if !show.capabilities.iter().any(|capability| capability == "tools") {
        return None;
    }
    Some(Model {
        id: name.to_owned(),
        name: name.to_owned(),
        api: "openai-completions".to_owned(),
        provider: "ollama".to_owned(),
        base_url: format!("{host}/v1"),
        reasoning: show.capabilities.iter().any(|capability| capability == "thinking"),
        thinking_level_map: None,
        input: if show.capabilities.iter().any(|capability| capability == "vision") {
            vec![InputModality::Text, InputModality::Image]
        } else {
            vec![InputModality::Text]
        },
        cost: ModelCost::default(),
        context_window: get_context_window(&show.model_info),
        max_tokens: DEFAULT_OLLAMA_MAX_TOKENS,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: Some(ollama_compat()),
    })
}

enum Inspection {
    Fulfilled(Option<Box<Model>>),
    Rejected(String),
}

async fn inspect_ollama_model(host: &str, fetch: &reqwest::Client, api_key: &str, name: &str) -> Inspection {
    let show_url = Url::parse(&format!("{host}/api/show")).expect("ollama show url");
    let body = serde_json::json!({ "model": name });
    match fetch_ollama_json(show_url, api_key, fetch, Some(body)).await {
        Ok(value) => match parse_show_response(&value) {
            Ok(show) => Inspection::Fulfilled(to_ollama_model(host, name, &show).map(Box::new)),
            Err(error) => Inspection::Rejected(error),
        },
        Err(error) => Inspection::Rejected(error),
    }
}

async fn fetch_ollama_models(
    host: &str,
    fetch: &reqwest::Client,
    credential: Option<crate::models::Credential>,
    signal: crate::utils::abort::AbortSignal,
    stored: Option<crate::models_store::ModelsStoreEntry>,
) -> Result<Vec<Model>, ModelsError> {
    let api_key = credential
        .as_ref()
        .filter(|credential| credential.get("type").and_then(Value::as_str) == Some("api_key"))
        .and_then(|credential| credential.get("key"))
        .and_then(Value::as_str);
    let Some(api_key) = api_key else { return Ok(Vec::new()) };

    let tags_url = Url::parse(&format!("{host}/api/tags")).expect("ollama tags url");
    let tags = fetch_ollama_json(tags_url, api_key, fetch, None)
        .await
        .and_then(|value| parse_tags_response(&value))
        .map_err(|error| ModelsError::new(ModelsErrorCode::ModelSource, error))?;

    let mut slots: Vec<Option<Inspection>> = (0..tags.len()).map(|_| None).collect();
    let mut cursor = 0;
    while cursor < tags.len() {
        let wave: Vec<(usize, String)> = tags[cursor..std::cmp::min(cursor + OLLAMA_SHOW_CONCURRENCY, tags.len())]
            .iter()
            .cloned()
            .enumerate()
            .map(|(offset, name)| (cursor + offset, name))
            .collect();
        let results = futures::future::join_all(wave.into_iter().map(|(index, name)| {
            let host = host.to_owned();
            let fetch = fetch.clone();
            let api_key = api_key.to_owned();
            async move { (index, inspect_ollama_model(&host, &fetch, &api_key, &name).await) }
        }))
        .await;
        for (index, inspection) in results {
            slots[index] = Some(inspection);
        }
        cursor += OLLAMA_SHOW_CONCURRENCY;
    }
    let inspections: Vec<Inspection> = slots.into_iter().map(|slot| slot.expect("every tag inspected")).collect();

    let rejected: Vec<&String> = inspections
        .iter()
        .filter_map(|inspection| match inspection {
            Inspection::Rejected(reason) => Some(reason),
            Inspection::Fulfilled(_) => None,
        })
        .collect();
    if signal.aborted() {
        let reason = rejected.first().map(|reason| (*reason).clone()).unwrap_or_else(|| {
            signal.reason().map(|reason| reason.message).unwrap_or_else(|| "Ollama catalog refresh aborted".to_owned())
        });
        return Err(ModelsError::new(ModelsErrorCode::ModelSource, reason));
    }
    if !tags.is_empty() && rejected.len() == tags.len() {
        return Err(ModelsError::new(ModelsErrorCode::ModelSource, rejected[0].clone()));
    }

    let previous_by_id: HashMap<String, Model> = stored
        .as_ref()
        .map(|stored| {
            stored
                .models
                .iter()
                .filter(|model| model.provider == "ollama" && model.api == "openai-completions")
                .map(|model| (model.id.clone(), model.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut models = Vec::new();
    for (name, inspection) in tags.iter().zip(inspections.iter()) {
        match inspection {
            Inspection::Fulfilled(Some(model)) => models.push((**model).clone()),
            Inspection::Fulfilled(None) => {}
            Inspection::Rejected(_) => {
                if let Some(previous) = previous_by_id.get(name) {
                    models.push(previous.clone());
                }
            }
        }
    }
    if !rejected.is_empty() && models.is_empty() {
        return Err(ModelsError::new(ModelsErrorCode::ModelSource, rejected[0].clone()));
    }
    Ok(models)
}

pub fn ollama_provider(options: Option<OllamaProviderOptions>) -> Arc<dyn Provider> {
    let options = options.unwrap_or_default();
    let host = normalize_ollama_host(options.base_url.as_deref().unwrap_or(DEFAULT_OLLAMA_CLOUD_URL));
    let fetch = options.fetch.clone().unwrap_or_default();
    let base_url = format!("{host}/v1");
    let fetch_models: FetchModels = {
        let host = host.clone();
        let fetch = fetch.clone();
        Arc::new(move |context: &RefreshModelsContext| {
            let host = host.clone();
            let fetch = fetch.clone();
            let credential = context.credential.clone();
            let signal = context.signal.clone();
            let stored = context.stored.clone();
            Box::pin(async move { fetch_ollama_models(&host, &fetch, credential, signal, stored).await })
        })
    };
    create_provider(CreateProviderOptions {
        id: "ollama".into(),
        name: Some("Ollama Cloud".into()),
        base_url: Some(base_url),
        headers: None,
        models: Vec::new(),
        fetch_models: Some(fetch_models),
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
