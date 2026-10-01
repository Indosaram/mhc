//! Port of senpi packages/ai/src/api/google-shared.ts.
//!
//! The TS module imports `@google/genai` for its `Content`/`Part` types and the
//! `FinishReason`/`FunctionCallingConfigMode` enums; this port speaks the same wire JSON directly
//! (the SDK is a JavaScript package with no Rust analogue, the same trade the other wire-API ports
//! in this crate make). A `Part` is its wire object, so key order, `dominantPartKey` and the SDK's
//! verbatim passthrough of unknown parts are preserved.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::constrained_sampling::{get_json_schema_tool_parameters, resolve_json_schema_strict_sampling};
use crate::api::transform_messages::{transform_messages, NormalizeToolCallId, TransformMessagesOptions};
use crate::types::{
    AssistantMessage, Context, ImageContent, InputModality, Message, Model, ModelThinkingLevel,
    ProviderNativeContent, StopReason, Tool, UserContent,
};
use crate::utils::provider_retry::{
    retry_provider_request, ProviderErrorStatus, ProviderRequestError, ProviderRetryError, ProviderRetryOptions,
};
use crate::utils::sanitize_unicode::sanitize_surrogates;

/// A Google wire `Part`. The TS module treats parts as open objects (`Pick<Part, ...>` plus SDK
/// passthrough), so the port keeps the JSON object and reads typed fields off it.
pub type GooglePart = Map<String, Value>;

/// A Google wire `Content` (`{ role, parts }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoogleContent {
    pub role: String,
    pub parts: Vec<GooglePart>,
}

/// Thinking level for Gemini 3 models. Mirrors Google's ThinkingLevel enum values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleApiThinkingLevel {
    ThinkingLevelUnspecified,
    Minimal,
    Low,
    Medium,
    High,
}

impl GoogleApiThinkingLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            GoogleApiThinkingLevel::ThinkingLevelUnspecified => "THINKING_LEVEL_UNSPECIFIED",
            GoogleApiThinkingLevel::Minimal => "MINIMAL",
            GoogleApiThinkingLevel::Low => "LOW",
            GoogleApiThinkingLevel::Medium => "MEDIUM",
            GoogleApiThinkingLevel::High => "HIGH",
        }
    }
}

/// `Exclude<ThinkingLevel, "xhigh" | "max">`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedGoogleThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
}

impl ResolvedGoogleThinkingLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            ResolvedGoogleThinkingLevel::Minimal => "minimal",
            ResolvedGoogleThinkingLevel::Low => "low",
            ResolvedGoogleThinkingLevel::Medium => "medium",
            ResolvedGoogleThinkingLevel::High => "high",
        }
    }
}

/// Resolve a supported pi level or model-specific Google mapping to a standard Google level.
pub fn resolve_google_thinking_level(
    model: &Model,
    level: ModelThinkingLevel,
) -> Result<ResolvedGoogleThinkingLevel, String> {
    if level == ModelThinkingLevel::Off {
        return Ok(ResolvedGoogleThinkingLevel::High);
    }

    let mapped = model.thinking_level_map.as_ref().and_then(|map| map.get(&level));
    let mapped_string = mapped.and_then(|entry| entry.as_ref());
    let resolved_level = mapped_string.map(|value| value.to_lowercase()).unwrap_or_else(|| level.as_str().to_owned());
    match resolved_level.as_str() {
        "minimal" => Ok(ResolvedGoogleThinkingLevel::Minimal),
        "low" => Ok(ResolvedGoogleThinkingLevel::Low),
        "medium" => Ok(ResolvedGoogleThinkingLevel::Medium),
        "high" => Ok(ResolvedGoogleThinkingLevel::High),
        _ => {
            // `String(mapped)` for the TS `string | null | undefined` union.
            let mapped_text = match mapped {
                Some(Some(value)) => value.clone(),
                Some(None) => "null".to_owned(),
                None => "undefined".to_owned(),
            };
            Err(format!(
                "Unsupported Google thinking level mapping for {}/{}: {} -> {}",
                model.provider,
                model.id,
                level.as_str(),
                mapped_text
            ))
        }
    }
}

/// Determines whether a streamed Gemini `Part` should be treated as "thinking".
///
/// Protocol note (Gemini / Vertex AI thought signatures):
/// - `thought: true` is the definitive marker for thinking content (thought summaries).
/// - `thoughtSignature` is an encrypted representation of the model's internal thought process
///   used to preserve reasoning context across multi-turn interactions.
/// - `thoughtSignature` can appear on ANY part type (text, functionCall, etc.) - it does NOT
///   indicate the part itself is thinking content.
/// - For non-functionCall responses, the signature appears on the last part for context replay.
/// - When persisting/replaying model outputs, signature-bearing parts must be preserved as-is;
///   do not merge/move signatures across parts.
///
/// See: <https://ai.google.dev/gemini-api/docs/thought-signatures>
pub fn is_thinking_part(part: &GooglePart) -> bool {
    part.get("thought").and_then(Value::as_bool) == Some(true)
}

/// Retain thought signatures during streaming.
///
/// Some backends only send `thoughtSignature` on the first delta for a given part/block; later deltas may omit it.
/// This helper preserves the last non-empty signature for the current block.
///
/// Note: this does NOT merge or move signatures across distinct response parts. It only prevents
/// a signature from being overwritten with `undefined` within the same streamed block.
pub fn retain_thought_signature(existing: Option<String>, incoming: Option<&str>) -> Option<String> {
    match incoming {
        Some(incoming) if !incoming.is_empty() => Some(incoming.to_owned()),
        _ => existing,
    }
}

/// Thought signatures must be base64 for Google APIs (TYPE_BYTES).
fn is_valid_thought_signature(signature: Option<&str>) -> bool {
    let Some(signature) = signature.filter(|signature| !signature.is_empty()) else { return false };
    if signature.len() % 4 != 0 {
        return false;
    }
    let bytes = signature.as_bytes();
    let mut padding = 0;
    for byte in bytes {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' => {}
            b'=' => padding += 1,
            _ => return false,
        }
    }
    if padding > 2 {
        return false;
    }
    // `/^[A-Za-z0-9+/]+={0,2}$/`: at least one non-padding character before the padding.
    bytes.iter().position(|byte| *byte == b'=').is_none_or(|index| index > 0)
        && bytes[..bytes.len() - padding].iter().all(|byte| *byte != b'=')
}

