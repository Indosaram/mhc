//! Port of senpi packages/ai/src/api/warm-prompt-cache.ts.
//!
//! The TS module builds its request body with `buildAnthropicWarmPromptCacheParams` from
//! anthropic-messages.ts (node 11-anthropic). That builder is injected here so this module keeps
//! the TS control flow without duplicating the Anthropic converter; the default builder is wired
//! once 11-anthropic lands.

use serde_json::{Map, Value, json};

use crate::types::{
    Context, Model, ProviderHeaders, SimpleStreamOptions, StreamOptions, UsageCost,
};

#[path = "prompt_cache_warmers.rs"]
pub mod prompt_cache_warmers;
use crate::utils::headers::provider_headers_to_record;
use crate::utils::prompt_cache_ttl::is_anthropic_api_base_url;

#[derive(Debug, Clone, PartialEq)]
pub struct WarmPromptCacheUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: Option<UsageCost>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WarmPromptCacheResult {
    Unsupported,
    Supported {
        usage: WarmPromptCacheUsage,
        usage_raw: Value,
    },
}

pub type WarmPromptCacheOptions = SimpleStreamOptions;
pub type BuildWarmPromptCacheParams<'a> =
    &'a (dyn Fn(&Model, &Context, &StreamOptions) -> Result<Value, String> + Send + Sync);

pub async fn warm_prompt_cache(
    model: &Model,
    context: &Context,
    options: Option<StreamOptions>,
    build_params: BuildWarmPromptCacheParams<'_>,
) -> Result<WarmPromptCacheResult, String> {
    warm_prompt_cache_simple(
        model,
        context,
        SimpleStreamOptions {
            stream: options.unwrap_or_default(),
            ..SimpleStreamOptions::default()
        },
        build_params,
    )
    .await
}

