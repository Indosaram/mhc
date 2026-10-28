use crate::{
    openai_remote_convert::{convert_branch_entries, convert_pending_messages},
    openai_remote_model::{
        OpenAiRemoteCompactionIdentity, is_openai_remote_compaction_model,
        matches_openai_remote_compaction_identity, openai_remote_compaction_identity,
    },
    openai_remote_schema::{
        OPENAI_REMOTE_COMPACTION_SCHEMA, OpenAiRemoteCompactionDetails,
        get_openai_remote_compaction_details, is_openai_remote_compaction_output_item,
        is_retained_remote_output_item,
    },
};
use maho_ai::{
    context_provenance::{
        CONTEXT_PROVENANCE_FIELD, context_provenance_fingerprint, get_context_provenance,
    },
    types::{InputModality, Model},
};
use maho_core::{
    messages::convert_to_llm,
    session_manager::{build_context_entries, session_entry_to_context_messages},
};
use maho_ext_api::CompactionResult;
use serde_json::{Value, json};

pub const SENPI_COMPACTION_EVENT: &str = "senpi:compaction";
pub const OPENAI_REMOTE_REPLAY_BOUNDARY_SCOPE: &str = "openai-remote-replay";
pub const REMOTE_COMPACTION_TIMEOUT_MS: u64 = 15_000;
pub const CHATGPT_SUBSCRIPTION_REMOTE_COMPACTION_TIMEOUT_FLOOR_MS: u64 = 90_000;

pub fn openai_remote_compaction_timeout_ms(model: &Model, tokens: u64) -> u64 {
    let floor = if model.api == "openai-codex-responses" {
        CHATGPT_SUBSCRIPTION_REMOTE_COMPACTION_TIMEOUT_FLOOR_MS
    } else {
        REMOTE_COMPACTION_TIMEOUT_MS
    };
    floor.max(tokens.saturating_mul(2)).min(900_000)
}

pub fn openai_remote_compaction_uses_v2(model: &Model) -> bool {
    model.api == "openai-responses"
        && crate::openai_remote_responses_v2::supports_openai_responses_remote_compaction_v2(model)
}

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
    pub deadline: crate::openai_remote_timeout::RemoteCompactionDeadline,
    pub now_ms: u64,
    pub client: &'a reqwest::Client,
    pub runner: &'a crate::openai_remote_dependencies::OpenAiResponsesStreamRunner,
    pub provider_request: Option<&'a maho_ext_api::ProviderRequestPreparation>,
}