/// Only keep signatures from the same provider/model and with valid base64.
fn resolve_thought_signature(is_same_provider_and_model: bool, signature: Option<&str>) -> Option<String> {
    if is_same_provider_and_model && is_valid_thought_signature(signature) {
        signature.map(str::to_owned)
    } else {
        None
    }
}

/// Models via Google APIs that require explicit tool call IDs in function calls/responses.
pub fn requires_tool_call_id(model_id: &str) -> bool {
    let gemini_major_version = get_gemini_major_version(model_id);
    model_id.starts_with("claude-")
        || model_id.starts_with("gpt-oss-")
        || gemini_major_version.is_some_and(|version| version >= 3)
}

fn get_gemini_major_version(model_id: &str) -> Option<u32> {
    let lower = model_id.to_lowercase();
    let rest = lower.strip_prefix("gemini")?;
    let rest = rest.strip_prefix("-live").unwrap_or(rest);
    let rest = rest.strip_prefix('-')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u32>().ok()
}

fn supports_multimodal_function_response(model_id: &str) -> bool {
    match get_gemini_major_version(model_id) {
        Some(version) => version >= 3,
        None => true,
    }
}

/// `convertMessages(model, context, options)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConvertMessagesOptions {
    pub preserve_thinking: Option<bool>,
}

fn text_part(text: String) -> GooglePart {
    let mut part = GooglePart::new();
    part.insert("text".into(), Value::String(text));
    part
}

fn thinking_part(text: String, thought_signature: Option<String>) -> GooglePart {
    let mut part = GooglePart::new();
    part.insert("thought".into(), Value::Bool(true));
    part.insert("text".into(), Value::String(text));
    if let Some(signature) = thought_signature {
        part.insert("thoughtSignature".into(), Value::String(signature));
    }
    part
}

fn inline_data_part(mime_type: Option<&str>, data: Option<&str>) -> GooglePart {
    let mut inline_data = Map::new();
    if let Some(mime_type) = mime_type {
        inline_data.insert("mimeType".into(), Value::String(mime_type.to_owned()));
    }
    if let Some(data) = data {
        inline_data.insert("data".into(), Value::String(data.to_owned()));
    }
    let mut part = GooglePart::new();
    part.insert("inlineData".into(), Value::Object(inline_data));
    part
}

fn image_part(image: &ImageContent) -> GooglePart {
    inline_data_part(Some(&image.mime_type), Some(&image.data))
}