pub async fn warm_prompt_cache_simple(
    model: &Model,
    context: &Context,
    options: WarmPromptCacheOptions,
    build_params: BuildWarmPromptCacheParams<'_>,
) -> Result<WarmPromptCacheResult, String> {
    if crate::api::openai_responses_prompt_cache::is_openai_responses_prompt_cache_model(model) {
        if options.stream.cache_retention.or(model.cache_retention)
            == Some(crate::types::CacheRetention::None)
        {
            return Ok(WarmPromptCacheResult::Unsupported);
        }
        let Some(warmer) = prompt_cache_warmers::get_prompt_cache_warmer(&model.api) else {
            return Ok(WarmPromptCacheResult::Unsupported);
        };
        let (usage, usage_raw) = warmer(model.clone(), context.clone(), options).await?;
        return Ok(WarmPromptCacheResult::Supported {
            usage: WarmPromptCacheUsage {
                input: usage.input,
                output: usage.output,
                cache_read: usage.cache_read,
                cache_write: usage.cache_write,
                cost: Some(usage.cost),
            },
            usage_raw,
        });
    }
    if model.api != "anthropic-messages" || !is_anthropic_api_base_url(&model.base_url) {
        return Ok(WarmPromptCacheResult::Unsupported);
    }

    let options = options.stream;
    let headers = provider_headers_to_record(options.request.headers.as_ref());
    let mut params = build_params(model, context, &options)?;
    if let Some(transformed) = options
        .request
        .apply_payload_hook(
            &params,
            model,
            Some(&crate::types::ProviderRequestMetadata {
                model: model.clone(),
                headers: options.request.headers.clone().unwrap_or_default(),
            }),
        )
        .await?
    {
        params = transformed;
    }

    let sanitized = sanitize_warm_prompt_cache_params(&params);

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

    request_headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    let operation = async {
        let response = crate::utils::gzip_request_body::send_with_gzip_request_body(
            &client,
            &url,
            request_headers,
            json!(sanitized).to_string(),
            crate::utils::prompt_cache_ttl::get_anthropic_compat(model).supports_gzip_request_body,
        )
        .await
        .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.map_err(|error| error.to_string())?;
            return Err(format!("{status}: {body}"));
        }
        response
            .json::<Value>()
            .await
            .map_err(|error| error.to_string())
    };
    let timeout_ms = options.request.timeout_ms.unwrap_or(600_000);
    let timed = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), operation);
    let body = if let Some(signal) = &options.request.signal {
        crate::utils::abort::race_with_abort_signal(timed, signal)
            .await
            .map_err(|error| error.to_string())?
    } else {
        timed.await
    }
    .map_err(|_| format!("Request timed out after {timeout_ms}ms"))??;
    let usage_raw = body.get("usage").cloned().unwrap_or(Value::Null);
    let usage = WarmPromptCacheUsage {
        input: usage_raw
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: usage_raw
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read: usage_raw
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write: usage_raw
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cost: None,
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
        let mut model = crate::models_generated::get_builtin_provider_models("anthropic")
            .expect("models")[0]
            .clone();
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
            let result =
                warm_prompt_cache(&model(api, base_url), &Context::default(), None, &builder)
                    .await
                    .expect("result");
            assert_eq!(result, WarmPromptCacheResult::Unsupported);
        }
    }

    fn warm_context() -> Context {
        serde_json::from_value(json!({
            "systemPrompt": "You are concise.",
            "messages": [{ "role": "user", "content": "Hello", "timestamp": 1 }],
            "tools": [{
                "name": "lookup",
                "description": "Look something up",
                "parameters": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }
            }]
        }))
        .expect("context")
    }

    #[tokio::test]
    async fn sends_a_native_anthropic_max_tokens_0_request_and_parses_usage() {
        // Given: a local server standing in for api.anthropic.com (the client resolves the
        // native host to it, so the native-host gate stays real).
        let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let recorded = bodies.clone();
        let app = axum::Router::new().route(
            "/v1/messages",
            axum::routing::post(move |body: String| {
                let recorded = recorded.clone();
                async move {
                    recorded
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(serde_json::from_str(&body).unwrap_or(Value::Null));
                    axum::Json(json!({
                        "id": "msg_warm", "type": "message", "role": "assistant", "content": [],
                        "model": "claude-sonnet-4-5", "stop_reason": "max_tokens", "stop_sequence": null,
                        "usage": {
                            "input_tokens": 120, "output_tokens": 0,
                            "cache_read_input_tokens": 90, "cache_creation_input_tokens": 30
                        }
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let client = reqwest::Client::builder()
            .no_proxy()
            .resolve("api.anthropic.com", address)
            .build()
            .expect("client");
        let mut model =
            crate::models_generated::get_builtin_model("anthropic", "claude-sonnet-4-5")
                .expect("model")
                .clone();
        model.base_url = "http://api.anthropic.com".into();
        let mut options = StreamOptions::default();
        options.request.api_key = Some("test-key".into());
        options.request.fetch = Some(client);
        let builder = |model: &Model, context: &Context, options: &StreamOptions| {
            Ok(Value::Object(
                crate::api::anthropic_messages::build_anthropic_warm_prompt_cache_params(
                    model,
                    context,
                    Some(options),
                ),
            ))
        };

        // When
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            warm_prompt_cache(&model, &warm_context(), Some(options), &builder),
        )
        .await
        .expect("bounded wait")
        .expect("warm result");
        server.abort();

        // Then
        let bodies = bodies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(bodies.len(), 1);
        let body = &bodies[0];
        assert_eq!(body["max_tokens"], json!(0));
        assert_eq!(body["model"], json!("claude-sonnet-4-5"));
        assert!(body.get("stream").is_none());
        assert!(body.get("thinking").is_none());
        assert!(body.get("tool_choice").is_none());
        assert!(body.to_string().contains("\"cache_control\""));
        assert_eq!(
            result,
            WarmPromptCacheResult::Supported {
                usage: WarmPromptCacheUsage {
                    input: 120,
                    output: 0,
                    cache_read: 90,
                    cache_write: 30,
                    cost: None,
                },
                usage_raw: json!({
                    "input_tokens": 120, "output_tokens": 0,
                    "cache_read_input_tokens": 90, "cache_creation_input_tokens": 30
                }),
            }
        );
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
        model.headers = Some(
            [("anthropic-beta".to_owned(), "model".to_owned())]
                .into_iter()
                .collect(),
        );
        let mut options = StreamOptions::default();
        options.request.headers = Some(
            [("anthropic-beta".to_owned(), Some("options".to_owned()))]
                .into_iter()
                .collect(),
        );
        let headers = headers_for_warm_prompt_cache(&model, &options);
        assert_eq!(
            headers.get("anthropic-beta"),
            Some(&Some("options".to_owned()))
        );
    }

    #[tokio::test]
    async fn explicit_warmer_options_respect_disabled_model_cache_without_building() {
        // Given
        let mut model = model("openai-responses", "https://api.openai.com/v1");
        model.compat = Some(crate::model::ModelCompat(
            serde_json::from_value(json!({"supportsExplicitPromptCacheMode":true}))
                .expect("compat"),
        ));
        model.cache_retention = Some(crate::types::CacheRetention::None);
        let options = WarmPromptCacheOptions {
            reasoning: Some(crate::types::ThinkingLevel::High),
            service_tier: Some(crate::types::ServiceTierPreference::Priority),
            ..Default::default()
        };

        // When
        let result = warm_prompt_cache_simple(&model, &Context::default(), options, &|_, _, _| {
            panic!("disabled cache must not build a request")
        })
        .await
        .expect("unsupported cache");

        // Then
        assert_eq!(result, WarmPromptCacheResult::Unsupported);
    }
}
