//! Port of senpi packages/ai/src/api/openai-images-params.ts.

use serde_json::{json, Map, Value};

use crate::types::{ImagesContext, ImagesModel, ImagesOptions};

const MAX_PROMPT_CHARS: usize = 32_000;

pub const OPENAI_IMAGE_QUALITIES: &[&str] = &["auto", "low", "medium", "high", "xhigh", "max"];
pub const OPENAI_IMAGE_BACKGROUNDS: &[&str] = &["auto", "transparent", "opaque"];
pub const OPENAI_IMAGE_OUTPUT_FORMATS: &[&str] = &["png", "jpeg", "webp"];
pub const OPENAI_IMAGE_MODERATIONS: &[&str] = &["auto", "low"];

pub fn option_str(options: Option<&ImagesOptions>, key: &str) -> Option<String> {
    options?.extra.get(key).and_then(Value::as_str).map(str::to_owned)
}

pub fn option_u64(options: Option<&ImagesOptions>, key: &str) -> Option<u64> {
    options?.extra.get(key).and_then(Value::as_u64)
}

pub fn option_mask(options: Option<&ImagesOptions>) -> Option<crate::types::ImageContent> {
    options?
        .extra
        .get("mask")
        .and_then(|value| serde_json::from_value::<crate::types::ImageContent>(value.clone()).ok())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedImageSize {
    pub ok: bool,
    pub size: String,
    pub error: Option<String>,
}

impl ParsedImageSize {
    fn ok(size: impl Into<String>) -> Self {
        Self { ok: true, size: size.into(), error: None }
    }

    fn err(error: impl Into<String>) -> Self {
        Self { ok: false, size: String::new(), error: Some(error.into()) }
    }
}

pub fn parse_openai_image_size(size: &str) -> ParsedImageSize {
    if ["auto", "1024x1024", "1536x1024", "1024x1536"].contains(&size) {
        return ParsedImageSize::ok(size);
    }
    let Some((width, height)) = parse_dimensions(size) else {
        return ParsedImageSize::err("OpenAI image size must be auto or WIDTHxHEIGHT with integer dimensions");
    };
    if width > 3840 || height > 3840 {
        return ParsedImageSize::err("OpenAI image size edges must be at most 3840 pixels");
    }
    if width % 16 != 0 || height % 16 != 0 {
        return ParsedImageSize::err("OpenAI image size width and height must be divisible by 16");
    }
    if width > height * 3 || height > width * 3 {
        return ParsedImageSize::err("OpenAI image size aspect ratio must be between 1:3 and 3:1 inclusive");
    }
    let pixels = width * height;
    if !(655_360..=8_294_400).contains(&pixels) {
        return ParsedImageSize::err(
            "OpenAI image size must contain between 655360 and 8294400 pixels inclusive",
        );
    }
    ParsedImageSize::ok(size)
}

fn parse_dimensions(size: &str) -> Option<(u64, u64)> {
    let (width, height) = size.split_once('x')?;
    if width.is_empty() || height.is_empty() {
        return None;
    }
    if !width.bytes().all(|byte| byte.is_ascii_digit()) || !height.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((width.parse().ok()?, height.parse().ok()?))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedImageOutputOptions {
    pub ok: bool,
    pub output_format: String,
    pub error: Option<String>,
}

pub fn parse_openai_image_output_options(
    background: Option<&str>,
    output_format: Option<&str>,
    output_compression: Option<u64>,
    has_mask: bool,
    image_count: usize,
) -> ParsedImageOutputOptions {
    let output_format = output_format.unwrap_or("png").to_owned();
    if background == Some("transparent") && output_format == "jpeg" {
        return ParsedImageOutputOptions {
            ok: false,
            output_format,
            error: Some("OpenAI image background transparent requires output_format png or webp".into()),
        };
    }
    if let Some(compression) = output_compression {
        if output_format == "png" {
            return ParsedImageOutputOptions {
                ok: false,
                output_format,
                error: Some("OpenAI image output_compression requires output_format jpeg or webp".into()),
            };
        }
        if compression > 100 {
            return ParsedImageOutputOptions {
                ok: false,
                output_format,
                error: Some("OpenAI image output_compression must be an integer between 0 and 100".into()),
            };
        }
    }
    if has_mask && image_count == 0 {
        return ParsedImageOutputOptions {
            ok: false,
            output_format,
            error: Some("OpenAI image mask requires at least one input image".into()),
        };
    }
    ParsedImageOutputOptions { ok: true, output_format, error: None }
}

pub fn build_params(
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
) -> Result<Map<String, Value>, String> {
    let mut prompt_parts: Vec<String> = Vec::new();
    for item in &context.input {
        if let crate::types::ContentBlock::Text(text) = item {
            let text = crate::utils::sanitize_unicode::sanitize_surrogates(&text.text);
            if !text.trim().is_empty() {
                prompt_parts.push(text);
            }
        }
    }
    let prompt = prompt_parts.join("\n\n");
    if prompt.trim().is_empty() {
        return Err("Image generation requires a non-empty text prompt".into());
    }
    if prompt.len() > MAX_PROMPT_CHARS {
        return Err(format!("Image generation prompt exceeds {MAX_PROMPT_CHARS} characters"));
    }
    let size = option_str(options, "size").unwrap_or_else(|| "auto".to_owned());
    let parsed_size = parse_openai_image_size(&size);
    if !parsed_size.ok {
        return Err(parsed_size.error.unwrap_or_default());
    }
    let image_count = context
        .input
        .iter()
        .filter(|item| matches!(item, crate::types::ContentBlock::Image(_)))
        .count();
    let background = option_str(options, "background");
    let output_format = option_str(options, "outputFormat");
    let output_compression = option_u64(options, "outputCompression");
    let output = parse_openai_image_output_options(
        background.as_deref(),
        output_format.as_deref(),
        output_compression,
        option_mask(options).is_some(),
        image_count,
    );
    if !output.ok {
        return Err(output.error.unwrap_or_default());
    }

    let mut params = Map::new();
    params.insert("model".into(), Value::from(model.id.clone()));
    params.insert("prompt".into(), Value::from(prompt));
    params.insert("size".into(), Value::from(size));
    params.insert("quality".into(), Value::from(option_str(options, "quality").unwrap_or_else(|| "auto".to_owned())));
    params.insert("n".into(), Value::from(option_u64(options, "n").unwrap_or(1)));
    params.insert("output_format".into(), Value::from(output.output_format));
    params.insert("stream".into(), Value::Bool(false));
    if let Some(background) = background {
        params.insert("background".into(), Value::from(background));
    }
    if let Some(compression) = output_compression {
        params.insert("output_compression".into(), Value::from(compression));
    }
    if let Some(moderation) = option_str(options, "moderation") {
        params.insert("moderation".into(), Value::from(moderation));
    }
    Ok(params)
}

pub fn is_image_params(value: &Value) -> bool {
    value.get("prompt").and_then(Value::as_str).is_some()
}

pub fn images_options_json(options: Option<&ImagesOptions>) -> Value {
    json!(options.map(|options| options.extra.clone()).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentBlock, ImagesModelCost, TextContent};

    fn model() -> ImagesModel {
        ImagesModel {
            id: "gpt-image-2.5".into(),
            name: "gpt-image-2.5".into(),
            api: "openai-images".into(),
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            thinking_level_map: None,
            input: vec![crate::types::InputModality::Text, crate::types::InputModality::Image],
            output: vec![crate::types::ImagesOutputModality::Image],
            cost: ImagesModelCost::default(),
            sampling_params: None,
            headers: None,
            cache_retention: None,
            upstream_model_id: None,
            service_tier: None,
            recover_text_tool_calls: None,
        }
    }

    fn context_with_texts(texts: &[&str]) -> ImagesContext {
        ImagesContext {
            input: texts
                .iter()
                .map(|text| ContentBlock::Text(TextContent { text: (*text).to_owned(), ..TextContent::default() }))
                .collect(),
        }
    }

    #[test]
    fn size_parsing_matches_every_ts_branch() {
        assert_eq!(parse_openai_image_size("auto"), ParsedImageSize::ok("auto"));
        assert_eq!(parse_openai_image_size("1024x1024"), ParsedImageSize::ok("1024x1024"));
        assert_eq!(
            parse_openai_image_size("1024x1").error.as_deref(),
            Some("OpenAI image size width and height must be divisible by 16")
        );
        assert_eq!(
            parse_openai_image_size("4096x4096").error.as_deref(),
            Some("OpenAI image size edges must be at most 3840 pixels")
        );
        assert_eq!(
            parse_openai_image_size("3840x1024").error.as_deref(),
            Some("OpenAI image size aspect ratio must be between 1:3 and 3:1 inclusive")
        );
        assert_eq!(
            parse_openai_image_size("512x512").error.as_deref(),
            Some("OpenAI image size must contain between 655360 and 8294400 pixels inclusive")
        );
        assert_eq!(
            parse_openai_image_size("1024").error.as_deref(),
            Some("OpenAI image size must be auto or WIDTHxHEIGHT with integer dimensions")
        );
        assert_eq!(parse_openai_image_size("1280x1024"), ParsedImageSize::ok("1280x1024"));
    }

    #[test]
    fn output_options_validate_before_any_request() {
        assert_eq!(
            parse_openai_image_output_options(Some("transparent"), Some("jpeg"), None, false, 1).error.as_deref(),
            Some("OpenAI image background transparent requires output_format png or webp")
        );
        assert_eq!(
            parse_openai_image_output_options(None, Some("png"), Some(50), false, 1).error.as_deref(),
            Some("OpenAI image output_compression requires output_format jpeg or webp")
        );
        assert_eq!(
            parse_openai_image_output_options(None, Some("webp"), Some(101), false, 1).error.as_deref(),
            Some("OpenAI image output_compression must be an integer between 0 and 100")
        );
        assert_eq!(
            parse_openai_image_output_options(None, None, None, true, 0).error.as_deref(),
            Some("OpenAI image mask requires at least one input image")
        );
        assert_eq!(
            parse_openai_image_output_options(Some("opaque"), Some("webp"), Some(80), false, 0),
            ParsedImageOutputOptions { ok: true, output_format: "webp".into(), error: None }
        );
    }

    #[test]
    fn default_body_carries_only_the_required_fields() {
        let params = build_params(&model(), &context_with_texts(&["draw a cat"]), None).expect("params");
        assert_eq!(
            Value::Object(params),
            json!({
                "model": "gpt-image-2.5",
                "prompt": "draw a cat",
                "size": "auto",
                "quality": "auto",
                "n": 1,
                "output_format": "png",
                "stream": false
            })
        );
    }

    #[test]
    fn text_blocks_are_sanitized_joined_and_required() {
        let params = build_params(&model(), &context_with_texts(&["a", " ", "b"]), None).expect("params");
        assert_eq!(params.get("prompt"), Some(&Value::from("a\n\nb")));
        assert_eq!(
            build_params(&model(), &context_with_texts(&["  "]), None),
            Err("Image generation requires a non-empty text prompt".into())
        );
        let long = "x".repeat(MAX_PROMPT_CHARS + 1);
        assert_eq!(
            build_params(&model(), &context_with_texts(&[&long]), None),
            Err("Image generation prompt exceeds 32000 characters".into())
        );
    }

    #[test]
    fn optional_fields_are_forwarded_exactly_as_passed() {
        let mut options = ImagesOptions::default();
        options.extra.insert("size".into(), json!("1024x1024"));
        options.extra.insert("quality".into(), json!("high"));
        options.extra.insert("n".into(), json!(2));
        options.extra.insert("background".into(), json!("opaque"));
        options.extra.insert("outputFormat".into(), json!("webp"));
        options.extra.insert("outputCompression".into(), json!(80));
        options.extra.insert("moderation".into(), json!("low"));
        let params = build_params(&model(), &context_with_texts(&["x"]), Some(&options)).expect("params");
        assert_eq!(params.get("size"), Some(&Value::from("1024x1024")));
        assert_eq!(params.get("quality"), Some(&Value::from("high")));
        assert_eq!(params.get("n"), Some(&Value::from(2)));
        assert_eq!(params.get("background"), Some(&Value::from("opaque")));
        assert_eq!(params.get("output_format"), Some(&Value::from("webp")));
        assert_eq!(params.get("output_compression"), Some(&Value::from(80)));
        assert_eq!(params.get("moderation"), Some(&Value::from("low")));
    }

    #[test]
    fn image_params_duck_type_requires_a_string_prompt() {
        assert!(is_image_params(&json!({ "prompt": "x" })));
        assert!(!is_image_params(&json!({ "prompt": 3 })));
        assert!(!is_image_params(&json!({})));
    }
}