/// Convert internal messages to Gemini Content[] format.
pub fn convert_messages(model: &Model, context: &Context, options: ConvertMessagesOptions) -> Vec<GoogleContent> {
    let mut contents: Vec<GoogleContent> = Vec::new();
    let requires_ids = requires_tool_call_id(&model.id);
    let normalize_id = move |id: &str, _model: &Model, _assistant: &AssistantMessage| -> String {
        if requires_ids {
            crate::utils::tool_call_id::normalize_tool_call_id(id)
        } else {
            id.to_owned()
        }
    };
    let normalize_id_ref: NormalizeToolCallId<'_> = &normalize_id;

    let transformed_messages = transform_messages(
        &context.messages,
        model,
        Some(normalize_id_ref),
        &TransformMessagesOptions {
            preserve_thinking: options.preserve_thinking,
            ..TransformMessagesOptions::default()
        },
    );

    for msg in transformed_messages {
        match msg {
            Message::User(user) => match user.content {
                UserContent::Text(text) => contents.push(GoogleContent {
                    role: "user".into(),
                    parts: vec![text_part(sanitize_surrogates(&text))],
                }),
                UserContent::Blocks(blocks) => {
                    let parts: Vec<GooglePart> = blocks
                        .iter()
                        .map(|item| match item {
                            crate::types::ContentBlock::Text(text) => text_part(sanitize_surrogates(&text.text)),
                            crate::types::ContentBlock::Image(image) => image_part(image),
                            // The TS union is `(TextContent | ImageContent)[]`; the else branch reads
                            // `item.mimeType`/`item.data`, which JSON drops when undefined.
                            _ => inline_data_part(None, None),
                        })
                        .collect();
                    if parts.is_empty() {
                        continue;
                    }
                    contents.push(GoogleContent { role: "user".into(), parts });
                }
            },
            Message::Assistant(assistant) => {
                let mut parts: Vec<GooglePart> = Vec::new();
                // Check if message is from same provider and model - only then keep thinking blocks
                let is_same_provider_and_model = assistant.provider == model.provider && assistant.model == model.id;

                for block in &assistant.content {
                    match block {
                        crate::types::ContentBlock::Text(text) => {
                            let thought_signature =
                                resolve_thought_signature(is_same_provider_and_model, text.text_signature.as_deref());
                            // Skip empty text blocks — unless they carry a thought signature. Gemini can attach
                            // the signature to a part whose visible text is empty and requires it echoed back;
                            // dropping it breaks the reasoning chain and the model intermittently ends mid-task
                            // turns with a thought-only STOP (empty completion, no tool call).
                            if text.text.trim().is_empty() && thought_signature.is_none() {
                                continue;
                            }
                            parts.push(thinking_signature_text_part(
                                sanitize_surrogates(&text.text),
                                thought_signature,
                            ));
                        }
                        crate::types::ContentBlock::Thinking(thinking) => {
                            // Only keep as thinking block if same provider AND same model
                            // Otherwise convert to plain text (no tags to avoid model mimicking them)
                            if is_same_provider_and_model {
                                let thought_signature = resolve_thought_signature(
                                    is_same_provider_and_model,
                                    thinking.thinking_signature.as_deref(),
                                );
                                // Same rule as text blocks: an empty thinking block is dropped only when it
                                // carries no signature (mirrors the anthropic converter's handling).
                                if thinking.thinking.trim().is_empty() && thought_signature.is_none() {
                                    continue;
                                }
                                parts.push(thinking_part(sanitize_surrogates(&thinking.thinking), thought_signature));
                            } else {
                                // Cross-provider/model: the signature is unusable, empty blocks stay dropped.
                                if thinking.thinking.trim().is_empty() {
                                    continue;
                                }
                                parts.push(text_part(sanitize_surrogates(&thinking.thinking)));
                            }
                        }
                        crate::types::ContentBlock::ToolCall(tool_call) => {
                            let thought_signature = resolve_thought_signature(
                                is_same_provider_and_model,
                                tool_call.thought_signature.as_deref(),
                            );
                            let mut function_call = Map::new();
                            function_call.insert("name".into(), Value::String(tool_call.name.clone()));
                            function_call.insert("args".into(), Value::Object(tool_call.arguments.clone()));
                            if requires_ids {
                                function_call.insert("id".into(), Value::String(tool_call.id.clone()));
                            }
                            let mut part = GooglePart::new();
                            part.insert("functionCall".into(), Value::Object(function_call));
                            if let Some(signature) = thought_signature {
                                part.insert("thoughtSignature".into(), Value::String(signature));
                            }
                            parts.push(part);
                        }
                        crate::types::ContentBlock::ProviderNative(_) => {}
                        crate::types::ContentBlock::Image(_) => {}
                    }
                }

                if parts.is_empty() {
                    continue;
                }
                contents.push(GoogleContent { role: "model".into(), parts });
            }
            Message::ToolResult(result) => {
                // Extract text and image content
                let text_result = result
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        crate::types::ContentBlock::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<String>>()
                    .join("\n");
                let image_content: Vec<&ImageContent> = if model.input.contains(&InputModality::Image) {
                    result
                        .content
                        .iter()
                        .filter_map(|content| match content {
                            crate::types::ContentBlock::Image(image) => Some(image),
                            _ => None,
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                let has_text = !text_result.is_empty();
                let has_images = !image_content.is_empty();

                // Gemini 3+ models support multimodal function responses with images nested inside
                // functionResponse.parts. Claude and other non-Gemini models behind Cloud Code Assist /
                // Gemini < 3 still needs a separate user image turn.
                let model_supports_multimodal_function_response = supports_multimodal_function_response(&model.id);

                // Use "output" key for success, "error" key for errors as per SDK documentation
                let response_value = if has_text {
                    sanitize_surrogates(&text_result)
                } else if has_images {
                    "(see attached image)".to_owned()
                } else {
                    String::new()
                };

                let image_parts: Vec<GooglePart> = image_content.iter().map(|image| image_part(image)).collect();

                let mut response = Map::new();
                if result.is_error {
                    response.insert("error".into(), Value::String(response_value));
                } else {
                    response.insert("output".into(), Value::String(response_value));
                }

                let mut function_response = Map::new();
                function_response.insert("name".into(), Value::String(result.tool_name.clone()));
                function_response.insert("response".into(), Value::Object(response));
                if has_images && model_supports_multimodal_function_response {
                    function_response.insert(
                        "parts".into(),
                        Value::Array(image_parts.iter().cloned().map(Value::Object).collect()),
                    );
                }
                if requires_ids {
                    function_response.insert("id".into(), Value::String(result.tool_call_id.clone()));
                }

                let mut function_response_part = GooglePart::new();
                function_response_part.insert("functionResponse".into(), Value::Object(function_response));

                // Cloud Code Assist API requires all function responses to be in a single user turn.
                // Check if the last content is already a user turn with function responses and merge.
                let merge_into_last = contents.last().is_some_and(|last| {
                    last.role == "user" && last.parts.iter().any(|part| js_truthy(part.get("functionResponse")))
                });
                if merge_into_last {
                    contents.last_mut().expect("last content is present").parts.push(function_response_part);
                } else {
                    contents.push(GoogleContent { role: "user".into(), parts: vec![function_response_part] });
                }

                // For Gemini < 3, add images in a separate user message
                if has_images && !model_supports_multimodal_function_response {
                    let mut parts = vec![text_part("Tool result image:".into())];
                    parts.extend(image_parts);
                    contents.push(GoogleContent { role: "user".into(), parts });
                }
            }
            Message::ConfigurationUpdate(_) => {}
        }
    }

    contents
}

fn thinking_signature_text_part(text: String, thought_signature: Option<String>) -> GooglePart {
    let mut part = GooglePart::new();
    part.insert("text".into(), Value::String(text));
    if let Some(signature) = thought_signature {
        part.insert("thoughtSignature".into(), Value::String(signature));
    }
    part
}

/// JS truthiness for a JSON value (`if (part.functionResponse)`).
pub fn js_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|number| number != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

fn dominant_part_key(part: &GooglePart) -> String {
    part.iter()
        .find(|(_, value)| !value.is_null())
        .map(|(key, _)| key.clone())
        .unwrap_or_else(|| "unknown".to_owned())
}

pub fn to_provider_native_content(part: &GooglePart) -> ProviderNativeContent {
    if let Some(executable_code) = part.get("executableCode") {
        return ProviderNativeContent {
            subtype: "executableCode".into(),
            raw: serde_json::json!({ "executableCode": executable_code }),
        };
    }

    if let Some(code_execution_result) = part.get("codeExecutionResult") {
        return ProviderNativeContent {
            subtype: "codeExecutionResult".into(),
            raw: serde_json::json!({ "codeExecutionResult": code_execution_result }),
        };
    }

    ProviderNativeContent { subtype: dominant_part_key(part), raw: Value::Object(part.clone()) }
}

static JSON_SCHEMA_META_DECLARATIONS: &[&str] = &[
    "$schema",
    "$id",
    "$anchor",
    "$dynamicAnchor",
    "$vocabulary",
    "$comment",
    "$defs",
    "definitions", // pre-draft-2019-09 equivalent of $defs
];

/// Strip meta-declarations from a schema obj
fn sanitize_for_open_api(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, value) in map {
                if JSON_SCHEMA_META_DECLARATIONS.contains(&key.as_str()) {
                    continue;
                }
                result.insert(key.clone(), sanitize_for_open_api(value));
            }
            Value::Object(result)
        }
        Value::Array(items) => Value::Array(items.iter().map(sanitize_for_open_api).collect()),
        other => other.clone(),
    }
}

/// Keywords whose values hold instance data, not subschemas. These must be
/// passed through untouched so that e.g. `const: { optional: true }` is
/// preserved as legitimate data.
static SCHEMA_VALUE_KEYWORDS: &[&str] = &["const", "default", "examples", "enum"];

/// Keywords whose values are maps of name → subschema (e.g. `properties`).
/// The map keys are instance property / definition names and must be
/// preserved even if one happens to be named `optional`.
static SCHEMA_MAP_KEYS_STRIP: &[&str] = &["properties", "patternProperties", "$defs", "definitions"];

