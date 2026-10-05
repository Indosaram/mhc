use maho_ai::types::{InputModality, Model};
use maho_ext_api::CompactionResult;
use serde_json::{Value, json};
use crate::{openai_remote_convert::{convert_branch_entries, convert_pending_messages}, openai_remote_model::{is_openai_remote_compaction_model, openai_remote_compaction_identity}, openai_remote_schema::{OPENAI_REMOTE_COMPACTION_SCHEMA, is_openai_remote_compaction_output_item, is_retained_remote_output_item}};

pub const SENPI_COMPACTION_EVENT: &str = "senpi:compaction";

pub struct RemoteCompactionOptions<'a> {
    pub model: &'a Model,
    pub request: &'a OpenAiRemoteCompactionRequest,
    pub request_id: &'a str,
    pub first_kept_entry_id: &'a str,
    pub system_prompt: &'a str,
    pub session_id: &'a str,
    pub api_key: Option<String>,
    pub headers: std::collections::BTreeMap<String, String>,
    pub extra_body: Option<serde_json::Map<String, Value>>,
    pub origin: Value,
    pub signal: &'a maho_ai::utils::abort::AbortSignal,
    pub timeout: std::time::Duration,
    pub now_ms: u64,
    pub client: &'a reqwest::Client,
    pub runner: &'a crate::openai_remote_dependencies::OpenAiResponsesStreamRunner,
    pub provider_request: Option<&'a maho_ext_api::ProviderRequestPreparation>,
}

pub async fn run_remote_compaction(options: RemoteCompactionOptions<'_>, emit: &(dyn Fn(Value) + Sync)) -> Result<Option<CompactionResult>, String> {
    use crate::openai_remote_responses_v2 as v2;
    let stream_options = |signal|v2::ResponsesV2Options {
        model: options.model, request: options.request, first_kept_entry_id: options.first_kept_entry_id,
        origin: options.origin.clone(), system_prompt: options.system_prompt.into(), session_id: options.session_id.into(),
        api_key: options.api_key.clone(), headers: options.headers.clone(), extra_body: options.extra_body.clone(), signal, runner: options.runner,
    };
    if options.model.api == "openai-responses" && v2::supports_openai_responses_remote_compaction_v2(options.model)
        && let Some(result) = v2::attempt_openai_responses_v2_compaction(stream_options(options.signal.clone()), options.request_id, options.timeout, emit).await.map_err(|error|error.to_string())? { return Ok(Some(result)); }
    let event = |action: &str, transport: &str, fields: Value| {
        let mut event = json!({"version":1,"action":action,"route":"builtin.compaction.openai_remote","requestId":options.request_id,"modelId":options.model.id,"transport":transport});
        if let Some(fields) = fields.as_object() { for (key,value) in fields {event[key]=value.clone();} }
        emit(event);
    };
    if v2::supports_openai_responses_websocket(options.model) {
        event("remote_started","websocket",json!({"inputItemCount":options.request.input_item_count}));
        let result = crate::openai_remote_timeout::run_with_remote_timeout(options.signal,options.timeout,
            |signal|run_openai_responses_stream_compaction(stream_options(signal),options.now_ms),
            ||event("remote_fallback","websocket",json!({"reason":"remote-compaction-timeout"})),
            ||maho_ai::utils::event_stream::StreamError::new("aborted")).await;
        match result {
            Ok(Some(Some(result))) => {
                event("remote_completed","websocket",json!({"responseId":result.details.as_ref().map(|details|&details["responseId"]),"retainedInputItemCount":result.details.as_ref().map(|details|&details["retainedInputItemCount"])}));
                return Ok(Some(result));
            }
            Err(error) if options.signal.aborted() => return Err(error.to_string()),
            Err(error) => event("remote_fallback","websocket",json!({"reason":error.to_string()})),
            Ok(_) => event("remote_fallback","websocket",json!({"reason":"websocket-compaction-no-result"})),
        }
    }
    let body = match options.provider_request {
        Some(prepared) => (prepared.transform_payload)(options.request.body.clone()).await.map_err(|error|error.to_string())?,
        None => options.request.body.clone(),
    };
    if !body.is_object() || !body["model"].is_string() || !body["input"].as_array().is_some_and(|input|input.iter().all(Value::is_object)) {
        event("remote_fallback","compact-endpoint",json!({"reason":"invalid-compact-request-payload"}));
        return Ok(None);
    }
    let request = OpenAiRemoteCompactionRequest {body,input_item_count:options.request.input_item_count,tokens_before:options.request.tokens_before};
    let headers = options.headers.iter().map(|(key,value)|Ok((reqwest::header::HeaderName::from_bytes(key.as_bytes()).map_err(|error|error.to_string())?,reqwest::header::HeaderValue::from_str(value).map_err(|error|error.to_string())?)))
        .collect::<Result<reqwest::header::HeaderMap, String>>()?;
    crate::openai_remote_timeout::run_with_remote_timeout(options.signal,options.timeout, |signal|async move {
        run_openai_compact_endpoint_compaction(CompactEndpointOptions {client:options.client,headers,model:options.model,request:&request,request_id:options.request_id,signal:&signal,first_kept_entry_id:options.first_kept_entry_id,now_ms:options.now_ms,origin:options.origin.clone()},emit).await
    },||event("remote_fallback","compact-endpoint",json!({"reason":"remote-compaction-timeout"})),||"aborted".to_owned()).await.map(Option::flatten)
}

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

