use std::collections::BTreeMap;
use maho_ai::types::Model;
use serde_json::Value;

pub fn with_remote_compaction_v2_header(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let key = headers.keys().find(|key| key.eq_ignore_ascii_case("x-codex-beta-features"));
    let mut tokens = Vec::new();
    if let Some(value) = key.and_then(|key| headers.get(key)) {
        for token in value.split(',').map(str::trim).filter(|token| !token.is_empty()) {
            if !tokens.contains(&token) { tokens.push(token); }
        }
    }
    if !tokens.contains(&"remote_compaction_v2") { tokens.push("remote_compaction_v2"); }
    let mut next = headers.clone();
    if let Some(key) = key { next.remove(key); }
    next.insert("x-codex-beta-features".into(), tokens.join(","));
    next
}

fn openai_hostname(model: &Model) -> bool {
    url::Url::parse(if model.base_url.is_empty() { "https://api.openai.com/v1" } else { &model.base_url }).is_ok_and(|url| url.host_str() == Some("api.openai.com"))
}

pub fn supports_openai_responses_remote_compaction_v2(model: &Model) -> bool {
    if let Some(value) = model.compat.as_ref().and_then(|compat| compat.get("supportsRemoteCompactionV2")).and_then(Value::as_bool) { return value; }
    model.provider == "openai" && openai_hostname(model)
}

pub fn supports_openai_responses_websocket(model: &Model) -> bool {
    if model.provider != "openai" || model.api != "openai-responses" { return false; }
    model.compat.as_ref().and_then(|compat| compat.get("supportsWebSocket")).and_then(Value::as_bool).unwrap_or_else(|| openai_hostname(model))
}

pub fn rewrite_v2_payload(payload: &Value, input: &[Value]) -> Option<Value> {
    let mut object = payload.as_object()?.clone();
    object.insert("input".into(), Value::Array(input.iter().cloned().chain(std::iter::once(serde_json::json!({"type":"compaction_trigger"}))).collect()));
    Some(Value::Object(object))
}

pub fn has_v2_trigger(payload: &Value) -> bool {
    payload.get("input").and_then(Value::as_array).is_some_and(|items| items.iter().any(|item| item.get("type").and_then(Value::as_str) == Some("compaction_trigger")))
}

pub fn responses_v2_result(
    model: &Model,
    response: &maho_ai::types::AssistantMessage,
    first_kept_entry_id: &str,
    request: &crate::openai_remote::OpenAiRemoteCompactionRequest,
    origin: Value,
) -> Option<maho_ext_api::CompactionResult> {
    if matches!(response.stop_reason, maho_ai::types::StopReason::Error | maho_ai::types::StopReason::Aborted) { return None; }
    let serialized = serde_json::to_value(response).expect("assistant messages serialize");
    let compaction = serialized["content"].as_array()?.iter()
        .filter(|block| block["type"] == "providerNative")
        .filter_map(|block| crate::openai_remote_convert::provider_native_item(&block["raw"]))
        .find(crate::openai_remote_schema::is_openai_compaction_item)?;
    let created_at = response.timestamp / 1000;
    let response_id = compaction.get("id").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| format!("compaction-{created_at}"));
    Some(maho_ext_api::CompactionResult {
        summary: "OpenAI remote compaction retained 1 native item (responses-v2).".into(),
        first_kept_entry_id: first_kept_entry_id.into(),
        tokens_before: request.tokens_before,
        details: Some(serde_json::json!({"schema":crate::openai_remote_schema::OPENAI_REMOTE_COMPACTION_SCHEMA,"mode":"openai-remote","provider":model.provider,"api":"openai-responses","transport":"responses-v2","modelId":model.id,"responseId":response_id,"createdAt":created_at,"requestInputItemCount":request.input_item_count,"retainedInputItemCount":1,"replacementInput":[compaction],"origin":origin})),
    })
}

pub struct ResponsesV2Options<'a> {
    pub model: &'a Model,
    pub request: &'a crate::openai_remote::OpenAiRemoteCompactionRequest,
    pub first_kept_entry_id: &'a str,
    pub origin: Value,
    pub system_prompt: String,
    pub session_id: String,
    pub api_key: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub extra_body: Option<serde_json::Map<String, Value>>,
    pub signal: maho_ai::utils::abort::AbortSignal,
    pub runner: &'a crate::openai_remote_dependencies::OpenAiResponsesStreamRunner,
}

pub async fn run_openai_responses_v2_compaction(options: ResponsesV2Options<'_>) -> Result<Option<maho_ext_api::CompactionResult>, maho_ai::utils::event_stream::StreamError> {
    let input = options.request.body["input"].as_array().expect("compact request input is an array").clone();
    let stream_options = maho_ai::types::SimpleStreamOptions {
        stream: maho_ai::types::StreamOptions {
            request: maho_ai::types::ProviderRequestOptions {
                api_key: options.api_key,
                signal: Some(options.signal),
                headers: Some(with_remote_compaction_v2_header(&options.headers).into_iter().map(|(key, value)| (key, Some(value))).collect()),
                on_payload: Some(std::sync::Arc::new(move |payload, _, _| rewrite_v2_payload(payload, &input))),
                ..Default::default()
            },
            cache_retention: Some(maho_ai::types::CacheRetention::Short),
            extra_body: options.extra_body,
            session_id: Some(options.session_id),
            transport: Some(maho_ai::types::Transport::Sse),
            ..Default::default()
        },
        ..Default::default()
    };
    let context = maho_ai::types::Context { system_prompt: Some(options.system_prompt), messages: Vec::new(), tools: None };
    let stream = (options.runner)(options.model, &context, &stream_options);
    let response = stream.result().await?;
    Ok(responses_v2_result(options.model, &response, options.first_kept_entry_id, options.request, options.origin))
}