/// Strip the non-standard 'optional' keyword from a JSON schema object.
///
/// Position-aware: only strips `optional` when it appears as a schema keyword
/// (a sibling of `type`, `properties`, etc.). Preserves `optional` when it is
/// a property name inside `properties`/`patternProperties`/`$defs`/`definitions`,
/// and does not traverse value keywords (`const`/`default`/`examples`/`enum`)
/// whose contents are instance data, not subschemas.
fn strip_optional(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, value) in map {
                // Strip the non-standard 'optional' keyword from schema keyword position.
                if key == "optional" {
                    continue;
                }

                // Value keywords hold instance data, not schemas — pass through untouched.
                if SCHEMA_VALUE_KEYWORDS.contains(&key.as_str()) {
                    result.insert(key.clone(), value.clone());
                    continue;
                }

                // Map keys hold instance property names / definition names as keys.
                // Preserve each key but recurse into the subschema value.
                if SCHEMA_MAP_KEYS_STRIP.contains(&key.as_str())
                    && let Value::Object(map) = value
                {
                    let mut stripped = Map::new();
                    for (name, subschema) in map {
                        stripped.insert(name.clone(), strip_optional(subschema));
                    }
                    result.insert(key.clone(), Value::Object(stripped));
                    continue;
                }

                // All other keywords hold schemas (or schema arrays) — recurse.
                result.insert(key.clone(), strip_optional(value));
            }
            Value::Object(result)
        }
        Value::Array(items) => Value::Array(items.iter().map(strip_optional).collect()),
        other => other.clone(),
    }
}

/// Convert tools to Gemini function declarations format.
///
/// By default uses `parametersJsonSchema` which supports full JSON Schema (including
/// anyOf, oneOf, const, etc.). Set `useParameters` to true to use the legacy `parameters`
/// field instead (OpenAPI 3.03 Schema). This is needed for Cloud Code Assist with Claude
/// models, where the API translates `parameters` into Anthropic's `input_schema`.
pub fn convert_tools(
    tools: &[Tool],
    use_parameters: bool,
    supports_strict_mode: bool,
) -> Result<Option<Vec<Value>>, String> {
    if tools.is_empty() {
        return Ok(None);
    }
    let mut declarations: Vec<Value> = Vec::with_capacity(tools.len());
    for tool in tools {
        let strict = resolve_json_schema_strict_sampling(tool, supports_strict_mode)?;
        let parameters = get_json_schema_tool_parameters(tool, strict).map_err(|error| error.0)?;
        let mut declaration = Map::new();
        declaration.insert("name".into(), Value::String(tool.name.clone()));
        declaration.insert("description".into(), Value::String(tool.description.clone()));
        if use_parameters {
            declaration.insert("parameters".into(), strip_optional(&sanitize_for_open_api(&parameters)));
        } else {
            declaration.insert("parametersJsonSchema".into(), strip_optional(&parameters));
        }
        declarations.push(Value::Object(declaration));
    }
    Ok(Some(vec![serde_json::json!({ "functionDeclarations": declarations })]))
}

/// Gemini 3+ enforces required function parameters in validated tool-calling modes.
pub fn supports_google_strict_tool_sampling(model_id: &str) -> bool {
    get_gemini_major_version(model_id).is_some_and(|version| version >= 3)
}

/// Map tool choice string to Gemini FunctionCallingConfigMode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionCallingConfigMode {
    Auto,
    None,
    Any,
    Validated,
}

impl FunctionCallingConfigMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FunctionCallingConfigMode::Auto => "AUTO",
            FunctionCallingConfigMode::None => "NONE",
            FunctionCallingConfigMode::Any => "ANY",
            FunctionCallingConfigMode::Validated => "VALIDATED",
        }
    }
}

pub fn map_tool_choice(choice: &str) -> FunctionCallingConfigMode {
    match choice {
        "auto" => FunctionCallingConfigMode::Auto,
        "none" => FunctionCallingConfigMode::None,
        "any" => FunctionCallingConfigMode::Any,
        _ => FunctionCallingConfigMode::Auto,
    }
}

pub fn resolve_google_function_calling_mode(
    tools: &[Tool],
    tool_choice: Option<&str>,
    supports_strict_mode: bool,
) -> Result<Option<FunctionCallingConfigMode>, String> {
    let mut use_strict_mode = false;
    for tool in tools {
        if resolve_json_schema_strict_sampling(tool, supports_strict_mode)? == Some(true) {
            use_strict_mode = true;
            break;
        }
    }
    if tool_choice == Some("none") || tool_choice == Some("any") {
        return Ok(Some(map_tool_choice(tool_choice.unwrap_or("auto"))));
    }
    if use_strict_mode {
        return Ok(Some(FunctionCallingConfigMode::Validated));
    }
    Ok(tool_choice.map(map_tool_choice))
}

/// Map Gemini FinishReason to our StopReason.
pub fn map_stop_reason(reason: &str) -> Result<StopReason, String> {
    match reason {
        "STOP" => Ok(StopReason::Stop),
        "MAX_TOKENS" => Ok(StopReason::Length),
        "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "SAFETY" | "IMAGE_SAFETY" | "IMAGE_PROHIBITED_CONTENT"
        | "IMAGE_RECITATION" | "IMAGE_OTHER" | "RECITATION" | "FINISH_REASON_UNSPECIFIED" | "OTHER" | "LANGUAGE"
        | "MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL" | "TOO_MANY_TOOL_CALLS" | "NO_IMAGE" => {
            Ok(StopReason::Error)
        }
        other => Err(format!("Unhandled stop reason: {other}")),
    }
}

/// Map string finish reason to our StopReason (for raw API responses).
pub fn map_stop_reason_string(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::Length,
        _ => StopReason::Error,
    }
}

/// A Google request failure: an HTTP response the SDK would surface as an `ApiError` (`status` but
/// no `headers`), or a plain message (a stream-protocol failure).
#[derive(Debug, Clone)]
pub enum GoogleRequestError {
    /// The SDK's `ApiError`/`Error` shape: `message` plus an HTTP `status`.
    Sdk { message: String, status: Option<u16>, headers: Option<reqwest::header::HeaderMap> },
    /// A locally detected failure with no HTTP shape at all.
    Message(String),
}

impl std::fmt::Display for GoogleRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoogleRequestError::Sdk { message, .. } | GoogleRequestError::Message(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl ProviderRequestError for GoogleRequestError {
    fn provider_status(&self) -> Option<ProviderErrorStatus<'_>> {
        match self {
            GoogleRequestError::Sdk { status, headers, .. } => {
                Some(ProviderErrorStatus { status: *status, headers: headers.as_ref() })
            }
            GoogleRequestError::Message(_) => None,
        }
    }
}

