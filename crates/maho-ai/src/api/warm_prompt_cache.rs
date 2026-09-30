//! Port of senpi packages/ai/src/api/warm-prompt-cache.ts.
//!
//! The TS module builds its request body with `buildAnthropicWarmPromptCacheParams` from
//! anthropic-messages.ts (node 11-anthropic). That builder is injected here so this module keeps
//! the TS control flow without duplicating the Anthropic converter; the default builder is wired
//! once 11-anthropic lands.

use serde_json::{json, Map, Value};

use crate::types::{Context, Model, ProviderHeaders, StreamOptions};
use crate::utils::headers::provider_headers_to_record;
use crate::utils::prompt_cache_ttl::is_anthropic_api_base_url;

#[derive(Debug, Clone, PartialEq)]
pub struct WarmPromptCacheUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WarmPromptCacheResult {
    Unsupported,
    Supported { usage: WarmPromptCacheUsage, usage_raw: Value },
}

pub type WarmPromptCacheOptions = StreamOptions;
pub type BuildWarmPromptCacheParams<'a> =
    &'a dyn Fn(&Model, &Context, &StreamOptions) -> Result<Value, String>;

pub async fn warm_prompt_cache(
    model: &Model,
    context: &Context,
    options: Option<StreamOptions>,
    build_params: BuildWarmPromptCacheParams<'_>,
) -> Result<WarmPromptCacheResult, String> {
    if model.api != "anthropic-messages" || !is_anthropic_api_base_url(&model.base_url) {
        return Ok(WarmPromptCacheResult::Unsupported);
    }

    let options = options.unwrap_or_default();
    let headers = provider_headers_to_record(options.request.headers.as_ref());
    let mut params = build_params(model, context, &options)?;
    if let Some(on_payload) = &options.request.on_payload
        && let Some(transformed) = on_payload(
            &params,
            model,
            Some(&crate::types::ProviderRequestMetadata {
                model: model.clone(),
                headers: options.request.headers.clone().unwrap_or_default(),
            }),
    ) {
        params = transformed;
    }

    let mut sanitized = params.as_object().cloned().unwrap_or_else(Map::new);
    sanitized.remove("stream");
    sanitized.remove("thinking");
    sanitized.remove("output_config");
    sanitized.remove("tool_choice");
    sanitized.insert("max_tokens".into(), Value::from(0));

    let url = format!("{}/v1/messages", model.base_url.trim_end_matches('/'));
    let client = options.request.fetch.clone().unwrap_or_default();
    let mut request_headers = reqwest::header::HeaderMap::new();
    request_headers.insert(
        reqwest::header::HeaderName::from_static("anthropic-version"),
        reqwest::header::HeaderValue::from_static("2023-06-01"),
    );
    request_headers.insert(
        reqwest::header::HeaderName::from_static("anthropic-beta"),
        reqwest::header::HeaderValue::from_static("prompt-caching-2024-07-31"),
    );
    if let Some(api_key) = options.request.api_key.as_deref()
        && !api_key.is_empty()
    {
        request_headers.insert(
            reqwest::header::HeaderName::from_static("x-api-key"),
            reqwest::header::HeaderValue::from_str(api_key).map_err(|error| error.to_string())?,
        );
    }
    if let Some(model_headers) = &model.headers {
        for (name, value) in model_headers {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                reqwest::header::HeaderValue::from_str(value),
            ) {
                request_headers.insert(name, value);
            }
        }
    }
    if let Some(headers) = &headers {
        for (name, value) in headers {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                reqwest::header::HeaderValue::from_str(value),
            ) {
                request_headers.insert(name, value);
            }
        }
    }

    let response = client
        .post(&url)
        .headers(request_headers)
        .json(&json!(sanitized))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("{status}: {body}"));
    }
    let body: Value = response.json().await.map_err(|error| error.to_string())?;
    let usage_raw = body.get("usage").cloned().unwrap_or(Value::Null);
    let usage = WarmPromptCacheUsage {
        input: usage_raw.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
        output: usage_raw.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        cache_read: usage_raw.get("cache_read_input_tokens").and_then(Value::as_u64).unwrap_or(0),
        cache_write: usage_raw.get("cache_creation_input_tokens").and_then(Value::as_u64).unwrap_or(0),
    };
    Ok(WarmPromptCacheResult::Supported { usage, usage_raw })
}

pub fn sanitize_warm_prompt_cache_params(params: &Value) -> Map<String, Value> {
    let mut sanitized = params.as_object().cloned().unwrap_or_else(Map::new);
    sanitized.remove("stream");
    sanitized.remove("thinking");
    sanitized.remove("output_config");
    sanitized.remove("tool_choice");
    sanitized.insert("max_tokens".into(), Value::from(0));
    sanitized
}

pub fn headers_for_warm_prompt_cache(model: &Model, options: &StreamOptions) -> ProviderHeaders {
    let mut headers: ProviderHeaders = model
        .headers
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|(name, value)| (name, Some(value)))
        .collect();
    if let Some(overrides) = &options.request.headers {
        for (name, value) in overrides {
            headers.insert(name.clone(), value.clone());
        }
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(api: &str, base_url: &str) -> Model {
        let mut model = crate::models_generated::get_builtin_provider_models("anthropic").expect("models")[0].clone();
        model.api = api.into();
        model.base_url = base_url.into();
        model
    }

    #[tokio::test]
    async fn non_native_anthropic_lanes_report_unsupported_without_a_request() {
        let builder = |_model: &Model, _context: &Context, _options: &StreamOptions| Ok(json!({}));
        for (api, base_url) in [
            ("openai-completions", "https://api.anthropic.com"),
            ("anthropic-messages", "https://example.com"),
            ("anthropic-messages", "https://openrouter.ai/api/v1"),
        ] {
            let result = warm_prompt_cache(&model(api, base_url), &Context::default(), None, &builder)
                .await
                .expect("result");
            assert_eq!(result, WarmPromptCacheResult::Unsupported);
        }
    }

    #[test]
    fn sanitized_params_drop_streaming_and_thinking_fields() {
        let sanitized = sanitize_warm_prompt_cache_params(&json!({
            "model": "claude",
            "stream": true,
            "thinking": { "type": "enabled" },
            "output_config": { "effort": "high" },
            "tool_choice": { "type": "auto" },
            "max_tokens": 4096,
            "messages": []
        }));
        assert_eq!(sanitized.get("max_tokens"), Some(&Value::from(0)));
        assert_eq!(sanitized.get("model"), Some(&Value::from("claude")));
        assert!(!sanitized.contains_key("stream"));
        assert!(!sanitized.contains_key("thinking"));
        assert!(!sanitized.contains_key("output_config"));
        assert!(!sanitized.contains_key("tool_choice"));
    }

    #[test]
    fn merged_headers_let_options_override_model_headers() {
        let mut model = model("anthropic-messages", "https://api.anthropic.com");
        model.headers = Some([("anthropic-beta".to_owned(), "model".to_owned())].into_iter().collect());
        let mut options = StreamOptions::default();
        options.request.headers = Some(
            [("anthropic-beta".to_owned(), Some("options".to_owned()))].into_iter().collect(),
        );
        let headers = headers_for_warm_prompt_cache(&model, &options);
        assert_eq!(headers.get("anthropic-beta"), Some(&Some("options".to_owned())));
    }
}
