//! Port of senpi packages/ai/src/api/simple-options.ts (the subset `openai-completions.ts` uses).
//!
//! Node-private copy: `src/api/simple_options.rs` is owned by node 12-misc. Kept here so this lane
//! compiles standalone; delete it once 12-misc merges.

use super::context_room::{clamp_max_tokens_to_context, ContextWindowExhaustedError, MIN_ANSWER_TOKENS};
use crate::model::Model;
use crate::types::{Context, ModelThinkingLevel, SimpleStreamOptions, StreamOptions, ThinkingBudgets};
use serde_json::{Map, Value};

pub fn apply_extra_body(
    target: &mut Map<String, Value>,
    extra_body: Option<&Map<String, Value>>,
    reserved_keys: &[&str],
) {
    let Some(extra_body) = extra_body else { return };
    for (key, value) in extra_body {
        if reserved_keys.contains(&key.as_str()) {
            continue;
        }
        target.insert(key.clone(), value.clone());
    }
}

pub const OPENAI_COMPLETIONS_RESERVED_BODY_KEYS: [&str; 18] = [
    "model",
    "messages",
    "stream",
    "stream_options",
    "tools",
    "tool_choice",
    "store",
    "temperature",
    "max_tokens",
    "max_completion_tokens",
    "reasoning_effort",
    "reasoning",
    "thinking",
    "enable_thinking",
    "chat_template_kwargs",
    "tool_stream",
    "provider",
    "providerOptions",
];

pub fn build_base_options(
    model: &Model,
    context: &Context,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
) -> Result<StreamOptions, ContextWindowExhaustedError> {
    let sampling_params = if model.sampling_params.is_some() || options.is_some_and(|o| o.stream.sampling_params.is_some())
    {
        let mut merged = model.sampling_params.clone().unwrap_or_default();
        if let Some(extra) = options.and_then(|options| options.stream.sampling_params.as_ref()) {
            for (key, value) in extra {
                merged.insert(key.clone(), value.clone());
            }
        }
        Some(merged)
    } else {
        None
    };

    let max_tokens = options.and_then(|options| options.stream.max_tokens).unwrap_or(model.max_tokens);
    let mut stream = StreamOptions {
        temperature: options.and_then(|options| options.stream.temperature),
        sampling_params,
        max_tokens: Some(clamp_max_tokens_to_context(model, context, max_tokens)?),
        transport: options.and_then(|options| options.stream.transport),
        cache_retention: options.and_then(|options| options.stream.cache_retention).or(model.cache_retention),
        session_id: options.and_then(|options| options.stream.session_id.clone()),
        extra_body: options.and_then(|options| options.stream.extra_body.clone()),
        websocket_connect_timeout_ms: options.and_then(|options| options.stream.websocket_connect_timeout_ms),
        metadata: options.and_then(|options| options.stream.metadata.clone()),
        ..StreamOptions::default()
    };
    let request = &mut stream.request;
    request.signal = options.and_then(|options| options.stream.request.signal.clone());
    request.abort_server_side_fallback = options.and_then(|options| options.stream.request.abort_server_side_fallback);
    request.telemetry_context = options.and_then(|options| options.stream.request.telemetry_context.clone());
    request.api_key = api_key
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .or_else(|| options.and_then(|options| options.stream.request.api_key.clone()));
    request.fetch = options.and_then(|options| options.stream.request.fetch.clone());
    request.headers = options.and_then(|options| options.stream.request.headers.clone());
    request.on_payload = options.and_then(|options| options.stream.request.on_payload.clone());
    request.on_response = options.and_then(|options| options.stream.request.on_response.clone());
    request.timeout_ms = options.and_then(|options| options.stream.request.timeout_ms);
    request.max_retries = options.and_then(|options| options.stream.request.max_retries);
    request.max_retry_delay_ms = options.and_then(|options| options.stream.request.max_retry_delay_ms);
    request.env = options.and_then(|options| options.stream.request.env.clone());
    Ok(stream)
}

pub const DEFAULT_THINKING_BUDGETS: ThinkingBudgets =
    ThinkingBudgets { minimal: Some(1024), low: Some(2048), medium: Some(8192), high: Some(16384), max: None };

fn clamp_reasoning(effort: ModelThinkingLevel) -> ModelThinkingLevel {
    match effort {
        ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max => ModelThinkingLevel::High,
        other => other,
    }
}

pub fn clamp_max_for_openai(effort: ModelThinkingLevel, xhigh_supported: bool) -> ModelThinkingLevel {
    match effort {
        ModelThinkingLevel::Max => {
            if xhigh_supported {
                ModelThinkingLevel::Xhigh
            } else {
                ModelThinkingLevel::High
            }
        }
        other => other,
    }
}

pub fn thinking_budget_for_level(
    reasoning_level: ModelThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> u64 {
    let mut budgets = DEFAULT_THINKING_BUDGETS;
    if let Some(custom) = custom_budgets {
        budgets.minimal = custom.minimal.or(budgets.minimal);
        budgets.low = custom.low.or(budgets.low);
        budgets.medium = custom.medium.or(budgets.medium);
        budgets.high = custom.high.or(budgets.high);
        budgets.max = custom.max.or(budgets.max);
    }
    let level = clamp_reasoning(reasoning_level);
    match level {
        ModelThinkingLevel::Minimal => budgets.minimal,
        ModelThinkingLevel::Low => budgets.low,
        ModelThinkingLevel::Medium => budgets.medium,
        ModelThinkingLevel::High => budgets.high,
        ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max => budgets.max,
        ModelThinkingLevel::Off => None,
    }
    .unwrap_or_default()
}

pub fn clamp_thinking_budget_to_answer_room(thinking_budget: u64, ceiling: u64) -> u64 {
    thinking_budget.min(ceiling.saturating_sub(MIN_ANSWER_TOKENS))
}
