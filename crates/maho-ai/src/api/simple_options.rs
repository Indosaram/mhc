//! Port of senpi packages/ai/src/api/simple-options.ts.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use serde_json::{Map, Value};

use crate::api::context_room::ContextWindowExhaustedError;
use crate::types::{
    Context, Model, ProviderRequestOptions, SimpleStreamOptions, StreamOptions, ThinkingBudgets, ThinkingLevel,
};

pub use crate::api::context_room::{
    clamp_max_tokens_to_context, ContextWindowExhaustedError as ContextGuardError, CONTEXT_GUARD_MIN_WINDOW,
    CONTEXT_SAFETY_TOKENS, MIN_ANSWER_TOKENS,
};

/// Merge user-supplied extraBody fields into a provider request payload, skipping any key the
/// provider manages itself (model id, messages, stream flag, etc.).
pub fn apply_extra_body(
    target: &mut Map<String, Value>,
    extra_body: Option<&Map<String, Value>>,
    reserved_keys: &BTreeSet<&'static str>,
) {
    let Some(extra_body) = extra_body else { return };
    for (key, value) in extra_body {
        if reserved_keys.contains(key.as_str()) {
            continue;
        }
        target.insert(key.clone(), value.clone());
    }
}

fn reserved(keys: &[&'static str]) -> BTreeSet<&'static str> {
    keys.iter().copied().collect()
}

pub static OPENAI_COMPLETIONS_RESERVED_BODY_KEYS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    reserved(&[
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
    ])
});

pub static OPENAI_RESPONSES_RESERVED_BODY_KEYS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    reserved(&[
        "model",
        "input",
        "instructions",
        "stream",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "reasoning",
        "max_output_tokens",
        "temperature",
        "text",
        "store",
        "include",
        "prompt_cache_key",
        "prompt_cache_retention",
        "service_tier",
    ])
});

/// Reserved keys for the inner Google `config` object (serialized as the request body's
/// generationConfig/tools/systemInstruction by the @google/genai SDK). extraBody is merged into
/// `config`, not the top-level GenerateContentParameters.
pub static GOOGLE_RESERVED_BODY_KEYS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    reserved(&[
        "systemInstruction",
        "tools",
        "toolConfig",
        "temperature",
        "maxOutputTokens",
        "thinkingConfig",
        "responseMimeType",
        "responseSchema",
        "cachedContent",
        "abortSignal",
        "httpOptions",
    ])
});

pub static ANTHROPIC_RESERVED_BODY_KEYS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    reserved(&[
        "model",
        "messages",
        "system",
        "stream",
        "tools",
        "tool_choice",
        "temperature",
        "max_tokens",
        "thinking",
        "output_config",
        "metadata",
    ])
});

pub static MISTRAL_RESERVED_BODY_KEYS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    reserved(&["model", "messages", "stream", "tools", "toolChoice", "temperature", "maxTokens", "promptMode"])
});

pub static BEDROCK_RESERVED_BODY_KEYS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    reserved(&[
        "modelId",
        "messages",
        "system",
        "toolConfig",
        "additionalModelRequestFields",
        "inferenceConfig",
        "requestMetadata",
    ])
});

fn merge_sampling_params(
    model: &Model,
    options: Option<&SimpleStreamOptions>,
) -> Option<Map<String, Value>> {
    let option_params = options.and_then(|options| options.stream.sampling_params.as_ref());
    if model.sampling_params.is_none() && option_params.is_none() {
        return None;
    }
    let mut merged = model.sampling_params.clone().unwrap_or_default();
    if let Some(option_params) = option_params {
        for (key, value) in option_params {
            merged.insert(key.clone(), value.clone());
        }
    }
    Some(merged)
}

