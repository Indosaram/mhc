use maho_ai::types::{Model, ModelThinkingLevel};
use serde_json::{Value, json};

pub fn summary_max_tokens(model: &Model, context_window: u64) -> u64 {
    let headroom = if model.max_tokens > 0 { model.max_tokens.min(32_768) } else { 32_768 };
    if context_window > 0 { headroom.min(context_window / 2) } else { headroom }
}

pub fn summarization_reasoning_options(model: &Model) -> Value {
    if !model.reasoning { return json!({}); }
    if model.api == "anthropic-messages" { return json!({"thinkingEnabled":false}); }
    let effort = [(ModelThinkingLevel::Low, "low"), (ModelThinkingLevel::Medium, "medium"), (ModelThinkingLevel::High, "high")]
        .into_iter().find(|(level, _)| !matches!(model.thinking_level_map.as_ref().and_then(|map| map.get(level)), Some(None)));
    let Some((_, effort)) = effort else { return json!({}); };
    match model.api.as_str() {
        "openai-responses" | "openai-codex-responses" | "azure-openai-responses" => json!({"reasoningEffort":effort,"reasoningSummary":null}),
        "openai-completions" => json!({"reasoningEffort":effort}),
        _ => json!({}),
    }
}

pub fn has_summarization_reasoning_override(model: &Model) -> bool {
    summarization_reasoning_options(model).as_object().is_some_and(|options| !options.is_empty())
}

pub fn get_summary_text(message: &Value) -> String {
    if let Some(text) = message.get("content").and_then(Value::as_str) { return text.trim().to_owned(); }
    message.get("content").and_then(Value::as_array).into_iter().flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n").trim().to_owned()
}

pub fn is_assistant_message(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("assistant") && message.get("stopReason").is_some()
}

#[derive(Debug)]
pub enum SummaryStreamError {
    Provider(maho_ai::utils::event_stream::StreamError),
    IdleTimeout,
    DurationBudget,
}

pub async fn consume_summary_stream(
    stream: &maho_ai::types::AssistantMessageEventStream,
    controller: &maho_ai::utils::abort::AbortController,
    caller_signal: Option<&maho_ai::utils::abort::AbortSignal>,
    idle_timeout: std::time::Duration,
    max_duration: std::time::Duration,
    on_progress: &(dyn Fn(&str) + Sync),
) -> Result<Option<maho_ai::types::AssistantMessage>, SummaryStreamError> {
    let idle = tokio::time::sleep(idle_timeout);
    let duration = tokio::time::sleep(max_duration);
    tokio::pin!(idle, duration);
    let cancelled = async {
        match caller_signal {
            Some(signal) => signal.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(cancelled);
    loop {
        tokio::select! {
            () = &mut cancelled => { controller.abort(None); return Ok(None); }
            () = &mut idle => { controller.abort(None); return Err(SummaryStreamError::IdleTimeout); }
            () = &mut duration => { controller.abort(None); return Err(SummaryStreamError::DurationBudget); }
            event = stream.next() => match event.map_err(SummaryStreamError::Provider)? {
                Some(event) => {
                    idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                    if let maho_ai::types::AssistantMessageEvent::TextDelta { delta, .. } = event
                        && !delta.is_empty() { on_progress(&delta); }
                }
                None => break,
            }
        }
    }
    tokio::select! {
        () = &mut cancelled => { controller.abort(None); Ok(None) }
        () = &mut idle => { controller.abort(None); Err(SummaryStreamError::IdleTimeout) }
        () = &mut duration => { controller.abort(None); Err(SummaryStreamError::DurationBudget) }
        result = stream.result() => result.map(Some).map_err(SummaryStreamError::Provider),
    }
}

pub type SummaryStreamRunner = std::sync::Arc<dyn Fn(&Model, &maho_ai::types::Context, maho_ai::types::StreamOptions) -> maho_ai::types::AssistantMessageEventStream + Send + Sync>;

pub struct SummaryRequestOptions<'a> {
    pub snapshot: &'a crate::speculative::SpeculativeCompactionSnapshot,
    pub messages: &'a [Value],
    pub prompt: &'a crate::prompts::CompactionPrompt,
    pub api_key: Option<String>,
    pub headers: Option<maho_ai::types::ProviderHeaders>,
    pub extra_body: Option<serde_json::Map<String, Value>>,
    pub signal: Option<&'a maho_ai::utils::abort::AbortSignal>,
    pub max_duration: std::time::Duration,
    pub omit_reasoning_options: bool,
    pub forbid_tool_calls: bool,
    pub stream_runner: Option<&'a SummaryStreamRunner>,
}

pub async fn generate_summary_message(options: SummaryRequestOptions<'_>, on_progress: &(dyn Fn(&str) + Sync)) -> Result<Option<maho_ai::types::AssistantMessage>, SummaryStreamError> {
    let mut messages = options.messages.to_vec();
    messages.push(json!({"role":"user","content":[{"type":"text","text":options.prompt.user}],"timestamp":chrono::Utc::now().timestamp_millis()}));
    let typed = maho_core::messages::convert_to_llm(&messages).into_iter().map(serde_json::from_value).collect::<Result<Vec<maho_ai::types::Message>, _>>()
        .map_err(|error| SummaryStreamError::Provider(maho_ai::utils::event_stream::StreamError::new(error.to_string())))?;
    let messages = crate::repair_tool_pairs::repair_orphaned_tool_results(&crate::summarization_turn_order::normalize_summarization_turn_order(&typed), chrono::Utc::now().timestamp_millis());
    let context = maho_ai::types::Context { system_prompt: Some(options.snapshot.system_prompt.as_deref().unwrap_or(options.prompt.system).into()), messages, tools: (!options.snapshot.tools.is_empty()).then(|| options.snapshot.tools.clone()) };
    let controller = maho_ai::utils::abort::AbortController::new();
    let mut stream_options = maho_ai::types::StreamOptions {
        max_tokens: Some(summary_max_tokens(&options.snapshot.model, options.snapshot.context_window)),
        extra_body: options.extra_body,
        request: maho_ai::types::ProviderRequestOptions { api_key: options.api_key, headers: options.headers, signal: Some(controller.signal()),
            on_payload: Some(std::sync::Arc::new(|payload, model, _| {
                if model.api == "anthropic-messages" {
                    payload.as_object().map(|payload| Value::Object(maho_ai::api::anthropic_tool_pairs::sanitize_anthropic_tool_pairs(payload)))
                } else { Some(payload.clone()) }
            })), ..Default::default() },
        ..Default::default()
    };
    if !options.omit_reasoning_options && let Some(reasoning) = summarization_reasoning_options(&options.snapshot.model).as_object() { stream_options.extra.extend(reasoning.clone()); }
    if options.forbid_tool_calls { stream_options.extra.insert("toolChoice".into(), json!("none")); }
    let stream = match options.stream_runner {
        Some(runner) => runner(&options.snapshot.model, &context, stream_options),
        None => maho_ai::compat::stream(&options.snapshot.model, &context, Some(stream_options)),
    };
    consume_summary_stream(&stream, &controller, options.signal, std::time::Duration::from_millis(300_000), options.max_duration, on_progress).await
}