pub async fn run_remote_compaction(
    options: RemoteCompactionOptions<'_>,
    emit: &(dyn Fn(Value) + Sync),
) -> Result<Option<CompactionResult>, String> {
    use crate::openai_remote_responses_v2 as v2;
    let stream_options = |signal| v2::ResponsesV2Options {
        model: options.model,
        request: options.request,
        first_kept_entry_id: options.first_kept_entry_id,
        origin: options.origin.clone(),
        system_prompt: options.system_prompt.into(),
        session_id: options.session_id.into(),
        api_key: options.api_key.clone(),
        headers: options.headers.clone(),
        extra_body: options.extra_body.clone(),
        signal,
        runner: options.runner,
    };
    let compaction_timeout = v2::openai_remote_compaction_timeout(
        options.model,
        crate::overflow_retry::estimate_total_tokens(
            &options.request.body["input"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        ) as f64,
        options.timeout,
    );
    // One shared deadline across every attempt (senpi `createRemoteCompactionDeadline`).
    let deadline = options.deadline.with_attempt_budget(compaction_timeout);
    if (options.model.api == "openai-responses"
        && v2::supports_openai_responses_remote_compaction_v2(options.model))
        || options.model.api == "openai-codex-responses"
    {
        let result = v2::attempt_openai_responses_v2_compaction(
            stream_options(options.signal.clone()),
            options.request_id,
            tokio::time::Instant::now() + deadline.next_attempt(),
            emit,
        )
        .await
        .map_err(|error| error.to_string())?;
        if result.is_some() {
            return Ok(result);
        }
        if options.model.api == "openai-codex-responses" {
            return Ok(None);
        }
    }
    let event = |action: &str, transport: &str, fields: Value| {
        let mut event = json!({"version":1,"action":action,"route":"builtin.compaction.openai_remote","requestId":options.request_id,"modelId":options.model.id,"transport":transport});
        if let Some(fields) = fields.as_object() {
            for (key, value) in fields {
                event[key] = value.clone();
            }
        }
        emit(event);
    };
    let budget_exhausted = |transport: &str| {
        let exhausted = deadline.remaining().is_zero();
        if exhausted {
            event("remote_fallback", transport, json!({"reason":"remote-compaction-budget-exhausted"}));
        }
        exhausted
    };
    if v2::supports_openai_responses_websocket(options.model) && !budget_exhausted("websocket") {
        event(
            "remote_started",
            "websocket",
            json!({"inputItemCount":options.request.input_item_count}),
        );
        let result = crate::openai_remote_timeout::run_with_remote_timeout(
            options.signal,
            deadline.next_attempt(),
            |signal| run_openai_responses_stream_compaction(stream_options(signal), options.now_ms),
            || {
                event(
                    "remote_fallback",
                    "websocket",
                    json!({"reason":"remote-compaction-timeout"}),
                )
            },
            || maho_ai::utils::event_stream::StreamError::new("aborted"),
        )
        .await;
        match result {
            Ok(Some(Some(result))) => {
                event(
                    "remote_completed",
                    "websocket",
                    json!({"responseId":result.details.as_ref().map(|details|&details["responseId"]),"retainedInputItemCount":result.details.as_ref().map(|details|&details["retainedInputItemCount"])}),
                );
                return Ok(Some(result));
            }
            Err(error) if options.signal.aborted() => return Err(error.to_string()),
            Err(error) => event(
                "remote_fallback",
                "websocket",
                json!({"reason":error.to_string()}),
            ),
            Ok(_) => event(
                "remote_fallback",
                "websocket",
                json!({"reason":"websocket-compaction-no-result"}),
            ),
        }
    }
    let body = match options.provider_request {
        Some(prepared) => (prepared.transform_payload)(options.request.body.clone())
            .await
            .map_err(|error| error.to_string())?,
        None => options.request.body.clone(),
    };
    if !body.is_object()
        || !body["model"].is_string()
        || !body["input"]
            .as_array()
            .is_some_and(|input| input.iter().all(Value::is_object))
    {
        event(
            "remote_fallback",
            "compact-endpoint",
            json!({"reason":"invalid-compact-request-payload"}),
        );
        return Ok(None);
    }
    let request = OpenAiRemoteCompactionRequest {
        body,
        session_id: options.request.session_id.clone(),
        input_item_count: options.request.input_item_count,
        tokens_before: options.request.tokens_before,
    };
    if budget_exhausted("compact-endpoint") {
        return Ok(None);
    }
    let headers = options
        .headers
        .iter()
        .map(|(key, value)| {
            Ok((
                reqwest::header::HeaderName::from_bytes(key.as_bytes())
                    .map_err(|error| error.to_string())?,
                reqwest::header::HeaderValue::from_str(value).map_err(|error| error.to_string())?,
            ))
        })
        .collect::<Result<reqwest::header::HeaderMap, String>>()?;
    crate::openai_remote_timeout::run_with_remote_timeout(
        options.signal,
        deadline.next_attempt(),
        |signal| async move {
            run_openai_compact_endpoint_compaction(
                CompactEndpointOptions {
                    client: options.client,
                    headers,
                    model: options.model,
                    request: &request,
                    request_id: options.request_id,
                    signal: &signal,
                    first_kept_entry_id: options.first_kept_entry_id,
                    now_ms: options.now_ms,
                    origin: options.origin.clone(),
                },
                emit,
            )
            .await
        },
        || {
            event(
                "remote_fallback",
                "compact-endpoint",
                json!({"reason":"remote-compaction-timeout"}),
            )
        },
        || "aborted".to_owned(),
    )
    .await
    .map(Option::flatten)
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

pub async fn run_openai_compact_endpoint_compaction(
    options: CompactEndpointOptions<'_>,
    emit: &(dyn Fn(Value) + Sync),
) -> Result<Option<CompactionResult>, String> {
    let event = |action: &str, fields: Value| {
        let mut event = json!({"version":1,"action":action,"route":"builtin.compaction.openai_remote","requestId":options.request_id,"modelId":options.model.id,"transport":"compact-endpoint"});
        if let Some(fields) = fields.as_object() {
            for (key, value) in fields {
                event[key] = value.clone();
            }
        }
        emit(event);
    };
    event(
        "remote_started",
        json!({"inputItemCount":options.request.input_item_count}),
    );
    let endpoint = crate::openai_remote_model::openai_remote_compaction_endpoint_url(options.model)
        .map_err(|error| error.to_string())?;
    let response = tokio::select! {
        () = options.signal.cancelled() => return Err("aborted".into()),
        response = options.client.post(endpoint).headers(options.headers.clone()).json(&options.request.body).send() => response,
    };
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            event("remote_fallback", json!({"reason":error.to_string()}));
            return Ok(None);
        }
    };
    if !response.status().is_success() {
        event(
            "remote_fallback",
            json!({"reason":format!("HTTP {}", response.status().as_u16())}),
        );
        return Ok(None);
    }
    let payload = tokio::select! {
        () = options.signal.cancelled() => return Err("aborted".into()),
        payload = response.json::<Value>() => payload,
    };
    let payload = match payload {
        Ok(payload) => payload,
        Err(error) => {
            event("remote_fallback", json!({"reason":error.to_string()}));
            return Ok(None);
        }
    };
    let Some(response) = parse_openai_compacted_response(
        &payload,
        options.model,
        options.request_id,
        options.now_ms,
    ) else {
        event(
            "remote_fallback",
            json!({"reason":"invalid-compact-response"}),
        );
        return Ok(None);
    };
    let result = match build_openai_remote_compaction_result(
        options.model,
        options.first_kept_entry_id,
        options.request,
        &response,
        Some(options.origin.clone()),
    ) {
        Ok(result) => result,
        Err(error) => {
            event("remote_fallback", json!({"reason":error}));
            return Ok(None);
        }
    };
    event(
        "remote_completed",
        json!({"responseId":response["id"],"retainedInputItemCount":result.details.as_ref().map(|details| &details["retainedInputItemCount"])}),
    );
    Ok(Some(result))
}