/// Run a Google GenAI SDK request with the shared provider retry policy
/// (408/409/429/5xx with backoff, honoring retry-after), mirroring how the
/// Anthropic and [OI] adapters wrap their initial request in
/// retryProviderRequest. The SDK's ApiError has a `status` property but no
/// `headers` property, and retryProviderRequest only retries errors that carry
/// both, so normalize the error by adding the missing `headers` before
/// rethrowing — which is what [`GoogleRequestError::Sdk`] already carries.
pub async fn retry_google_request<T, F, Fut>(
    request: F,
    options: &ProviderRetryOptions,
) -> Result<T, ProviderRetryError<GoogleRequestError>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, GoogleRequestError>>,
{
    retry_provider_request(request, options).await
}

/// `JSON.stringify(value)` for the compact shapes the adapters build (tool-call arguments, error
/// bodies). Numbers format the way JS prints them, and object keys use JS order.
pub fn js_json_stringify(value: &Value) -> String {
    let mut out = String::new();
    write_compact_json(&mut out, value);
    out
}

fn write_compact_json(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Value::Number(number) => match number.as_f64().filter(|value| value.is_finite()) {
            Some(value) => out.push_str(&crate::utils::js::number_to_string(value)),
            None => out.push_str("null"),
        },
        Value::String(text) => out.push_str(&serde_json::to_string(text).unwrap_or_default()),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_compact_json(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (index, key) in crate::utils::js::object_keys(map).into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push(':');
                if let Some(item) = map.get(key) {
                    write_compact_json(out, item);
                }
            }
            out.push('}');
        }
    }
}

/// The `GoogleGenAI` constructor options the TS adapters build. The SDK is a JavaScript package
/// with no Rust analogue, so the port keeps the same fields and derives the request URL and auth
/// headers the SDK would.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoogleClientConfig {
    pub vertexai: bool,
    pub api_key: Option<String>,
    pub project: Option<String>,
    pub location: Option<String>,
    pub api_version: Option<String>,
    pub base_url: Option<String>,
    /// `httpOptions.baseUrlResourceScope = ResourceScope.COLLECTION`.
    pub base_url_resource_scope_collection: bool,
    /// `googleAuthOptions.keyFilename` (`GOOGLE_APPLICATION_CREDENTIALS`).
    pub google_auth_key_filename: Option<String>,
    pub headers: Option<std::collections::BTreeMap<String, String>>,
}

impl GoogleClientConfig {
    /// `ApiClient.getRequestUrlInternal` + `constructUrl` + `tModel` for
    /// `models.generateContentStream`.
    pub fn stream_generate_content_url(&self, model_id: &str) -> String {
        let (default_base_url, default_api_version) = if self.vertexai {
            (self.vertex_default_base_url(), "v1beta1")
        } else {
            ("https://generativelanguage.googleapis.com".to_owned(), "v1beta")
        };
        let base_url = self.base_url.clone().unwrap_or(default_base_url);
        let base_url = base_url.trim_end_matches('/');
        let api_version = self.api_version.clone().unwrap_or_else(|| default_api_version.to_owned());
        let mut url = if api_version.is_empty() { base_url.to_owned() } else { format!("{base_url}/{api_version}") };

        let path = if self.vertexai { self.vertex_model_path(model_id) } else { mldev_model_path(model_id) };
        if self.should_prepend_vertex_project_path(&path) {
            let project = self.project.as_deref().unwrap_or_default();
            let location = self.location.as_deref().unwrap_or_default();
            url = format!("{url}/projects/{project}/locations/{location}");
        }
        format!("{url}/{path}:streamGenerateContent?alt=sse")
    }

    fn vertex_default_base_url(&self) -> String {
        if self.project.is_none() || self.location.is_none() {
            return "https://aiplatform.googleapis.com".to_owned();
        }
        match self.location.as_deref().unwrap_or_default() {
            "global" => "https://aiplatform.googleapis.com".to_owned(),
            location @ ("us" | "eu") => format!("https://aiplatform.{location}.rep.googleapis.com"),
            location => format!("https://{location}-aiplatform.googleapis.com"),
        }
    }

    fn vertex_model_path(&self, model_id: &str) -> String {
        if model_id.starts_with("publishers/")
            || model_id.starts_with("projects/")
            || model_id.starts_with("models/")
        {
            return model_id.to_owned();
        }
        match model_id.split_once('/') {
            Some((publisher, model)) => format!("publishers/{publisher}/models/{model}"),
            None => format!("publishers/google/models/{model_id}"),
        }
    }

    fn should_prepend_vertex_project_path(&self, path: &str) -> bool {
        if self.base_url_resource_scope_collection || !self.vertexai {
            return false;
        }
        if self.project.is_none() || self.location.is_none() {
            return false;
        }
        !path.starts_with("projects/")
    }
}

fn mldev_model_path(model_id: &str) -> String {
    if model_id.starts_with("models/") || model_id.starts_with("tunedModels/") {
        model_id.to_owned()
    } else {
        format!("models/{model_id}")
    }
}

/// The auth the SDK's `GoogleAuth` resolves for a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoogleAuth {
    /// `x-goog-api-key` (an explicit API key).
    ApiKey(String),
    /// `authorization: Bearer …` (Application Default Credentials).
    Bearer(String),
}

/// The outbound `models.generateContentStream` request.
#[derive(Debug, Clone)]
pub struct GoogleStreamRequest<'a> {
    pub url: String,
    pub headers: Option<&'a std::collections::BTreeMap<String, String>>,
    pub auth: Option<GoogleAuth>,
    pub body: Value,
}

/// Issues the streaming generate-content POST and returns the SSE response, mapping a non-OK
/// response to the error shape the SDK's `throwErrorIfNotOK` produces (`ApiError` with
/// `message = JSON.stringify(errorBody)` and `status`).
pub async fn post_stream_generate_content(
    http: &reqwest::Client,
    request: GoogleStreamRequest<'_>,
) -> Result<reqwest::Response, GoogleRequestError> {
    let mut builder = http.post(&request.url).header("content-type", "application/json");
    if let Some(headers) = request.headers {
        for (name, value) in headers {
            let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                reqwest::header::HeaderValue::from_str(value),
            ) else {
                continue;
            };
            builder = builder.header(name, value);
        }
    }
    match request.auth {
        Some(GoogleAuth::ApiKey(api_key)) => builder = builder.header("x-goog-api-key", api_key),
        Some(GoogleAuth::Bearer(token)) => builder = builder.bearer_auth(token),
        None => {}
    }
    let body = serde_json::to_string(&request.body).map_err(|error| GoogleRequestError::Message(error.to_string()))?;
    let response = builder.body(body).send().await.map_err(|error| GoogleRequestError::Message(error.to_string()))?;
    if !response.status().is_success() {
        return Err(http_error_from_response(response).await);
    }
    Ok(response)
}

