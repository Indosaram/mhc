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
