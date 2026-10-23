//! Port of senpi packages/ai/src/api/system-one-shared.ts.

use crate::types::{
    ClassifierAnswer, ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierQuestion,
    ClassifierResult, ClassifierStopReason, Model, ProviderHeaders, ProviderResponse, Usage,
};
use crate::utils::headers::{headers_to_record, merge_provider_headers_to_record};
use crate::utils::provider_retry::{
    ProviderErrorStatus, ProviderRequestError, ProviderRetryOptions, retry_provider_request,
};
use indexmap::IndexMap;
use serde_json::{Value, json};

pub trait SystemOneTransport: Send + Sync {
    fn api(&self) -> &str;
    fn label(&self) -> &str;
    fn url(&self, model: &ClassifierModel) -> String;
    fn payload(&self, model: &ClassifierModel, request: Value) -> Value;
    fn output(&self, body: Value) -> Result<Value, String>;
}

/// Shared request metadata for the existing auth/cost boundary. Classifiers remain separately typed.
pub(crate) fn request_model(model: &ClassifierModel) -> Model {
    Model {
        deprecated: None,
        id: model.id.clone(),
        name: model.name.clone(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        base_url: model.base_url.clone(),
        reasoning: false,
        default_thinking_level: None,
        thinking_level_map: None,
        prompt_cache: None,
        supports_assistant_prefill: None,
        input: model.input.clone(),
        cost: model.cost.clone(),
        context_window: model.context_window,
        max_tokens: 0,
        sampling_params: None,
        headers: model.headers.clone(),
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
        input_limits: model.input_limits.clone(),
    }
}

pub fn result(model: &ClassifierModel) -> ClassifierResult {
    ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: IndexMap::new(),
        usage: None,
        stop_reason: ClassifierStopReason::Stop,
        error_message: None,
        timestamp: crate::utils::diagnostics::now_ms(),
    }
}

pub fn fail(output: &mut ClassifierResult, options: &ClassifierOptions, error: String) {
    output.stop_reason = if options.request.signal.as_ref().is_some_and(|s| s.aborted()) {
        ClassifierStopReason::Aborted
    } else {
        ClassifierStopReason::Error
    };
    output.error_message = Some(error);
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct RequestError {
    message: String,
    status: Option<u16>,
    headers: Option<reqwest::header::HeaderMap>,
}

impl ProviderRequestError for RequestError {
    fn provider_status(&self) -> Option<ProviderErrorStatus<'_>> {
        self.status.map(|status| ProviderErrorStatus {
            status: Some(status),
            headers: self.headers.as_ref(),
        })
    }
}

pub(crate) async fn post(
    model: &ClassifierModel,
    options: &ClassifierOptions,
    url: &str,
    mut payload: Value,
    observe: bool,
    label: &str,
) -> Result<Value, String> {
    if observe
        && let Some(hook) = &options.on_payload
        && let Some(next) = hook(payload.clone(), model.clone()).await?
    {
        payload = next;
    }
    let request = &options.request;
    let mut defaults =
        ProviderHeaders::from([("content-type".into(), Some("application/json".into()))]);
    if let Some(key) = &request.api_key {
        defaults.insert("authorization".into(), Some(format!("Bearer {key}")));
    }
    let model_headers = model.headers.as_ref().map(|headers| {
        headers
            .iter()
            .map(|(k, v)| (k.clone(), Some(v.clone())))
            .collect()
    });
    let headers = merge_provider_headers_to_record(&[
        Some(&defaults),
        model_headers.as_ref(),
        request.headers.as_ref(),
    ])
    .unwrap_or_default();
    let client = request.fetch.clone().unwrap_or_default();
    let (body, response) = retry_provider_request(|| {
        let mut builder = client.post(url).json(&payload);
        for (name, value) in &headers { builder = builder.header(name, value); }
        async move {
            let operation = async {
                let response = builder.send().await.map_err(|e| RequestError { message: e.to_string(), status: None, headers: None })?;
                let status = response.status();
                let headers = response.headers().clone();
                let text = response.text().await.map_err(|e| RequestError { message: e.to_string(), status: None, headers: None })?;
                if !status.is_success() {
                    let normalized = crate::utils::error_body::normalize_provider_error(
                        &crate::utils::error_body::ThrownProviderError::Error(Box::new(
                            crate::utils::error_body::SdkErrorShape {
                                message: format!("{label} returned {}", status.as_u16()),
                                status: Some(json!(status.as_u16())),
                                headers: Some(headers.clone()),
                                body: Some(Value::String(text)),
                                ..Default::default()
                            },
                        )),
                    );
                    return Err(RequestError {
                        message: crate::utils::error_body::format_provider_error(&normalized, Some(&format!("{label} error"))),
                        status: Some(status.as_u16()), headers: Some(headers),
                    });
                }
                let body = serde_json::from_str(&text).map_err(|e| RequestError { message: e.to_string(), status: None, headers: None })?;
                Ok((body, ProviderResponse { status: status.as_u16(), headers: headers_to_record(&headers) }))
            };
            let bounded = async {
                match request.timeout_ms {
                    Some(ms) => tokio::time::timeout(std::time::Duration::from_millis(ms), operation).await
                        .map_err(|_| RequestError { message: format!("Request timed out after {ms}ms"), status: None, headers: None })?,
                    None => operation.await,
                }
            };
            match &request.signal {
                Some(signal) => tokio::select! {
                    biased;
                    () = signal.cancelled() => Err(RequestError { message: "Request aborted".into(), status: None, headers: None }),
                    output = bounded => output,
                },
                None => bounded.await,
            }
        }
    }, &ProviderRetryOptions { max_retries: Some(request.max_retries.unwrap_or(2)), max_retry_delay_ms: request.max_retry_delay_ms, signal: request.signal.clone() })
        .await.map_err(|e| e.to_string())?;
    if observe && let Some(hook) = &options.on_response {
        hook(response, model.clone()).await?;
    }
    Ok(body)
}