pub struct OpenAiRemoteCompactionRequest {
    pub body: Value,
    pub session_id: Option<String>,
    pub input_item_count: usize,
    pub tokens_before: u64,
}

pub fn create_openai_remote_compaction_request(
    model: Option<&Model>,
    system_prompt: &str,
    branch_entries: &[Value],
    messages: Option<&[Value]>,
    tokens_before: u64,
    session_id: Option<&str>,
    service_tier: Option<&str>,
) -> Option<OpenAiRemoteCompactionRequest> {
    if !is_openai_remote_compaction_model(model) {
        return None;
    }
    let model = model?;
    let images = model.input.contains(&InputModality::Image);
    let input = messages.map_or_else(
        || convert_branch_entries(branch_entries, images),
        |messages| convert_pending_messages(messages, images),
    );
    if input.is_empty() {
        return None;
    }
    let input_item_count = input.len();
    let mut body = json!({"model":model.id,"input":input});
    if !system_prompt.is_empty() {
        body["instructions"] = json!(system_prompt);
    }
    let transcript = maho_ai::utils::transcript::normalize_context(&maho_ai::types::Context {
        system_prompt: (!system_prompt.is_empty()).then(|| system_prompt.to_owned()),
        messages: Vec::new(),
        tools: None,
        active_tool_names: None,
    });
    if let Some(key) =
        maho_ai::api::openai_prompt_cache::derive_openai_prompt_cache_key(&model.id, &transcript)
    {
        body["prompt_cache_key"] = json!(key);
    }
    if let Some(tier) = service_tier.filter(|tier| !tier.is_empty()) {
        body["service_tier"] = json!(tier);
    }
    Some(OpenAiRemoteCompactionRequest {
        body,
        session_id: session_id.filter(|id| !id.is_empty()).map(str::to_owned),
        input_item_count,
        tokens_before,
    })
}

