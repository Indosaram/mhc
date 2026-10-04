//! Port of senpi packages/ai/src/api/openrouter-images.ts.

use serde_json::{json, Map, Value};

use crate::types::{
    AssistantImages, ContentBlock, ImagesContext, ImagesFunction, ImagesModel, ImagesOptions,
    ImagesStopReason, ProviderHeaders, Usage, UsageCost,
};
use crate::utils::diagnostics::now_ms;
use crate::utils::headers::{headers_to_record, provider_headers_to_record};
use crate::utils::provider_retry::{
    retry_provider_request, ProviderErrorStatus, ProviderRequestError, ProviderRetryError, ProviderRetryOptions,
};
use crate::utils::sanitize_unicode::sanitize_surrogates;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OpenRouterImagesRequestError {
    pub message: String,
    pub status: Option<u16>,
}

impl ProviderRequestError for OpenRouterImagesRequestError {
    fn provider_status(&self) -> Option<ProviderErrorStatus<'_>> {
        Some(ProviderErrorStatus { status: self.status, headers: None })
    }
}

pub fn build_params(model: &ImagesModel, context: &ImagesContext) -> Map<String, Value> {
    let content: Vec<Value> = context
        .input
        .iter()
        .map(|item| match item {
            ContentBlock::Text(text) => json!({ "type": "text", "text": sanitize_surrogates(&text.text) }),
            ContentBlock::Image(image) => {
                json!({ "type": "image_url", "image_url": { "url": format!("data:{};base64,{}", image.mime_type, image.data) } })
            }
            other => serde_json::to_value(other).unwrap_or(Value::Null),
        })
        .collect();

    let mut params = Map::new();
    params.insert("model".into(), Value::from(model.id.clone()));
    params.insert("messages".into(), json!([{ "role": "user", "content": content }]));
    params.insert("stream".into(), Value::Bool(false));
    params.insert(
        "modalities".into(),
        if model.output.contains(&crate::types::ImagesOutputModality::Text) {
            json!(["image", "text"])
        } else {
            json!(["image"])
        },
    );
    params
}

/// `$/million tokens`; cache reads and writes stay separate counters.
pub fn parse_usage(raw_usage: &Value, model: &ImagesModel) -> Usage {
    let prompt_tokens = raw_usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
    let reported_cached_tokens = raw_usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write_tokens = raw_usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cache_write_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read_tokens = if cache_write_tokens > 0 {
        reported_cached_tokens.saturating_sub(cache_write_tokens)
    } else {
        reported_cached_tokens
    };
    let input = prompt_tokens.saturating_sub(cache_read_tokens + cache_write_tokens);
    let output = raw_usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0);
    let mut usage = Usage {
        input,
        output,
        cache_read: cache_read_tokens,
        cache_write: cache_write_tokens,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output + cache_read_tokens + cache_write_tokens,
        cost: UsageCost::default(),
    };
    usage.cost.input = (model.cost.input / 1_000_000.0) * input as f64;
    usage.cost.output = (model.cost.output / 1_000_000.0) * output as f64;
    usage.cost.cache_read = (model.cost.cache_read / 1_000_000.0) * cache_read_tokens as f64;
    usage.cost.cache_write = (model.cost.cache_write / 1_000_000.0) * cache_write_tokens as f64;
    usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    usage
}

pub async fn generate_images(
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<ImagesOptions>,
) -> AssistantImages {
    let options_ref = options.as_ref();
    let mut output = AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: Vec::new(),
        response_id: None,
        usage: None,
        background: None,
        stop_reason: ImagesStopReason::Stop,
        error_message: None,
        timestamp: now_ms(),
    };

    let result = run_generate(model, context, options_ref, &mut output).await;
    if let Err(error) = result {
        let aborted = options_ref
            .and_then(|options| options.request.signal.as_ref())
            .is_some_and(|signal| signal.aborted());
        output.stop_reason = if aborted { ImagesStopReason::Aborted } else { ImagesStopReason::Error };
        output.error_message = Some(error);
    }
    output
}

