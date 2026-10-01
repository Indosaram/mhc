use maho_ai::types::{InputModality, Model};
use maho_ext_api::CompactionResult;
use serde_json::{Value, json};
use crate::{openai_remote_convert::{convert_branch_entries, convert_pending_messages}, openai_remote_model::{is_openai_remote_compaction_model, openai_remote_compaction_identity}, openai_remote_schema::{OPENAI_REMOTE_COMPACTION_SCHEMA, is_openai_remote_compaction_output_item, is_retained_remote_output_item}};

pub const SENPI_COMPACTION_EVENT: &str = "senpi:compaction";

pub struct CompactEndpointOptions<'a> {
    pub client: &'a reqwest::Client,
    pub headers: reqwest::header::HeaderMap,
    pub model: &'a Model,
    pub request: &'a OpenAiRemoteCompactionRequest,
    pub request_id: &'a str,
    pub signal: &'a maho_ai::utils::abort::AbortSignal,
    pub first_kept_entry_id: &'a str,
    pub now_ms: u64,
    pub origin: Value,
}

pub async fn run_openai_compact_endpoint_compaction(options: CompactEndpointOptions<'_>, emit: &dyn Fn(Value)) -> Result<Option<CompactionResult>, String> {
    let event = |action: &str, fields: Value| {
        let mut event = json!({"version":1,"action":action,"route":"builtin.compaction.openai_remote","requestId":options.request_id,"modelId":options.model.id,"transport":"compact-endpoint"});
        if let Some(fields) = fields.as_object() { for (key, value) in fields { event[key] = value.clone(); } }
        emit(event);
    };
    event("remote_started", json!({"inputItemCount":options.request.input_item_count}));
    let endpoint = crate::openai_remote_model::openai_remote_compaction_endpoint_url(options.model).map_err(|error| error.to_string())?;
    let response = tokio::select! {
        () = options.signal.cancelled() => return Err("aborted".into()),
        response = options.client.post(endpoint).headers(options.headers.clone()).json(&options.request.body).send() => response,
    };
    let response = match response {
        Ok(response) => response,
        Err(error) => { event("remote_fallback", json!({"reason":error.to_string()})); return Ok(None); }
    };
    if !response.status().is_success() {
        event("remote_fallback", json!({"reason":format!("HTTP {}", response.status().as_u16())})); return Ok(None);
    }
    let payload = match response.json::<Value>().await {
        Ok(payload) => payload,
        Err(error) => { event("remote_fallback", json!({"reason":error.to_string()})); return Ok(None); }
    };
    let Some(response) = parse_openai_compacted_response(&payload, options.model, options.request_id, options.now_ms) else {
        event("remote_fallback", json!({"reason":"invalid-compact-response"})); return Ok(None);
    };
    let result = match build_openai_remote_compaction_result(options.model, options.first_kept_entry_id, options.request, &response, Some(options.origin.clone())) {
        Ok(result) => result,
        Err(error) => { event("remote_fallback", json!({"reason":error})); return Ok(None); }
    };
    event("remote_completed", json!({"responseId":response["id"],"retainedInputItemCount":result.details.as_ref().map(|details| &details["retainedInputItemCount"])}));
    Ok(Some(result))
}

pub struct OpenAiRemoteCompactionRequest { pub body: Value, pub input_item_count: usize, pub tokens_before: u64 }

pub fn create_openai_remote_compaction_request(model: Option<&Model>, system_prompt: &str, branch_entries: &[Value], messages: Option<&[Value]>, tokens_before: u64, prompt_cache_key: Option<&str>, service_tier: Option<&str>) -> Option<OpenAiRemoteCompactionRequest> {
    if !is_openai_remote_compaction_model(model) { return None; }
    let model = model?;
    let images = model.input.contains(&InputModality::Image);
    let input = messages.map_or_else(|| convert_branch_entries(branch_entries, images), |messages| convert_pending_messages(messages, images));
    if input.is_empty() { return None; }
    let input_item_count = input.len();
    let mut body = json!({"model":model.id,"input":input});
    if !system_prompt.is_empty() { body["instructions"] = json!(system_prompt); }
    if let Some(key) = prompt_cache_key.filter(|key| !key.is_empty()) { body["prompt_cache_key"] = json!(key); }
    if let Some(tier) = service_tier.filter(|tier| !tier.is_empty()) { body["service_tier"] = json!(tier); }
    Some(OpenAiRemoteCompactionRequest { body, input_item_count, tokens_before })
}

pub fn parse_openai_compacted_response(value: &Value, model: &Model, request_id: &str, now_ms: u64) -> Option<Value> {
    let output = value.get("output")?.as_array()?;
    if model.api == "openai-responses" && (value.get("object").and_then(Value::as_str) != Some("response.compaction") || !value.get("id").is_some_and(Value::is_string) || !value.get("created_at").is_some_and(Value::is_number)) { return None; }
    let mut response = json!({"id":value.get("id").filter(|value| value.is_string()).cloned().unwrap_or_else(||json!(format!("codex-compact:{request_id}"))),"created_at":value.get("created_at").filter(|value| value.is_number()).cloned().unwrap_or_else(||json!(now_ms / 1000)),"object":"response.compaction","output":output.iter().filter(|value| value.is_object()).cloned().collect::<Vec<_>>()});
    if let Some(usage) = value.get("usage").filter(|value| value.is_object()) { response["usage"] = usage.clone(); }
    Some(response)
}

fn formatted_count(count: usize) -> String {
    let digits = count.to_string();
    digits.chars().enumerate().fold(String::new(), |mut output, (index, digit)| {
        if index > 0 && (digits.len() - index).is_multiple_of(3) { output.push(','); }
        output.push(digit); output
    })
}

pub fn build_openai_remote_compaction_result(model: &Model, first_kept_entry_id: &str, request: &OpenAiRemoteCompactionRequest, response: &Value, origin: Option<Value>) -> Result<CompactionResult, &'static str> {
    let replacement: Vec<_> = response.get("output").and_then(Value::as_array).into_iter().flatten().filter(|item| is_retained_remote_output_item(item)).cloned().collect();
    if !replacement.iter().any(is_openai_remote_compaction_output_item) { return Err("OpenAI remote compaction did not return a compaction item"); }
    let identity = openai_remote_compaction_identity(model);
    let mut details = json!({"schema":OPENAI_REMOTE_COMPACTION_SCHEMA,"mode":"openai-remote","provider":identity.provider,"api":identity.api,"transport":"compact-endpoint","modelId":model.id,"responseId":response["id"],"createdAt":response["created_at"],"requestInputItemCount":request.input_item_count,"retainedInputItemCount":replacement.len(),"replacementInput":replacement});
    if let Some(origin) = origin { details["origin"] = origin; }
    if let Some(usage) = response.get("usage") { details["usage"] = usage.clone(); }
    let path = if model.api == "openai-codex-responses" { "/codex/responses/compact" } else { "/v1/responses/compact" };
    Ok(CompactionResult { summary: format!("OpenAI remote compaction checkpoint.\nNative {path} replay is active for {} retained item(s).\nOriginal OpenAI input items compacted: {}.", formatted_count(replacement.len()), formatted_count(request.input_item_count)), first_kept_entry_id: first_kept_entry_id.into(), tokens_before: request.tokens_before, details: Some(details) })
}
