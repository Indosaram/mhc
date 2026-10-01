//! Port of senpi packages/ai/src/api/openai-images.ts.
//!
//! The TS module drives the `openai` npm SDK; the same wire contract is issued here through
//! `reqwest` (JSON for generations, multipart for edits).

use serde_json::{json, Map, Value};

use crate::api::openai_client_auth::resolve_openai_client_auth;
use crate::api::openai_images_edit::{build_edit_params, upload_bytes, upload_name};
use crate::api::openai_images_params::{build_params, is_image_params, option_str};
use crate::api::openai_images_result::{requested_output_format, resolve_image};
use crate::types::{
    AssistantImages, ContentBlock, ImagesBackground, ImagesContext, ImagesFunction, ImagesModel, ImagesOptions,
    ImagesStopReason, ProviderHeaders, Usage, UsageCost,
};
use crate::utils::diagnostics::now_ms;
use crate::utils::headers::headers_to_record;
use crate::utils::provider_retry::{retry_provider_request, ProviderErrorStatus, ProviderRequestError, ProviderRetryOptions, ProviderRetryError};

pub use crate::api::openai_images_params::{
    parse_openai_image_output_options as parse_openai_image_output_options_public,
    parse_openai_image_size as parse_openai_image_size_public, OPENAI_IMAGE_BACKGROUNDS, OPENAI_IMAGE_MODERATIONS,
    OPENAI_IMAGE_OUTPUT_FORMATS, OPENAI_IMAGE_QUALITIES,
};