async fn run_generate(
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
    output: &mut AssistantImages,
) -> Result<(), String> {
    let Some(api_key) = options.and_then(|options| options.request.api_key.clone()).filter(|key| !key.is_empty()) else {
        return Err(format!("No API key for provider: {}", model.provider));
    };

    let payload_model = crate::api::openai_images::model_as_payload_model(model);
    let mut params = Value::Object(build_params(model, context));
    if let Some(options) = options
        && let Some(next) = options.request.apply_payload_hook(&params, &payload_model, None).await?
    {
        params = next;
    }

    let headers: ProviderHeaders = {
        let mut merged: ProviderHeaders = model
            .headers
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|(name, value)| (name, Some(value)))
            .collect();
        if let Some(overrides) = options.and_then(|options| options.request.headers.as_ref()) {
            for (name, value) in overrides {
                merged.insert(name.clone(), value.clone());
            }
        }
        merged
    };
    let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    let client = options.and_then(|options| options.request.fetch.clone()).unwrap_or_default();
    let mut request_headers = reqwest::header::HeaderMap::new();
    request_headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|error| error.to_string())?,
    );
    if let Some(extra) = provider_headers_to_record(Some(&headers)) {
        for (name, value) in extra {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                reqwest::header::HeaderValue::from_str(&value),
            ) {
                request_headers.insert(name, value);
            }
        }
    }

    let retry_options = ProviderRetryOptions {
        max_retries: options.and_then(|options| options.request.max_retries),
        max_retry_delay_ms: options.and_then(|options| options.request.max_retry_delay_ms),
        signal: options.and_then(|options| options.request.signal.clone()),
    };
    let signal = options.and_then(|options| options.request.signal.clone());
    let send = || {
        let client = client.clone();
        let url = url.clone();
        let request_headers = request_headers.clone();
        let params = params.clone();
        let signal = signal.clone();
        async move {
            if signal.as_ref().is_some_and(|signal| signal.aborted()) {
                return Err(OpenRouterImagesRequestError { message: "Request aborted".into(), status: None });
            }
            let request = client.post(&url).headers(request_headers).json(&params);
            let response = match &signal {
                Some(signal) => tokio::select! {
                    result = request.send() => result,
                    _ = signal.cancelled() => return Err(OpenRouterImagesRequestError { message: "Request aborted".into(), status: None }),
                },
                None => request.send().await,
            }
            .map_err(|error| OpenRouterImagesRequestError { message: error.to_string(), status: None })?;
            let status = response.status().as_u16();
            let response_headers = headers_to_record(response.headers());
            let text = response.text().await.unwrap_or_default();
            if !(200..300).contains(&status) {
                return Err(OpenRouterImagesRequestError { message: text, status: Some(status) });
            }
            let body = serde_json::from_str::<Value>(&text)
                .map_err(|error| OpenRouterImagesRequestError { message: error.to_string(), status: Some(status) })?;
            Ok((body, status, response_headers))
        }
    };

    let (body, raw_status, raw_headers) = retry_provider_request(send, &retry_options)
        .await
        .map_err(|error| match error {
            ProviderRetryError::Request(error) => error.message,
            ProviderRetryError::RetryDelay { message, .. } => message,
            ProviderRetryError::Aborted => "Request aborted".to_owned(),
        })?;

    if let Some(options) = options {
        options.request.apply_response_hook(
            &crate::types::ProviderResponse { status: raw_status, headers: raw_headers }, &payload_model,
        ).await?;
    }

    output.response_id = body.get("id").and_then(Value::as_str).map(str::to_owned);
    if let Some(usage) = body.get("usage") {
        output.usage = Some(parse_usage(usage, model));
    }

    if let Some(choice) = body.get("choices").and_then(Value::as_array).and_then(|choices| choices.first()) {
        let message = choice.get("message").cloned().unwrap_or(Value::Null);
        if let Some(content) = message.get("content").and_then(Value::as_str)
            && !content.is_empty()
        {
            output.output.push(ContentBlock::text(content));
        }
        for image in message.get("images").and_then(Value::as_array).cloned().unwrap_or_default() {
            let image_url = match image.get("image_url") {
                Some(Value::String(url)) => Some(url.clone()),
                Some(Value::Object(object)) => object.get("url").and_then(Value::as_str).map(str::to_owned),
                _ => None,
            };
            let Some(image_url) = image_url.filter(|url| url.starts_with("data:")) else { continue };
            let Some(rest) = image_url.strip_prefix("data:") else { continue };
            let Some((mime_type, data)) = rest.split_once(";base64,") else { continue };
            output.output.push(ContentBlock::Image(crate::types::ImageContent {
                data: data.to_owned(),
                mime_type: mime_type.to_owned(),
            }));
        }
    }

    Ok(())
}