pub fn build_base_options(
    model: &Model,
    context: &Context,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
) -> Result<StreamOptions, ContextWindowExhaustedError> {
    let stream = options.map(|options| &options.stream);
    let sampling_params = merge_sampling_params(model, options);
    let max_tokens = clamp_max_tokens_to_context(
        model,
        context,
        stream.and_then(|stream| stream.max_tokens).unwrap_or(model.max_tokens),
    )?;
    let resolved_api_key = api_key
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .or_else(|| stream.and_then(|stream| stream.request.api_key.clone()));
    let request = ProviderRequestOptions {
        signal: stream.and_then(|stream| stream.request.signal.clone()),
        abort_server_side_fallback: stream.and_then(|stream| stream.request.abort_server_side_fallback),
        telemetry_context: stream.and_then(|stream| stream.request.telemetry_context.clone()),
        api_key: resolved_api_key,
        affinity_session_id: None,
        stream_kind: stream.and_then(|stream| stream.request.stream_kind),
        fetch: stream.and_then(|stream| stream.request.fetch.clone()),
        env: stream.and_then(|stream| stream.request.env.clone()),
        on_payload: stream.and_then(|stream| stream.request.on_payload.clone()),
        on_response: stream.and_then(|stream| stream.request.on_response.clone()),
        async_on_payload: stream.and_then(|stream| stream.request.async_on_payload.clone()),
        async_on_response: stream.and_then(|stream| stream.request.async_on_response.clone()),
        headers: stream.and_then(|stream| stream.request.headers.clone()),
        timeout_ms: stream.and_then(|stream| stream.request.timeout_ms),
        max_retries: stream.and_then(|stream| stream.request.max_retries),
        max_retry_delay_ms: stream.and_then(|stream| stream.request.max_retry_delay_ms),
    };
    Ok(StreamOptions {
        request,
        temperature: stream.and_then(|stream| stream.temperature),
        sampling_params,
        max_tokens: Some(max_tokens),
        transport: stream.and_then(|stream| stream.transport),
        cache_retention: stream
            .and_then(|stream| stream.cache_retention)
            .or(model.cache_retention),
        session_id: stream.and_then(|stream| stream.session_id.clone()),
        extra_body: stream.and_then(|stream| stream.extra_body.clone()),
        websocket_connect_timeout_ms: stream.and_then(|stream| stream.websocket_connect_timeout_ms),
        metadata: stream.and_then(|stream| stream.metadata.clone()),
        extra: Map::new(),
    })
}

pub fn default_thinking_budgets() -> ThinkingBudgets {
    ThinkingBudgets { minimal: Some(1024), low: Some(2048), medium: Some(8192), high: Some(16384), max: None }
}

pub fn clamp_reasoning(effort: ThinkingLevel) -> Option<ThinkingLevel> {
    Some(match effort {
        ThinkingLevel::Xhigh | ThinkingLevel::Max => ThinkingLevel::High,
        other => other,
    })
}

/// Clamp "max" to an OpenAI-compatible effort. OpenAI-style reasoning APIs accept
/// low|medium|high|xhigh but not "max"; "max" downgrades to "xhigh" on xhigh-capable models and
/// to "high" otherwise.
pub fn clamp_max_for_openai(effort: ThinkingLevel, xhigh_supported: bool) -> Option<ThinkingLevel> {
    Some(match effort {
        ThinkingLevel::Max => {
            if xhigh_supported {
                ThinkingLevel::Xhigh
            } else {
                ThinkingLevel::High
            }
        }
        other => other,
    })
}

fn budget_for_level(budgets: &ThinkingBudgets, level: ThinkingLevel) -> u64 {
    match level {
        ThinkingLevel::Minimal => budgets.minimal,
        ThinkingLevel::Low => budgets.low,
        ThinkingLevel::Medium => budgets.medium,
        ThinkingLevel::High => budgets.high,
        ThinkingLevel::Xhigh | ThinkingLevel::Max => None,
    }
    .unwrap_or(0)
}

pub fn thinking_budget_for_level(reasoning_level: ThinkingLevel, custom_budgets: Option<&ThinkingBudgets>) -> u64 {
    let mut budgets = default_thinking_budgets();
    if let Some(custom) = custom_budgets {
        if custom.minimal.is_some() {
            budgets.minimal = custom.minimal;
        }
        if custom.low.is_some() {
            budgets.low = custom.low;
        }
        if custom.medium.is_some() {
            budgets.medium = custom.medium;
        }
        if custom.high.is_some() {
            budgets.high = custom.high;
        }
        if custom.max.is_some() {
            budgets.max = custom.max;
        }
    }
    let level = clamp_reasoning(reasoning_level).unwrap_or(reasoning_level);
    budget_for_level(&budgets, level)
}