pub async fn run_openai_compact_endpoint_compaction(options: CompactEndpointOptions<'_>, emit: &(dyn Fn(Value) + Sync)) -> Result<Option<CompactionResult>, String> {
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
    let payload = tokio::select! {
        () = options.signal.cancelled() => return Err("aborted".into()),
        payload = response.json::<Value>() => payload,
    };
    let payload = match payload {
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

pub fn create_openai_responses_stream_compaction_payload(payload: &Value, request: &OpenAiRemoteCompactionRequest) -> Option<Value> {
    let mut payload = payload.as_object()?.clone();
    let mut input: Vec<_> = payload.get("input").and_then(Value::as_array).into_iter().flatten()
        .take_while(|item|matches!(item.get("role").and_then(Value::as_str), Some("system" | "developer")))
        .map(|item|crate::openai_remote_convert::provider_native_item(item).unwrap_or_else(||json!({"role":item["role"],"content":item.get("content").filter(|value|value.is_string()).cloned().unwrap_or(json!([]))}))).collect();
    input.extend(request.body["input"].as_array()?.iter().cloned());
    input.push(json!({"type":"context_compaction"}));
    payload.insert("input".into(),json!(input));
    payload.insert("model".into(),request.body["model"].clone());
    for key in ["prompt_cache_key","service_tier"] {
        if let Some(value) = request.body.get(key).filter(|value|value.as_str().is_some_and(|value|!value.is_empty())) { payload.insert(key.into(),value.clone()); }
    }
    Some(Value::Object(payload))
}

pub fn build_openai_responses_stream_compaction_result(model: &Model, first_kept_entry_id: &str, request: &OpenAiRemoteCompactionRequest, response: &maho_ai::types::AssistantMessage, now_ms: u64, origin: Option<Value>) -> Result<CompactionResult, &'static str> {
    let serialized = serde_json::to_value(response).expect("assistant messages serialize");
    let item = serialized["content"].as_array().into_iter().flatten().filter(|block|block["type"] == "providerNative")
        .filter_map(|block|crate::openai_remote_convert::provider_native_item(&block["raw"]))
        .find(crate::openai_remote_schema::is_openai_context_compaction_item).ok_or("OpenAI Responses stream compaction did not return a context_compaction item")?;
    let mut replacement: Vec<_> = request.body["input"].as_array().into_iter().flatten().filter(|item|crate::openai_remote_schema::is_retained_responses_stream_input_item(item)).cloned().collect();
    replacement.push(item);
    let mut details = json!({"schema":OPENAI_REMOTE_COMPACTION_SCHEMA,"mode":"openai-remote","provider":"openai","api":"openai-responses","transport":"websocket","modelId":model.id,"responseId":response.response_id.clone().unwrap_or_else(||format!("response-{now_ms}")),"createdAt":response.timestamp/1000,"requestInputItemCount":request.input_item_count,"retainedInputItemCount":replacement.len(),"replacementInput":replacement,"usage":{"input":response.usage.input,"output":response.usage.output,"cacheRead":response.usage.cache_read,"cacheWrite":response.usage.cache_write,"totalTokens":response.usage.total_tokens}});
    if let Some(origin) = origin { details["origin"] = origin; }
    Ok(CompactionResult { summary:format!("OpenAI remote compaction checkpoint.\nNative Responses WebSocket replay is active for {} retained item(s).\nOriginal OpenAI input items compacted: {}.",formatted_count(replacement.len()),formatted_count(request.input_item_count)), first_kept_entry_id:first_kept_entry_id.into(),tokens_before:request.tokens_before,details:Some(details) })
}

pub async fn run_openai_responses_stream_compaction(options: crate::openai_remote_responses_v2::ResponsesV2Options<'_>, now_ms: u64) -> Result<Option<CompactionResult>,maho_ai::utils::event_stream::StreamError> {
    let request_body = options.request.body.clone();
    let stream_options = maho_ai::types::SimpleStreamOptions { stream: maho_ai::types::StreamOptions {
        request: maho_ai::types::ProviderRequestOptions { api_key:options.api_key,headers:Some(options.headers.into_iter().map(|(key,value)|(key,Some(value))).collect()),signal:Some(options.signal),
            on_payload:Some(std::sync::Arc::new(move |payload,_,_|create_openai_responses_stream_compaction_payload(payload,&OpenAiRemoteCompactionRequest {body:request_body.clone(),input_item_count:0,tokens_before:0}))), ..Default::default() },
        cache_retention:Some(maho_ai::types::CacheRetention::Short),extra_body:options.extra_body,session_id:Some(options.session_id),transport:Some(maho_ai::types::Transport::Websocket),..Default::default()
    },..Default::default() };
    let context = maho_ai::types::Context {system_prompt:Some(options.system_prompt),messages:Vec::new(),tools:None};
    let response = (options.runner)(options.model,&context,&stream_options).result().await?;
    if matches!(response.stop_reason,maho_ai::types::StopReason::Error | maho_ai::types::StopReason::Aborted) { return Ok(None); }
    build_openai_responses_stream_compaction_result(options.model,options.first_kept_entry_id,options.request,&response,now_ms,Some(options.origin)).map(Some).map_err(maho_ai::utils::event_stream::StreamError::new)
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