/// The SDK's `generateContentParametersToMldev`/`...ToVertex` plus
/// `generateContentConfigToMldev`/`...ToVertex`: the adapter-level `{ model, contents, config }`
/// params become the wire body the endpoint accepts. The model rides in the URL, so it is dropped
/// from the body, and `config` keys the SDK's generated converters do not know are dropped too —
/// which is why `GOOGLE_RESERVED_BODY_KEYS` only reserves the keys the SDK understands.
pub fn request_body(params: &Value, vertex: bool) -> Result<Value, GoogleRequestError> {
    let mut body = Map::new();
    if let Some(contents) = params.get("contents") {
        body.insert("contents".into(), contents.clone());
    }
    let mut generation_config = Map::new();
    if let Some(config) = params.get("config").and_then(Value::as_object) {
        for (key, value) in config {
            match key.as_str() {
                "systemInstruction" => {
                    body.insert("systemInstruction".into(), content_from_string(value));
                }
                "safetySettings" | "tools" | "toolConfig" => {
                    body.insert(key.clone(), value.clone());
                }
                "serviceTier" => {
                    body.insert("serviceTier".into(), value.clone());
                }
                "labels" if vertex => {
                    body.insert("labels".into(), value.clone());
                }
                "labels" => {
                    return Err(GoogleRequestError::Message(
                        "labels parameter is only supported in Gemini Enterprise Agent Platform mode, not in Gemini Developer API mode."
                            .into(),
                    ));
                }
                "cachedContent" if vertex => {
                    body.insert("cachedContent".into(), value.clone());
                }
                "temperature" | "topP" | "topK" | "candidateCount" | "maxOutputTokens" | "stopSequences"
                | "responseLogprobs" | "logprobs" | "presencePenalty" | "frequencyPenalty" | "seed"
                | "responseMimeType" | "responseSchema" | "responseJsonSchema" | "cachedContent"
                | "thinkingConfig" => {
                    generation_config.insert(key.clone(), value.clone());
                }
                _ => {}
            }
        }
    }
    if !generation_config.is_empty() {
        body.insert("generationConfig".into(), Value::Object(generation_config));
    }
    Ok(Value::Object(body))
}

/// The SDK's `tContent`: a string becomes a user turn with one text part.
fn content_from_string(value: &Value) -> Value {
    match value {
        Value::String(text) => serde_json::json!({ "role": "user", "parts": [{ "text": text }] }),
        other => other.clone(),
    }
}

async fn http_error_from_response(response: reqwest::Response) -> GoogleRequestError {
    let status = response.status().as_u16();
    let status_text = response.status().canonical_reason().unwrap_or_default().to_owned();
    let is_json = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("application/json"));
    let text = response.text().await.unwrap_or_default();
    let message = if is_json {
        serde_json::from_str::<Value>(&text).map(|value| js_json_stringify(&value)).unwrap_or(text)
    } else {
        js_json_stringify(&serde_json::json!({
            "error": { "message": text, "code": status, "status": status_text }
        }))
    };
    GoogleRequestError::Sdk { message, status: Some(status), headers: None }
}

/// The SDK's `processStreamResponse` SSE reader: decodes network chunks into parsed
/// `GenerateContentResponse` values, raising the SDK's in-band error before yielding them.
#[derive(Debug, Default)]
pub struct GoogleSseDecoder {
    buffer: String,
}

impl GoogleSseDecoder {
    pub fn push_bytes(&mut self, bytes: &[u8]) -> Result<Vec<Value>, GoogleRequestError> {
        let chunk_string = String::from_utf8_lossy(bytes).into_owned();
        if let Ok(chunk_json) = serde_json::from_str::<Value>(&chunk_string)
            && let Some(error) = chunk_json.get("error")
            && let Some(code) = error.get("code").and_then(Value::as_u64).filter(|code| (400..600).contains(code))
        {
            return Err(GoogleRequestError::Sdk {
                message: format!(
                    "got status: {}. {}",
                    crate::utils::diagnostics::js_string(error.get("status").unwrap_or(&Value::Null)),
                    js_json_stringify(&chunk_json)
                ),
                status: Some(code as u16),
                headers: None,
            });
        }
        self.buffer.push_str(&chunk_string);
        let mut chunks = Vec::new();
        while let Some((index, length)) = find_sse_event_boundary(&self.buffer) {
            let event = self.buffer[..index].to_owned();
            self.buffer = self.buffer[index + length..].to_owned();
            let trimmed = crate::utils::js::trim(&event);
            let Some(payload) = trimmed.strip_prefix("data:") else { continue };
            let payload = crate::utils::js::trim(payload);
            let chunk = serde_json::from_str::<Value>(payload)
                .map_err(|error| GoogleRequestError::Message(error.to_string()))?;
            chunks.push(chunk);
        }
        Ok(chunks)
    }

    /// The SDK's end-of-stream check: a non-empty buffer is an incomplete JSON segment.
    pub fn finish(&mut self) -> Result<Vec<Value>, GoogleRequestError> {
        if self.buffer.trim().is_empty() {
            return Ok(Vec::new());
        }
        Err(GoogleRequestError::Message("Incomplete JSON segment at the end".into()))
    }
}

const SSE_DELIMITERS: [&str; 3] = ["\n\n", "\r\r", "\r\n\r\n"];

fn find_sse_event_boundary(buffer: &str) -> Option<(usize, usize)> {
    let mut found: Option<(usize, usize)> = None;
    for delimiter in SSE_DELIMITERS {
        if let Some(index) = buffer.find(delimiter)
            && found.is_none_or(|(best, _)| index < best)
        {
            found = Some((index, delimiter.len()));
        }
    }
    found
}