const ENDPOINT_SUFFIXES: &[&str] = &["/chat/completions", "/responses", "/models"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OpenAiImagesRequestError {
    pub message: String,
    pub status: Option<u16>,
}

impl ProviderRequestError for OpenAiImagesRequestError {
    fn provider_status(&self) -> Option<ProviderErrorStatus<'_>> {
        Some(ProviderErrorStatus { status: self.status, headers: None })
    }
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
    let mut params = build_params(model, context, options)?;
    let images: Vec<crate::types::ImageContent> = context
        .input
        .iter()
        .filter_map(|item| match item {
            ContentBlock::Image(image) => Some(image.clone()),
            _ => None,
        })
        .collect();
    let method = if images.is_empty() { "generate" } else { "edit" };
    if method == "edit" {
        params = build_edit_params(&params, &images, crate::api::openai_images_params::option_mask(options).as_ref())?;
    }
    let payload_model = model_as_payload_model(model);
    let mut params_value = Value::Object(params.clone());
    if let Some(on_payload) = options.and_then(|options| options.request.on_payload.as_ref())
        && let Some(next) = on_payload(&params_value, &payload_model, None)
    {
        if !is_image_params(&next) {
            return Err("onPayload returned an invalid image generation payload".into());
        }
        params_value = next;
        params = params_value.as_object().cloned().unwrap_or_default();
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
    let auth = resolve_openai_client_auth(
        &model.provider,
        options.and_then(|options| options.request.api_key.as_deref()),
        Some(&headers),
    )?;
    let base_url = normalize_base_url(&model.base_url)?;
    let url = format!("{base_url}/images/{}", if method == "edit" { "edits" } else { "generations" });

    let client = options.and_then(|options| options.request.fetch.clone()).unwrap_or_default();
    let mut request_headers = reqwest::header::HeaderMap::new();
    request_headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", auth.api_key))
            .map_err(|error| error.to_string())?,
    );
    if let Some(default_headers) = &auth.headers {
        for (name, value) in default_headers {
            let (Ok(name), Some(value)) = (
                reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                value.as_ref(),
            ) else {
                continue;
            };
            if let Ok(value) = reqwest::header::HeaderValue::from_str(value) {
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
        let images = images.clone();
        let mask = crate::api::openai_images_params::option_mask(options);
        let method = method.to_owned();
        let signal = signal.clone();
        async move {
            if signal.as_ref().is_some_and(|signal| signal.aborted()) {
                return Err(OpenAiImagesRequestError { message: "Request aborted".into(), status: None });
            }
            send_request(&client, &url, request_headers, &params, &images, mask.as_ref(), &method, signal.as_ref()).await
        }
    };

    let (body, raw_status, raw_headers) = retry_provider_request(send, &retry_options)
        .await
        .map_err(|error| match error {
            ProviderRetryError::Request(error) => error.message,
            ProviderRetryError::RetryDelay { message, .. } => message,
            ProviderRetryError::Aborted => "Request aborted".to_owned(),
        })?;

    if let Some(on_response) = options.and_then(|options| options.request.on_response.as_ref()) {
        on_response(&crate::types::ProviderResponse { status: raw_status, headers: raw_headers }, &payload_model);
    }

    if let Some(usage) = body.get("usage") {
        output.usage = Some(parse_usage(usage, model));
    }
    match body.get("background").and_then(Value::as_str) {
        Some("transparent") => output.background = Some(ImagesBackground::Transparent),
        Some("opaque") => output.background = Some(ImagesBackground::Opaque),
        _ => {}
    }
    let data = body.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
    if data.is_empty() {
        return Err("[OI] images response contained no image data".into());
    }

    let output_format = requested_output_format(option_str(options, "outputFormat").as_deref());
    for datum in &data {
        let image = resolve_image(datum, &output_format, options).await?;
        if let Some(prompt) = datum.get("revised_prompt").and_then(Value::as_str)
            && !prompt.trim().is_empty()
        {
            output.output.push(ContentBlock::text(prompt));
        }
        output.output.push(ContentBlock::Image(image));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn send_request(
    client: &reqwest::Client,
    url: &str,
    headers: reqwest::header::HeaderMap,
    params: &Map<String, Value>,
    images: &[crate::types::ImageContent],
    mask: Option<&crate::types::ImageContent>,
    method: &str,
    signal: Option<&crate::utils::abort::AbortSignal>,
) -> Result<(Value, u16, std::collections::BTreeMap<String, String>), OpenAiImagesRequestError> {
    let request = if method == "edit" {
        let mut fields: Vec<MultipartField> = Vec::new();
        for (key, value) in params {
            if key == "image" || key == "mask" {
                continue;
            }
            fields.push(MultipartField::text(key, json_scalar(value)));
        }
        for (index, image) in images.iter().enumerate() {
            let bytes = upload_bytes(image).map_err(|message| OpenAiImagesRequestError { message, status: None })?;
            fields.push(MultipartField::file("image[]", &upload_name(image, &format!("reference-{index}")), &image.mime_type, bytes));
        }
        if let Some(mask) = mask {
            let bytes = upload_bytes(mask).map_err(|message| OpenAiImagesRequestError { message, status: None })?;
            fields.push(MultipartField::file("mask", &upload_name(mask, "mask"), &mask.mime_type, bytes));
        }
        let (body, content_type) = encode_multipart(&fields);
        let mut headers = headers;
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_str(&content_type)
                .map_err(|error| OpenAiImagesRequestError { message: error.to_string(), status: None })?,
        );
        client.post(url).headers(headers).body(body)
    } else {
        client.post(url).headers(headers).json(params)
    };

    let response = match signal {
        Some(signal) => tokio::select! {
            result = request.send() => result,
            _ = signal.cancelled() => return Err(OpenAiImagesRequestError { message: "Request aborted".into(), status: None }),
        },
        None => request.send().await,
    }
    .map_err(|error| OpenAiImagesRequestError { message: error.to_string(), status: None })?;
    let status = response.status().as_u16();
    let response_headers = headers_to_record(response.headers());
    let text = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        let message = error_message_from_body(&text);
        return Err(OpenAiImagesRequestError { message, status: Some(status) });
    }
    let body = serde_json::from_str::<Value>(&text)
        .map_err(|error| OpenAiImagesRequestError { message: error.to_string(), status: Some(status) })?;
    Ok((body, status, response_headers))
}

const MULTIPART_BOUNDARY: &str = "maho-ai-openai-images-boundary";

struct MultipartField {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    value: Vec<u8>,
}

impl MultipartField {
    fn text(name: &str, value: String) -> Self {
        Self { name: name.to_owned(), filename: None, content_type: None, value: value.into_bytes() }
    }

    fn file(name: &str, filename: &str, content_type: &str, value: Vec<u8>) -> Self {
        Self {
            name: name.to_owned(),
            filename: Some(filename.to_owned()),
            content_type: Some(content_type.to_owned()),
            value,
        }
    }
}

fn encode_multipart(fields: &[MultipartField]) -> (Vec<u8>, String) {
    let mut body: Vec<u8> = Vec::new();
    for field in fields {
        body.extend_from_slice(format!("--{MULTIPART_BOUNDARY}\r\n").as_bytes());
        match &field.filename {
            Some(filename) => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"",
                        field.name
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(format!("; filename=\"{filename}\"\r\n").as_bytes());
                if let Some(content_type) = &field.content_type {
                    body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
                }
            }
            None => body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{}\"\r\n", field.name).as_bytes()),
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(&field.value);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{MULTIPART_BOUNDARY}--\r\n").as_bytes());
    (body, format!("multipart/form-data; boundary={MULTIPART_BOUNDARY}"))
}

pub fn model_as_payload_model(model: &ImagesModel) -> crate::types::Model {
    crate::types::Model {
        id: model.id.clone(),
        name: model.name.clone(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        base_url: model.base_url.clone(),
        reasoning: false,
        thinking_level_map: None,
        input: model.input.clone(),
        cost: crate::types::ModelCost {
            input: model.cost.input,
            output: model.cost.output,
            cache_read: model.cost.cache_read,
            cache_write: model.cost.cache_write,
            tiers: model.cost.tiers.clone(),
        },
        context_window: 0,
        max_tokens: 0,
        sampling_params: None,
        headers: model.headers.clone(),
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
    }
}

fn json_scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn error_message_from_body(body: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<Value>(body) {
        if let Some(message) = parsed.get("error").and_then(|error| error.get("message")).and_then(Value::as_str) {
            return message.to_owned();
        }
        if let Some(message) = parsed.get("message").and_then(Value::as_str) {
            return message.to_owned();
        }
    }
    body.to_owned()
}

/// `$/million tokens`, with image input tokens carrying their own rate.
pub fn parse_usage(raw_usage: &Value, model: &ImagesModel) -> Usage {
    let input = raw_usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let output = raw_usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
    let image_tokens = raw_usage
        .get("input_tokens_details")
        .and_then(|details| details.get("image_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let text_tokens = raw_usage
        .get("input_tokens_details")
        .and_then(|details| details.get("text_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| input.saturating_sub(image_tokens));
    let image_rate = model.cost.image_input.unwrap_or(model.cost.input);
    let mut usage = Usage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: raw_usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(input + output),
        cost: UsageCost::default(),
    };
    usage.cost.input = (model.cost.input * text_tokens as f64 + image_rate * image_tokens as f64) / 1_000_000.0;
    usage.cost.output = (model.cost.output / 1_000_000.0) * output as f64;
    usage.cost.cache_read = 0.0;
    usage.cost.cache_write = 0.0;
    usage.cost.total = usage.cost.input + usage.cost.output;
    usage
}

pub fn normalize_base_url(base_url: &str) -> Result<String, String> {
    let mut url = url::Url::parse(base_url).map_err(|_| format!("Invalid [OI] images base URL: {base_url}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("[OI] images base URL must use HTTP or HTTPS".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("[OI] images base URL must not include a query or fragment".into());
    }
    let pathname = url.path().trim_end_matches('/').to_owned();
    let normalized_pathname = pathname.to_lowercase();
    if normalized_pathname.contains("/images") {
        return Err("[OI] images base URL must not include an images endpoint".into());
    }
    if ENDPOINT_SUFFIXES.iter().any(|suffix| normalized_pathname.ends_with(suffix)) {
        return Err("[OI] images base URL must not include a known API endpoint".into());
    }
    let pathname = if pathname.ends_with("/v1") { pathname } else { format!("{pathname}/v1") };
    url.set_path(&pathname);
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

pub fn images_function() -> ImagesFunction {
    std::sync::Arc::new(|model, context, options| {
        Box::pin(async move { generate_images(model, context, options).await })
    })
}

pub fn params_json(params: &Map<String, Value>) -> Value {
    json!(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ImagesModelCost, InputModality, ImagesOutputModality};

    fn model(image_input: Option<f64>) -> ImagesModel {
        ImagesModel {
            id: "gpt-image-2.5".into(),
            name: "gpt-image-2.5".into(),
            api: "openai-images".into(),
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            thinking_level_map: None,
            input: vec![InputModality::Text, InputModality::Image],
            output: vec![ImagesOutputModality::Image],
            cost: ImagesModelCost {
                input: 5.0,
                output: 40.0,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
                image_input,
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
    fn base_urls_are_normalized_and_validated() {
        assert_eq!(normalize_base_url("https://api.openai.com/v1"), Ok("https://api.openai.com/v1".into()));
        assert_eq!(normalize_base_url("https://api.openai.com"), Ok("https://api.openai.com/v1".into()));
        assert_eq!(
            normalize_base_url("https://api.openai.com/v1/images"),
            Err("[OI] images base URL must not include an images endpoint".into())
        );
        assert_eq!(
            normalize_base_url("https://api.openai.com/v1/chat/completions"),
            Err("[OI] images base URL must not include a known API endpoint".into())
        );
        assert_eq!(
            normalize_base_url("https://api.openai.com/v1?a=1"),
            Err("[OI] images base URL must not include a query or fragment".into())
        );
        assert_eq!(
            normalize_base_url("ftp://api.openai.com"),
            Err("[OI] images base URL must use HTTP or HTTPS".into())
        );
        assert_eq!(
            normalize_base_url("not a url"),
            Err("Invalid [OI] images base URL: not a url".into())
        );
    }

    #[test]
    fn usage_prices_image_tokens_at_the_image_rate() {
        let usage = parse_usage(
            &json!({ "input_tokens": 1000, "output_tokens": 500, "total_tokens": 1500, "input_tokens_details": { "image_tokens": 400, "text_tokens": 600 } }),
            &model(Some(25.0)),
        );
        assert_eq!(usage.input, 1000);
        assert_eq!(usage.output, 500);
        assert_eq!(usage.total_tokens, 1500);
        assert!((usage.cost.input - (5.0 * 600.0 + 25.0 * 400.0) / 1_000_000.0).abs() < 1e-12);
        assert!((usage.cost.output - 40.0 * 500.0 / 1_000_000.0).abs() < 1e-12);
        assert!((usage.cost.total - (usage.cost.input + usage.cost.output)).abs() < 1e-12);
    }

    #[test]
    fn usage_falls_back_to_the_text_rate_without_an_image_rate() {
        let usage = parse_usage(
            &json!({ "input_tokens": 1000, "output_tokens": 0, "input_tokens_details": { "image_tokens": 400 } }),
            &model(None),
        );
        assert!((usage.cost.input - 5.0 * 1000.0 / 1_000_000.0).abs() < 1e-12);
        assert_eq!(usage.total_tokens, 1000);
    }

    #[test]
    fn error_bodies_prefer_the_nested_message() {
        assert_eq!(
            error_message_from_body("{\"error\":{\"message\":\"nope\"}}"),
            "nope"
        );
        assert_eq!(error_message_from_body("{\"message\":\"plain\"}"), "plain");
        assert_eq!(error_message_from_body("not json"), "not json");
    }
}