pub fn images_function() -> ImagesFunction {
    std::sync::Arc::new(|model, context, options| {
        Box::pin(async move { generate_images(model, context, options).await })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ImageContent, ImagesModelCost, ImagesOutputModality, InputModality, TextContent};

    fn model() -> ImagesModel {
        ImagesModel {
            id: "black-forest-labs/flux.2-pro".into(),
            name: "black-forest-labs/flux.2-pro".into(),
            api: "openrouter-images".into(),
            provider: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            thinking_level_map: None,
            input: vec![InputModality::Text, InputModality::Image],
            output: vec![ImagesOutputModality::Image, ImagesOutputModality::Text],
            cost: ImagesModelCost {
                input: 2.0,
                output: 12.0,
                cache_read: 0.5,
                cache_write: 1.0,
                tiers: None,
                image_input: None,
            },
            sampling_params: None,
            headers: None,
            cache_retention: None,
            upstream_model_id: None,
            service_tier: None,
            recover_text_tool_calls: None,
        }
    }

    #[test]
    fn params_carry_the_prompt_and_data_urls_with_text_modality() {
        let context = ImagesContext {
            input: vec![
                ContentBlock::Text(TextContent { text: "draw".into(), ..TextContent::default() }),
                ContentBlock::Image(ImageContent { data: "AA==".into(), mime_type: "image/png".into() }),
            ],
        };
        let params = build_params(&model(), &context);
        assert_eq!(params.get("model"), Some(&Value::from("black-forest-labs/flux.2-pro")));
        assert_eq!(params.get("stream"), Some(&Value::Bool(false)));
        assert_eq!(params.get("modalities"), Some(&json!(["image", "text"])));
        let message = &params["messages"][0];
        assert_eq!(message["role"], json!("user"));
        assert_eq!(message["content"][0], json!({ "type": "text", "text": "draw" }));
        assert_eq!(
            message["content"][1],
            json!({ "type": "image_url", "image_url": { "url": "data:image/png;base64,AA==" } })
        );
    }

    #[test]
    fn params_omit_text_modality_when_the_model_has_no_text_output() {
        let mut model = model();
        model.output = vec![ImagesOutputModality::Image];
        let params = build_params(&model, &ImagesContext::default());
        assert_eq!(params.get("modalities"), Some(&json!(["image"])));
    }

    #[test]
    fn usage_keeps_cache_read_and_write_separate() {
        let usage = parse_usage(
            &json!({ "prompt_tokens": 100, "completion_tokens": 20, "prompt_tokens_details": { "cached_tokens": 30, "cache_write_tokens": 10 } }),
            &model(),
        );
        assert_eq!(usage.input, 70);
        assert_eq!(usage.output, 20);
        assert_eq!(usage.cache_read, 20);
        assert_eq!(usage.cache_write, 10);
        assert_eq!(usage.total_tokens, 120);
        assert!((usage.cost.cache_write - 1.0 * 10.0 / 1_000_000.0).abs() < 1e-12);
        assert!((usage.cost.total - (usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write)).abs() < 1e-12);
    }

    #[test]
    fn usage_without_a_breakdown_treats_cached_tokens_as_reads() {
        let usage = parse_usage(&json!({ "prompt_tokens": 100, "completion_tokens": 0 }), &model());
        assert_eq!(usage.input, 100);
        assert_eq!(usage.cache_read, 0);
        assert_eq!(usage.cache_write, 0);
    }
}