/// Application Default Credentials, mirroring `google-auth-library`'s `getApplicationDefault`:
/// the `GOOGLE_APPLICATION_CREDENTIALS` file, then the well-known gcloud file, then the GCE
/// metadata server.
pub async fn resolve_application_default_credentials(
    http: &reqwest::Client,
    key_filename: Option<&str>,
) -> Result<String, GoogleRequestError> {
    if let Some(path) = key_filename.filter(|path| !path.is_empty()) {
        return access_token_from_credentials_file(http, path).await;
    }
    if let Some(home) = dirs::home_dir() {
        let well_known = home.join(".config/gcloud/application_default_credentials.json");
        if well_known.exists() {
            return access_token_from_credentials_file(http, &well_known.to_string_lossy()).await;
        }
    }
    match metadata_server_access_token(http).await {
        Ok(token) => Ok(token),
        Err(_) => Err(GoogleRequestError::Message(
            "Could not load the default credentials. Browse to https://cloud.google.com/docs/authentication/getting-started for more information."
                .into(),
        )),
    }
}

async fn metadata_server_access_token(http: &reqwest::Client) -> Result<String, GoogleRequestError> {
    let response = http
        .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
        .header("Metadata-Flavor", "Google")
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .map_err(|error| GoogleRequestError::Message(error.to_string()))?;
    if !response.status().is_success() {
        return Err(http_error_from_response(response).await);
    }
    let body = response.text().await.map_err(|error| GoogleRequestError::Message(error.to_string()))?;
    serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| value.get("access_token").and_then(Value::as_str).map(str::to_owned))
        .ok_or_else(|| GoogleRequestError::Message("metadata server returned no access token".into()))
}

/// Cached ADC tokens, keyed by credentials path (the SDK's `GoogleAuth` caches per client).
static ADC_TOKEN_CACHE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, (String, i64)>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

async fn access_token_from_credentials_file(
    http: &reqwest::Client,
    path: &str,
) -> Result<String, GoogleRequestError> {
    let now = crate::utils::diagnostics::now_ms();
    if let Some((token, _expires_at)) = ADC_TOKEN_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(path)
        .filter(|(_, expires_at)| *expires_at - 60_000 > now)
        .cloned()
    {
        return Ok(token);
    }

    let text = std::fs::read_to_string(path).map_err(|error| {
        GoogleRequestError::Message(format!(
            "Unable to read the credential file specified by the GOOGLE_APPLICATION_CREDENTIALS environment variable: {error}"
        ))
    })?;
    let credentials: Value = serde_json::from_str(&text)
        .map_err(|error| GoogleRequestError::Message(format!("Unable to parse the credential file {path}: {error}")))?;
    let credential_type = credentials.get("type").and_then(Value::as_str).unwrap_or_default();
    let (token, expires_in) = match credential_type {
        "authorized_user" => refresh_authorized_user_token(http, &credentials).await?,
        "service_account" => service_account_access_token(http, &credentials).await?,
        other => {
            return Err(GoogleRequestError::Message(format!("Unsupported credential type {other} in {path}")));
        }
    };
    ADC_TOKEN_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.to_owned(), (token.clone(), now + expires_in.saturating_mul(1000) as i64));
    Ok(token)
}

fn credential_field(credentials: &Value, name: &str) -> Result<String, GoogleRequestError> {
    credentials
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| GoogleRequestError::Message(format!("credential file is missing {name}")))
}

async fn refresh_authorized_user_token(
    http: &reqwest::Client,
    credentials: &Value,
) -> Result<(String, u64), GoogleRequestError> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &credential_field(credentials, "client_id")?)
        .append_pair("client_secret", &credential_field(credentials, "client_secret")?)
        .append_pair("refresh_token", &credential_field(credentials, "refresh_token")?)
        .append_pair("grant_type", "refresh_token")
        .finish();
    let response = http
        .post("https://oauth2.googleapis.com/token")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|error| GoogleRequestError::Message(error.to_string()))?;
    read_token_response(response).await
}

async fn service_account_access_token(
    http: &reqwest::Client,
    credentials: &Value,
) -> Result<(String, u64), GoogleRequestError> {
    let token_uri = credentials
        .get("token_uri")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("https://oauth2.googleapis.com/token")
        .to_owned();
    let assertion = service_account_assertion(credentials, &token_uri)?;
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer")
        .append_pair("assertion", &assertion)
        .finish();
    let response = http
        .post(&token_uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|error| GoogleRequestError::Message(error.to_string()))?;
    read_token_response(response).await
}

async fn read_token_response(response: reqwest::Response) -> Result<(String, u64), GoogleRequestError> {
    if !response.status().is_success() {
        return Err(http_error_from_response(response).await);
    }
    let body = response.text().await.map_err(|error| GoogleRequestError::Message(error.to_string()))?;
    let value: Value = serde_json::from_str(&body)
        .map_err(|error| GoogleRequestError::Message(format!("token endpoint returned invalid JSON: {error}")))?;
    let token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| GoogleRequestError::Message("token endpoint returned no access_token".into()))?;
    let expires_in = value.get("expires_in").and_then(Value::as_u64).unwrap_or(3600);
    Ok((token.to_owned(), expires_in))
}

fn service_account_assertion(credentials: &Value, token_uri: &str) -> Result<String, GoogleRequestError> {
    let client_email = credential_field(credentials, "client_email")?;
    let private_key = credential_field(credentials, "private_key")?;
    let now = crate::utils::diagnostics::now_ms() / 1000;
    let header = serde_json::json!({ "alg": "RS256", "typ": "JWT" });
    let claims = serde_json::json!({
        "iss": client_email,
        "scope": "https://www.googleapis.com/auth/cloud-platform",
        "aud": token_uri,
        "iat": now,
        "exp": now + 3600,
    });
    let signing_input =
        format!("{}.{}", base64_url(&js_json_stringify(&header)), base64_url(&js_json_stringify(&claims)));
    let signature = sign_rs256(&private_key, signing_input.as_bytes())?;
    Ok(format!("{signing_input}.{}", base64_url_bytes(&signature)))
}

fn base64_url(text: &str) -> String {
    base64_url_bytes(text.as_bytes())
}