pub fn wire_request(context: &ClassifierContext) -> Result<Value, String> {
    let mut questions = serde_json::Map::new();
    for (id, question) in &context.questions {
        let mut value = serde_json::to_value(question).map_err(|e| e.to_string())?;
        if matches!(question, ClassifierQuestion::Bool { .. }) {
            value["type"] = json!("noul");
        }
        questions.insert(id.clone(), value);
    }
    Ok(json!({ "state": context.state, "questions": questions }))
}

fn number(value: &Value, field: &str, label: &str) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("{label} returned an invalid {field}"))
}

pub fn parse_answers(
    label: &str,
    value: &Value,
    context: &ClassifierContext,
) -> Result<IndexMap<String, ClassifierAnswer>, String> {
    if !value.is_object() {
        return Err(format!("{label} returned an unexpected response"));
    }
    context
        .questions
        .iter()
        .map(|(id, question)| {
            let answer = &value[id];
            if !answer.is_object() {
                return Err(format!("{label} did not return an answer for {id}"));
            }
            let invalid = || format!("{label} did not return a matching answer for {id}");
            let parsed = match question {
                ClassifierQuestion::Choice { .. } => {
                    if answer["type"] != "choice" {
                        return Err(invalid());
                    }
                    let choice = answer["choice"].as_str().ok_or_else(invalid)?.to_owned();
                    let probabilities = answer["probabilities"]
                        .as_object()
                        .ok_or_else(|| format!("{label} returned invalid probabilities for {id}"))?
                        .iter()
                        .map(|(key, value)| {
                            Ok((
                                key.clone(),
                                number(value, &format!("probability for {id}.{key}"), label)?,
                            ))
                        })
                        .collect::<Result<_, String>>()?;
                    ClassifierAnswer::Choice {
                        choice,
                        probabilities,
                        confidence: number(
                            &answer["confidence"],
                            &format!("confidence for {id}"),
                            label,
                        )?,
                    }
                }
                ClassifierQuestion::Score { .. } => {
                    if answer["type"] != "score" {
                        return Err(invalid());
                    }
                    ClassifierAnswer::Score {
                        score: number(&answer["score"], &format!("score for {id}"), label)?,
                        confidence: number(
                            &answer["confidence"],
                            &format!("confidence for {id}"),
                            label,
                        )?,
                    }
                }
                ClassifierQuestion::Bool { .. } => {
                    if answer["type"] != "noul" {
                        return Err(invalid());
                    }
                    ClassifierAnswer::Bool {
                        probability: number(
                            &answer["noul"],
                            &format!("probability for {id}"),
                            label,
                        )?,
                    }
                }
            };
            Ok((id.clone(), parsed))
        })
        .collect()
}

fn parse_usage(value: &Value, model: &ClassifierModel) -> Option<Usage> {
    if !value.is_object()
        || (value.get("input_tokens").is_none() && value.get("output_tokens").is_none())
    {
        return None;
    }
    let mut usage = Usage {
        input: value["input_tokens"].as_u64().unwrap_or(0),
        output: value["output_tokens"].as_u64().unwrap_or(0),
        ..Usage::default()
    };
    usage.total_tokens = usage.input.saturating_add(usage.output);
    crate::models::calculate_cost(&request_model(model), &mut usage);
    Some(usage)
}

pub async fn classify_system_one(
    transport: &dyn SystemOneTransport,
    model: &ClassifierModel,
    context: &ClassifierContext,
    options: Option<ClassifierOptions>,
) -> ClassifierResult {
    let options = options.unwrap_or_default();
    let mut output = result(model);
    let operation = async {
        if model.api != transport.api() {
            return Err(format!("Unsupported classifier API: {}", model.api));
        }
        if options
            .request
            .api_key
            .as_ref()
            .is_none_or(String::is_empty)
        {
            return Err(format!("No API key for provider: {}", model.provider));
        }
        let body = post(
            model,
            &options,
            &transport.url(model),
            transport.payload(model, wire_request(context)?),
            true,
            transport.label(),
        )
        .await?;
        let body = transport.output(body)?;
        output.usage = parse_usage(&body["usage"], model);
        output.answers = parse_answers(transport.label(), &body["answers"], context)?;
        Ok::<_, String>(())
    }
    .await;
    if let Err(error) = operation {
        fail(&mut output, &options, error);
    }
    output
}