/// Cap a thinking budget so at least [`MIN_ANSWER_TOKENS`] remain under a shared response ceiling.
pub fn clamp_thinking_budget_to_answer_room(thinking_budget: u64, ceiling: u64) -> u64 {
    thinking_budget.min(ceiling.saturating_sub(MIN_ANSWER_TOKENS))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdjustedThinking {
    pub max_tokens: u64,
    pub thinking_budget: u64,
}

pub fn adjust_max_tokens_for_thinking(
    base_max_tokens: Option<u64>,
    model_max_tokens: u64,
    reasoning_level: ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> AdjustedThinking {
    // An absent/unresolvable level means "no thinking", not a NaN budget.
    let Some(level) = clamp_reasoning(reasoning_level) else {
        return AdjustedThinking { max_tokens: base_max_tokens.unwrap_or(model_max_tokens), thinking_budget: 0 };
    };
    let mut thinking_budget = thinking_budget_for_level(level, custom_budgets);
    let max_tokens = match base_max_tokens {
        None => model_max_tokens,
        Some(base) => (base + thinking_budget).min(model_max_tokens),
    };

    if max_tokens <= thinking_budget {
        thinking_budget = clamp_thinking_budget_to_answer_room(thinking_budget, max_tokens);
    }

    AdjustedThinking { max_tokens, thinking_budget }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_body_skips_reserved_keys() {
        let mut target = Map::new();
        target.insert("model".into(), Value::String("original".into()));
        let mut extra = Map::new();
        extra.insert("model".into(), Value::String("hijack".into()));
        extra.insert("top_p".into(), Value::from(0.5));
        apply_extra_body(&mut target, Some(&extra), &MISTRAL_RESERVED_BODY_KEYS);
        assert_eq!(target.get("model"), Some(&Value::String("original".into())));
        assert_eq!(target.get("top_p"), Some(&Value::from(0.5)));
    }

    #[test]
    fn clamping_matches_the_ts_ladder() {
        assert_eq!(clamp_reasoning(ThinkingLevel::Xhigh), Some(ThinkingLevel::High));
        assert_eq!(clamp_reasoning(ThinkingLevel::Max), Some(ThinkingLevel::High));
        assert_eq!(clamp_reasoning(ThinkingLevel::Low), Some(ThinkingLevel::Low));
        assert_eq!(clamp_max_for_openai(ThinkingLevel::Max, true), Some(ThinkingLevel::Xhigh));
        assert_eq!(clamp_max_for_openai(ThinkingLevel::Max, false), Some(ThinkingLevel::High));
    }

    #[test]
    fn thinking_budgets_use_the_defaults_and_overrides() {
        assert_eq!(thinking_budget_for_level(ThinkingLevel::Medium, None), 8192);
        let custom = ThinkingBudgets { minimal: None, low: None, medium: Some(4096), high: None, max: None };
        assert_eq!(thinking_budget_for_level(ThinkingLevel::Medium, Some(&custom)), 4096);
        assert_eq!(thinking_budget_for_level(ThinkingLevel::Low, Some(&custom)), 2048);
    }

    #[test]
    fn adjust_max_tokens_fits_thinking_inside_the_ceiling() {
        assert_eq!(
            adjust_max_tokens_for_thinking(None, 8192, ThinkingLevel::Medium, None),
            AdjustedThinking { max_tokens: 8192, thinking_budget: 7168 }
        );
        assert_eq!(
            adjust_max_tokens_for_thinking(Some(8192), 200_000, ThinkingLevel::Medium, None),
            AdjustedThinking { max_tokens: 16384, thinking_budget: 8192 }
        );
    }

    #[test]
    fn clamp_thinking_budget_leaves_answer_room() {
        assert_eq!(clamp_thinking_budget_to_answer_room(8192, 4096), 3072);
        assert_eq!(clamp_thinking_budget_to_answer_room(8192, 512), 0);
    }
}