fn base64_url_bytes(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn sign_rs256(private_key_pem: &str, message: &[u8]) -> Result<Vec<u8>, GoogleRequestError> {
    use base64::Engine;
    let der = private_key_pem.lines().filter(|line| !line.starts_with("-----")).collect::<String>();
    let der = base64::engine::general_purpose::STANDARD
        .decode(der.trim())
        .map_err(|error| GoogleRequestError::Message(format!("invalid private key: {error}")))?;
    let key_pair = ring::signature::RsaKeyPair::from_pkcs8(&der)
        .map_err(|error| GoogleRequestError::Message(format!("invalid private key: {error}")))?;
    let mut signature = vec![0u8; key_pair.public().modulus_len()];
    key_pair
        .sign(&ring::signature::RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), message, &mut signature)
        .map_err(|error| GoogleRequestError::Message(format!("failed to sign the assertion: {error}")))?;
    Ok(signature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gemini_major_versions_parse_like_the_ts_regex() {
        assert_eq!(get_gemini_major_version("gemini-2.5-flash"), Some(2));
        assert_eq!(get_gemini_major_version("gemini-3.1-pro-preview"), Some(3));
        assert_eq!(get_gemini_major_version("gemini-live-3-flash"), Some(3));
        assert_eq!(get_gemini_major_version("gemini-flash-latest"), None);
        assert_eq!(get_gemini_major_version("gpt-oss-120b"), None);
    }

    #[test]
    fn thought_signatures_must_be_base64() {
        assert!(is_valid_thought_signature(Some("AAAAAAAAAAAAAAAAAAAAAA==")));
        assert!(!is_valid_thought_signature(Some("AAA")));
        assert!(!is_valid_thought_signature(Some("")));
        assert!(!is_valid_thought_signature(None));
        assert!(!is_valid_thought_signature(Some("opaque-signature")));
    }

    #[test]
    fn dominant_part_keys_follow_insertion_order() {
        let mut part = GooglePart::new();
        part.insert("text".into(), Value::String("hi".into()));
        assert_eq!(dominant_part_key(&part), "text");
        let mut empty = GooglePart::new();
        assert_eq!(dominant_part_key(&empty), "unknown");
        empty.insert("maybe".into(), Value::Null);
        assert_eq!(dominant_part_key(&empty), "unknown");
    }

    fn part(value: Value) -> GooglePart {
        value.as_object().cloned().expect("a part object")
    }

    #[test]
    fn provider_native_content_labels_the_known_google_part_kinds() {
        let executable =
            to_provider_native_content(&part(json!({ "executableCode": { "language": "python", "code": "print(1)" } })));
        assert_eq!(executable.subtype, "executableCode");
        assert_eq!(executable.raw, json!({ "executableCode": { "language": "python", "code": "print(1)" } }));

        let result = to_provider_native_content(&part(
            json!({ "codeExecutionResult": { "outcome": "OUTCOME_OK", "output": "1\n" } }),
        ));
        assert_eq!(result.subtype, "codeExecutionResult");

        let other = to_provider_native_content(&part(json!({ "fileData": { "fileUri": "gs://x" } })));
        assert_eq!(other.subtype, "fileData");
        assert_eq!(other.raw, json!({ "fileData": { "fileUri": "gs://x" } }));
    }

    #[test]
    fn request_bodies_drop_the_model_and_unknown_config_keys() {
        let params = json!({
            "model": "gemini-2.5-flash",
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "config": {
                "temperature": 0.5,
                "maxOutputTokens": 16,
                "systemInstruction": "be brief",
                "tools": [{ "functionDeclarations": [] }],
                "toolConfig": { "functionCallingConfig": { "mode": "AUTO" } },
                "thinkingConfig": { "thinkingBudget": 0 },
                "bogusKey": true,
            },
        });
        let body = request_body(&params, false).expect("request body");
        assert_eq!(
            body,
            json!({
                "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
                "systemInstruction": { "role": "user", "parts": [{ "text": "be brief" }] },
                "tools": [{ "functionDeclarations": [] }],
                "toolConfig": { "functionCallingConfig": { "mode": "AUTO" } },
                "generationConfig": {
                    "temperature": 0.5,
                    "maxOutputTokens": 16,
                    "thinkingConfig": { "thinkingBudget": 0 },
                },
            })
        );
    }

    #[test]
    fn google_sse_decoder_splits_frames_and_raises_in_band_errors() {
        let mut decoder = GoogleSseDecoder::default();
        let chunks = decoder
            .push_bytes(b"data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n")
            .expect("decodes a frame");
        assert_eq!(chunks, vec![json!({ "candidates": [{ "finishReason": "STOP" }] })]);
        assert!(decoder.finish().expect("clean end").is_empty());

        let mut split = GoogleSseDecoder::default();
        assert!(split.push_bytes(b"data: {\"cand").expect("partial frame").is_empty());
        assert!(split.finish().is_err(), "an incomplete JSON segment must fail");

        let mut in_band = GoogleSseDecoder::default();
        let error = in_band
            .push_bytes(b"{\"error\":{\"code\":429,\"status\":\"RESOURCE_EXHAUSTED\"}}")
            .expect_err("an in-band error frame fails");
        assert_eq!(
            error.to_string(),
            "got status: RESOURCE_EXHAUSTED. {\"error\":{\"code\":429,\"status\":\"RESOURCE_EXHAUSTED\"}}"
        );
    }

    #[test]
    fn client_urls_follow_the_sdk_derivation() {
        let mldev = GoogleClientConfig {
            api_key: Some("test".into()),
            base_url: Some("https://generativelanguage.googleapis.com/v1beta".into()),
            api_version: Some(String::new()),
            ..GoogleClientConfig::default()
        };
        assert_eq!(
            mldev.stream_generate_content_url("gemini-2.5-flash"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );

        let vertex = GoogleClientConfig {
            vertexai: true,
            project: Some("test-project".into()),
            location: Some("us-central1".into()),
            api_version: Some("v1".into()),
            ..GoogleClientConfig::default()
        };
        assert_eq!(
            vertex.stream_generate_content_url("gemini-3-flash-preview"),
            "https://us-central1-aiplatform.googleapis.com/v1/projects/test-project/locations/us-central1/publishers/google/models/gemini-3-flash-preview:streamGenerateContent?alt=sse"
        );

        let collection = GoogleClientConfig {
            vertexai: true,
            project: Some("test-project".into()),
            location: Some("us-central1".into()),
            api_version: Some("v1".into()),
            base_url: Some("https://proxy.example.com".into()),
            base_url_resource_scope_collection: true,
            ..GoogleClientConfig::default()
        };
        assert_eq!(
            collection.stream_generate_content_url("gemini-3-flash-preview"),
            "https://proxy.example.com/v1/publishers/google/models/gemini-3-flash-preview:streamGenerateContent?alt=sse"
        );
    }
}