pub fn parse_openai_compacted_response(
    value: &Value,
    model: &Model,
    request_id: &str,
    now_ms: u64,
) -> Option<Value> {
    let output = value.get("output")?.as_array()?;
    if model.api == "openai-responses"
        && (value.get("object").and_then(Value::as_str) != Some("response.compaction")
            || !value.get("id").is_some_and(Value::is_string)
            || !value.get("created_at").is_some_and(Value::is_number))
    {
        return None;
    }
    let mut response = json!({"id":value.get("id").filter(|value| value.is_string()).cloned().unwrap_or_else(||json!(format!("codex-compact:{request_id}"))),"created_at":value.get("created_at").filter(|value| value.is_number()).cloned().unwrap_or_else(||json!(now_ms / 1000)),"object":"response.compaction","output":output.iter().filter(|value| value.is_object()).cloned().collect::<Vec<_>>()});
    if let Some(usage) = value.get("usage").filter(|value| value.is_object()) {
        response["usage"] = usage.clone();
    }
    Some(response)
}

fn formatted_count(count: usize) -> String {
    let digits = count.to_string();
    digits
        .chars()
        .enumerate()
        .fold(String::new(), |mut output, (index, digit)| {
            if index > 0 && (digits.len() - index).is_multiple_of(3) {
                output.push(',');
            }
            output.push(digit);
            output
        })
}

pub fn create_openai_responses_stream_compaction_payload(
    payload: &Value,
    request: &OpenAiRemoteCompactionRequest,
) -> Option<Value> {
    let mut payload = payload.as_object()?.clone();
    let mut input: Vec<_> = payload.get("input").and_then(Value::as_array).into_iter().flatten()
        .take_while(|item|matches!(item.get("role").and_then(Value::as_str), Some("system" | "developer")))
        .map(|item|crate::openai_remote_convert::provider_native_item(item).unwrap_or_else(||json!({"role":item["role"],"content":item.get("content").filter(|value|value.is_string()).cloned().unwrap_or(json!([]))}))).collect();
    input.extend(request.body["input"].as_array()?.iter().cloned());
    input.push(json!({"type":"context_compaction"}));
    payload.insert("input".into(), json!(input));
    payload.insert("model".into(), request.body["model"].clone());
    for key in ["service_tier"] {
        if let Some(value) = request
            .body
            .get(key)
            .filter(|value| value.as_str().is_some_and(|value| !value.is_empty()))
        {
            payload.insert(key.into(), value.clone());
        }
    }
    Some(Value::Object(payload))
}

