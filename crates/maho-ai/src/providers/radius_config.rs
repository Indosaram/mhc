//! Port of senpi packages/ai/src/providers/radius-config.ts.

use crate::models::Credential;
use crate::types::{InputModality, Model, ModelCost, ThinkingLevelMap};
use serde_json::Value;
use url::Url;

pub const DEFAULT_RADIUS_GATEWAY: &str = "https://radius.pi.dev";

#[derive(Debug, Clone, PartialEq)]
pub struct RadiusGatewayModel {
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Vec<InputModality>,
    pub cost: ModelCost,
    pub context_window: u64,
    pub max_tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RadiusGatewayConfig {
    pub base_url: String,
    pub models: Vec<RadiusGatewayModel>,
}

fn is_radius_gateway_model(value: &Value) -> bool {
    let Some(model) = value.as_object() else { return false };
    model.get("id").is_some_and(Value::is_string)
        && model.get("name").is_some_and(Value::is_string)
        && model.get("reasoning").is_some_and(Value::is_boolean)
        && model.get("input").is_some_and(Value::is_array)
        && model.get("cost").is_some_and(|cost| cost.is_object())
        && model.get("contextWindow").is_some_and(Value::is_number)
        && model.get("maxTokens").is_some_and(Value::is_number)
}

fn gateway_model(value: &Value) -> RadiusGatewayModel {
    let model = value.as_object().expect("validated radius gateway model");
    let thinking_level_map = model
        .get("thinkingLevelMap")
        .and_then(|map| serde_json::from_value::<ThinkingLevelMap>(map.clone()).ok());
    RadiusGatewayModel {
        id: model["id"].as_str().unwrap_or_default().to_owned(),
        name: model["name"].as_str().unwrap_or_default().to_owned(),
        reasoning: model["reasoning"].as_bool().unwrap_or(false),
        thinking_level_map,
        input: model
            .get("input")
            .and_then(Value::as_array)
            .map(|input| input.iter().filter_map(|entry| serde_json::from_value(entry.clone()).ok()).collect())
            .unwrap_or_default(),
        cost: model.get("cost").and_then(|cost| serde_json::from_value(cost.clone()).ok()).unwrap_or_default(),
        context_window: model["contextWindow"].as_u64().unwrap_or_default(),
        max_tokens: model["maxTokens"].as_u64().unwrap_or_default(),
    }
}

pub fn sanitize_radius_gateway_config(config: &Value) -> Option<RadiusGatewayConfig> {
    let config = config.as_object()?;
    let base_url = config.get("baseUrl")?.as_str()?.to_owned();
    let models = config.get("models")?.as_array()?;
    Some(RadiusGatewayConfig {
        base_url,
        models: models.iter().filter(|model| is_radius_gateway_model(model)).map(gateway_model).collect(),
    })
}

pub fn normalize_radius_gateway_url(value: &str) -> String {
    let lower = value.to_lowercase();
    let with_scheme = if lower.starts_with("http://") || lower.starts_with("https://") {
        value.to_owned()
    } else {
        format!("https://{value}")
    };
    with_scheme.trim_end_matches('/').to_owned()
}

pub fn get_radius_credential_config(credential: Option<&Credential>) -> Option<RadiusGatewayConfig> {
    sanitize_radius_gateway_config(credential?.get("gatewayConfig")?)
}

pub fn get_radius_models_from_config(provider_id: &str, config: &RadiusGatewayConfig) -> Vec<Model> {
    config
        .models
        .iter()
        .map(|model| Model {
            id: model.id.clone(),
            name: model.name.clone(),
            api: "pi-messages".to_owned(),
            provider: provider_id.to_owned(),
            base_url: config.base_url.clone(),
            reasoning: model.reasoning,
            thinking_level_map: model.thinking_level_map.clone(),
            input: model.input.clone(),
            cost: model.cost.clone(),
            context_window: model.context_window,
            max_tokens: model.max_tokens,
            sampling_params: None,
            headers: None,
            cache_retention: None,
            upstream_model_id: None,
            service_tier: None,
            recover_text_tool_calls: None,
            compat: None,
        })
        .collect()
}

pub fn get_radius_models(provider_id: &str, credential: Option<&Credential>) -> Vec<Model> {
    match get_radius_credential_config(credential) {
        Some(config) => get_radius_models_from_config(provider_id, &config),
        None => Vec::new(),
    }
}

fn truncate_http_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() > 512 {
        format!("{}…", trimmed.chars().take(512).collect::<String>())
    } else {
        trimmed.to_owned()
    }
}

pub async fn load_radius_gateway_config(
    gateway: &str,
    api_key: Option<&str>,
    signal: &crate::utils::abort::AbortSignal,
    fetch: &reqwest::Client,
) -> Result<RadiusGatewayConfig, String> {
    let url = Url::parse(gateway)
        .and_then(|base| base.join("/v1/config"))
        .map_err(|error| format!("Could not load Radius config from {gateway}: {error}"))?;
    let mut request = fetch.get(url).header("accept", "application/json");
    if let Some(api_key) = api_key {
        request = request.header("authorization", format!("Bearer {api_key}"));
    }
    let response = tokio::select! {
        biased;
        () = signal.cancelled() => return Err(format!("Could not load Radius config from {gateway}: aborted")),
        response = request.send() => response.map_err(|error| format!("Could not load Radius config from {gateway}: {error}"))?,
    };
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Could not load Radius config from {gateway}: {status}: {}", truncate_http_body(&body)));
    }
    let value: Value = response.json().await.map_err(|error| format!("Invalid Radius config from {gateway}: {error}"))?;
    sanitize_radius_gateway_config(&value).ok_or_else(|| format!("Invalid Radius config from {gateway}"))
}
