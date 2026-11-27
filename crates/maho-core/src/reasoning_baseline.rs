//! Port of senpi packages/coding-agent/src/core/reasoning-baseline.ts.

use maho_agent::types::AgentMessage;
use maho_ai::types::{ConfigurationUpdateMessage, Message, ModelThinkingLevel};
use serde_json::Value;

pub fn is_valid_thinking_level_str(level: &str) -> bool {
    matches!(
        level,
        "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
    )
}

pub fn parse_thinking_level_str(level: &str) -> Option<ModelThinkingLevel> {
    match level {
        "off" => Some(ModelThinkingLevel::Off),
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::Xhigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}

pub fn request_thinking_level(
    baseline: Option<&str>,
    thinking_level: ModelThinkingLevel,
) -> ModelThinkingLevel {
    baseline
        .and_then(parse_thinking_level_str)
        .unwrap_or(thinking_level)
}

fn is_configuration_update(msg: &AgentMessage) -> bool {
    matches!(msg, AgentMessage::Llm(Message::ConfigurationUpdate(_)))
}

fn configuration_update_effort(msg: &AgentMessage) -> Option<ModelThinkingLevel> {
    match msg {
        AgentMessage::Llm(Message::ConfigurationUpdate(cfg)) => {
            parse_thinking_level_str(&cfg.effort)
        }
        _ => None,
    }
}

fn thinking_level_str(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::Xhigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

pub fn with_live_configuration_update(
    mut messages: Vec<AgentMessage>,
    thinking_level: ModelThinkingLevel,
    timestamp: i64,
) -> Vec<AgentMessage> {
    let Some(newest_index) = messages.iter().rposition(is_configuration_update) else {
        return messages;
    };
    if configuration_update_effort(&messages[newest_index]) == Some(thinking_level) {
        return messages;
    }
    let update = AgentMessage::Llm(Message::ConfigurationUpdate(ConfigurationUpdateMessage {
        content: Vec::new(),
        effort: thinking_level_str(thinking_level).to_owned(),
        timestamp,
    }));
    let last_user_index = messages
        .iter()
        .rposition(|m| matches!(m, AgentMessage::Llm(Message::User(_))));
    let at = match last_user_index {
        Some(idx) if idx > newest_index => idx,
        _ => messages.len(),
    };
    messages.insert(at, update);
    messages
}

pub fn resume_reasoning_baseline(branch: &[Value]) -> Option<String> {
    let mut level: Option<String> = None;
    let mut level_before_change: Option<String> = None;
    let mut prev_type: Option<String> = None;
    let mut model: Option<String> = None;
    let mut baseline: Option<String> = None;

    for entry in branch {
        let entry_type = entry.get("type").and_then(Value::as_str);
        match entry_type {
            Some("thinking_level_change") => {
                level_before_change = level.clone();
                level = entry
                    .get("thinkingLevel")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Some("configuration_update")
                if prev_type.as_deref() == Some("thinking_level_change") && baseline.is_none() =>
            {
                baseline = level_before_change.clone();
            }
            Some("model_change") => {
                if let (Some(provider), Some(model_id)) = (
                    entry.get("provider").and_then(Value::as_str),
                    entry.get("modelId").and_then(Value::as_str),
                ) {
                    let next = format!("{provider}/{model_id}");
                    if model.as_deref() != Some(next.as_str()) {
                        baseline = None;
                    }
                    model = Some(next);
                }
            }
            _ => {}
        }
        prev_type = entry_type.map(str::to_owned);
    }
    baseline
}