pub fn build_openai_responses_stream_compaction_result(
    model: &Model,
    first_kept_entry_id: &str,
    request: &OpenAiRemoteCompactionRequest,
    response: &maho_ai::types::AssistantMessage,
    now_ms: u64,
    origin: Option<Value>,
) -> Result<CompactionResult, &'static str> {
    let serialized = serde_json::to_value(response).expect("assistant messages serialize");
    let item = serialized["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "providerNative")
        .filter_map(|block| crate::openai_remote_convert::provider_native_item(&block["raw"]))
        .find(crate::openai_remote_schema::is_openai_context_compaction_item)
        .ok_or("OpenAI Responses stream compaction did not return a context_compaction item")?;
    let mut replacement: Vec<_> = request.body["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| crate::openai_remote_schema::is_retained_responses_stream_input_item(item))
        .cloned()
        .collect();
    replacement.push(item);
    let mut details = json!({"schema":OPENAI_REMOTE_COMPACTION_SCHEMA,"mode":"openai-remote","provider":"openai","api":"openai-responses","transport":"websocket","modelId":model.id,"responseId":response.response_id.clone().unwrap_or_else(||format!("response-{now_ms}")),"createdAt":response.timestamp/1000,"requestInputItemCount":request.input_item_count,"retainedInputItemCount":replacement.len(),"replacementInput":replacement,"usage":{"input":response.usage.input,"output":response.usage.output,"cacheRead":response.usage.cache_read,"cacheWrite":response.usage.cache_write,"totalTokens":response.usage.total_tokens}});
    if let Some(origin) = origin {
        details["origin"] = origin;
    }
    Ok(CompactionResult {
        summary: format!(
            "OpenAI remote compaction checkpoint.\nNative Responses WebSocket replay is active for {} retained item(s).\nOriginal OpenAI input items compacted: {}.",
            formatted_count(replacement.len()),
            formatted_count(request.input_item_count)
        ),
        first_kept_entry_id: first_kept_entry_id.into(),
        tokens_before: request.tokens_before,
        details: Some(details),
    })
}

pub async fn run_openai_responses_stream_compaction(
    options: crate::openai_remote_responses_v2::ResponsesV2Options<'_>,
    now_ms: u64,
) -> Result<Option<CompactionResult>, maho_ai::utils::event_stream::StreamError> {
    let request_body = options.request.body.clone();
    let stream_options = maho_ai::types::SimpleStreamOptions {
        stream: maho_ai::types::StreamOptions {
            request: maho_ai::types::ProviderRequestOptions {
                api_key: options.api_key,
                headers: Some(
                    options
                        .headers
                        .into_iter()
                        .map(|(key, value)| (key, Some(value)))
                        .collect(),
                ),
                signal: Some(options.signal.clone()),
                on_payload: Some(std::sync::Arc::new(move |payload, _, _| {
                    create_openai_responses_stream_compaction_payload(
                        payload,
                        &OpenAiRemoteCompactionRequest {
                            body: request_body.clone(),
                            session_id: None,
                            input_item_count: 0,
                            tokens_before: 0,
                        },
                    )
                })),
                ..Default::default()
            },
            cache_retention: Some(maho_ai::types::CacheRetention::Short),
            extra_body: options.extra_body,
            session_id: Some(
                options
                    .request
                    .session_id
                    .clone()
                    .unwrap_or_else(|| options.session_id.to_owned()),
            ),
            transport: Some(maho_ai::types::Transport::Websocket),
            ..Default::default()
        },
        ..Default::default()
    };
    let context = maho_ai::types::Context {
        active_tool_names: None,
        system_prompt: Some(options.system_prompt),
        messages: Vec::new(),
        tools: None,
    };
    let stream = (options.runner)(options.model, &context, &stream_options);
    let response = tokio::select! {
        biased;
        () = options.signal.cancelled() => {
            stream.cancel().await?;
            return Ok(None);
        }
        response = stream.result() => response,
    };
    stream.cancel().await?;
    let response = response?;
    if matches!(
        response.stop_reason,
        maho_ai::types::StopReason::Error | maho_ai::types::StopReason::Aborted
    ) {
        return Ok(None);
    }
    build_openai_responses_stream_compaction_result(
        options.model,
        options.first_kept_entry_id,
        options.request,
        &response,
        now_ms,
        Some(options.origin),
    )
    .map(Some)
    .map_err(maho_ai::utils::event_stream::StreamError::new)
}

pub fn build_openai_remote_compaction_result(
    model: &Model,
    first_kept_entry_id: &str,
    request: &OpenAiRemoteCompactionRequest,
    response: &Value,
    origin: Option<Value>,
) -> Result<CompactionResult, &'static str> {
    let replacement: Vec<_> = response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| is_retained_remote_output_item(item))
        .cloned()
        .collect();
    if !replacement
        .iter()
        .any(is_openai_remote_compaction_output_item)
    {
        return Err("OpenAI remote compaction did not return a compaction item");
    }
    let identity = openai_remote_compaction_identity(model);
    let mut details = json!({"schema":OPENAI_REMOTE_COMPACTION_SCHEMA,"mode":"openai-remote","provider":identity.provider,"api":identity.api,"transport":"compact-endpoint","modelId":model.id,"responseId":response["id"],"createdAt":response["created_at"],"requestInputItemCount":request.input_item_count,"retainedInputItemCount":replacement.len(),"replacementInput":replacement});
    if let Some(origin) = origin {
        details["origin"] = origin;
    }
    if let Some(usage) = response.get("usage") {
        details["usage"] = usage.clone();
    }
    let path = if model.api == "openai-codex-responses" {
        "/codex/responses/compact"
    } else {
        "/v1/responses/compact"
    };
    Ok(CompactionResult {
        summary: format!(
            "OpenAI remote compaction checkpoint.\nNative {path} replay is active for {} retained item(s).\nOriginal OpenAI input items compacted: {}.",
            formatted_count(replacement.len()),
            formatted_count(request.input_item_count)
        ),
        first_kept_entry_id: first_kept_entry_id.into(),
        tokens_before: request.tokens_before,
        details: Some(details),
    })
}

struct RemoteCompactionCheckpoint {
    entry_id: String,
    index: usize,
    details: OpenAiRemoteCompactionDetails,
}

fn latest_remote_compaction(entries: &[Value]) -> Option<RemoteCompactionCheckpoint> {
    let (index, entry) = entries
        .iter()
        .enumerate()
        .rev()
        .find(|(_, entry)| entry.get("type").and_then(Value::as_str) == Some("compaction"))?;
    let details = entry
        .get("details")
        .and_then(get_openai_remote_compaction_details)?;
    Some(RemoteCompactionCheckpoint {
        entry_id: entry.get("id")?.as_str()?.into(),
        index,
        details,
    })
}

fn checkpoint_context_entries(
    entries: &[Value],
    remote: &RemoteCompactionCheckpoint,
) -> Vec<Value> {
    let checkpoint_entry_ids: std::collections::HashSet<&str> = entries
        .iter()
        .take(remote.index + 1)
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .collect();
    build_context_entries(entries, None)
        .into_iter()
        .filter(|entry| {
            entry
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| checkpoint_entry_ids.contains(id))
        })
        .collect()
}

/// The replay boundary [`mark_openai_remote_replay_boundary`] stamped onto the checkpoint-owned
/// messages: the remote compaction entry and the ordinals of the stamped messages that survive
/// the LLM projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAiRemoteReplayBoundary {
    pub compaction_entry_id: String,
    pub expected_ordinals: Vec<usize>,
}

/// senpi `markOpenAiRemoteReplayBoundary`: mark the exact checkpoint-owned messages before later
/// context hooks run. `entry_ids` is the typed session-entry stamp aligned with `messages` (senpi's
/// `SESSION_CONTEXT_ENTRY_ID`). The markers carry entry identity, not payload values. senpi seals
/// them by converting to Responses input items; the native Responses converter cannot carry
/// provenance per item, so the sealed baseline is the `convert_to_llm` projection, which does.
pub fn mark_openai_remote_replay_boundary(
    messages: Vec<Value>,
    entry_ids: &[Option<String>],
    model: Option<&Model>,
    branch_entries: &[Value],
) -> (Vec<Value>, Option<OpenAiRemoteReplayBoundary>) {
    let Some(model) = model.filter(|model| is_openai_remote_compaction_model(Some(*model))) else {
        return (messages, None);
    };
    let Some(remote) = latest_remote_compaction(branch_entries) else {
        return (messages, None);
    };
    let identity = OpenAiRemoteCompactionIdentity {
        provider: remote.details.provider.clone(),
        api: remote.details.api.clone(),
    };
    if remote.details.origin.is_none()
        || !matches_openai_remote_compaction_identity(model, &identity)
    {
        return (messages, None);
    }
    let checkpoint_ids: Vec<String> = checkpoint_context_entries(branch_entries, &remote)
        .iter()
        .filter(|entry| !session_entry_to_context_messages(entry).is_empty())
        .filter_map(|entry| entry.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    if checkpoint_ids.is_empty()
        || messages.len() < checkpoint_ids.len()
        || !checkpoint_ids
            .iter()
            .enumerate()
            .all(|(index, id)| entry_ids.get(index).and_then(Option::as_deref) == Some(id.as_str()))
    {
        return (messages, None);
    }
    let mark = |expected_ordinals: &[usize],
                integrity: &std::collections::BTreeMap<usize, String>|
     -> Vec<Value> {
        messages.iter().enumerate().map(|(index, message)| {
            let mut marked = message.clone();
            if index < checkpoint_ids.len() && let Some(object) = marked.as_object_mut() {
                let mut boundary = json!({"scope":OPENAI_REMOTE_REPLAY_BOUNDARY_SCOPE,"compactionEntryId":remote.entry_id,"ordinal":index,"expectedOrdinals":expected_ordinals});
                if let Some(integrity) = integrity.get(&index) { boundary["integrity"] = json!(integrity); }
                object.insert(CONTEXT_PROVENANCE_FIELD.into(), boundary);
            }
            marked
        }).collect()
    };
    let mut expected_ordinals = Vec::new();
    let mut integrity = std::collections::BTreeMap::new();
    for item in convert_to_llm(&mark(&[], &integrity)) {
        let Some(boundary) = get_context_provenance(&item) else {
            continue;
        };
        if boundary.get("scope").and_then(Value::as_str)
            != Some(OPENAI_REMOTE_REPLAY_BOUNDARY_SCOPE)
            || boundary.get("compactionEntryId").and_then(Value::as_str)
                != Some(remote.entry_id.as_str())
        {
            continue;
        }
        let Some(ordinal) = boundary
            .get("ordinal")
            .and_then(Value::as_u64)
            .and_then(|ordinal| usize::try_from(ordinal).ok())
        else {
            continue;
        };
        if let Some(fingerprint) = context_provenance_fingerprint(&item) {
            integrity.insert(ordinal, fingerprint);
        }
        expected_ordinals.push(ordinal);
    }
    if expected_ordinals.is_empty() {
        return (messages, None);
    }
    let marked = mark(&expected_ordinals, &integrity);
    (
        marked,
        Some(OpenAiRemoteReplayBoundary {
            compaction_entry_id: remote.entry_id,
            expected_ordinals,
        }),
    )
}

#[cfg(test)]
mod replay_boundary_tests {
    use super::*;

    fn model() -> Model {
        serde_json::from_value(json!({"id":"m","name":"m","provider":"openai","api":"openai-responses","baseUrl":"https://api.openai.com/v1","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":8000})).expect("model fixture")
    }

    fn branch() -> Vec<Value> {
        let details = json!({"schema":OPENAI_REMOTE_COMPACTION_SCHEMA,"mode":"openai-remote","provider":"openai","api":"openai-responses","transport":"compact-endpoint","modelId":"m","responseId":"r","createdAt":1,"requestInputItemCount":2,"retainedInputItemCount":1,"replacementInput":[{"type":"compaction","encrypted_content":"x"}],"origin":{"endpoint":"https://api.openai.com/v1","trustDomain":"https://api.openai.com","authTenantFingerprint":"sha256:abc"}});
        vec![
            json!({"type":"message","id":"a","parentId":null,"timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"old","timestamp":0}}),
            json!({"type":"message","id":"b","parentId":"a","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"kept","timestamp":0}}),
            json!({"type":"compaction","id":"c","parentId":"b","timestamp":"1970-01-01T00:00:00.000Z","summary":"s","firstKeptEntryId":"b","tokensBefore":5,"details":details}),
            json!({"type":"message","id":"d","parentId":"c","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"new","timestamp":0}}),
        ]
    }

    fn context(branch: &[Value]) -> (Vec<Value>, Vec<Option<String>>) {
        build_context_entries(branch, None)
            .iter()
            .flat_map(|entry| {
                session_entry_to_context_messages(entry)
                    .into_iter()
                    .map(|message| (message, entry["id"].as_str().map(str::to_owned)))
            })
            .unzip()
    }

    #[test]
    fn marks_checkpoint_owned_prefix_with_entry_identity() {
        let branch = branch();
        let (messages, entry_ids) = context(&branch);
        let (marked, boundary) =
            mark_openai_remote_replay_boundary(messages, &entry_ids, Some(&model()), &branch);
        assert_eq!(
            boundary,
            Some(OpenAiRemoteReplayBoundary {
                compaction_entry_id: "c".into(),
                expected_ordinals: vec![0, 1]
            })
        );
        assert_eq!(marked[0][CONTEXT_PROVENANCE_FIELD]["ordinal"], 0);
        assert_eq!(
            marked[1][CONTEXT_PROVENANCE_FIELD]["expectedOrdinals"],
            json!([0, 1])
        );
        assert!(marked[1][CONTEXT_PROVENANCE_FIELD]["integrity"].is_string());
        assert!(marked[2].get(CONTEXT_PROVENANCE_FIELD).is_none());
    }

    #[test]
    fn misaligned_entry_ids_leave_messages_unmarked() {
        let branch = branch();
        let (messages, mut entry_ids) = context(&branch);
        entry_ids[1] = None;
        assert_eq!(
            mark_openai_remote_replay_boundary(
                messages.clone(),
                &entry_ids,
                Some(&model()),
                &branch
            ),
            (messages, None)
        );
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    #[test]
    fn timeout_scales_with_input_and_respects_a_hard_cap() {
        let model: Model = serde_json::from_value(json!({"id":"m","name":"m","provider":"openai","api":"openai-responses","baseUrl":"https://api.openai.com/v1","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":200000,"maxTokens":10000})).expect("model fixture");
        assert_eq!(openai_remote_compaction_timeout_ms(&model, 1), 15_000);
        assert_eq!(openai_remote_compaction_timeout_ms(&model, 20_000), 40_000);
        assert_eq!(openai_remote_compaction_timeout_ms(&model, 300_000), 600_000);
        assert_eq!(
            openai_remote_compaction_timeout_ms(&model, u64::MAX),
            900_000
        );
    }
}
