//! Port of senpi packages/ai/src/api/openai-codex-responses/reasoning.ts.

use serde_json::{Map, Value};

/// `CodexReasoningSummary`.
pub const CODEX_REASONING_SUMMARIES: [&str; 3] = ["auto", "concise", "detailed"];

/// `normalizeCodexReasoningSummary`: the deprecated `off`/`on` aliases fold onto `None`/`auto`.
pub fn normalize_codex_reasoning_summary(reasoning_summary: Option<&Value>) -> Option<String> {
    match reasoning_summary {
        None => Some(String::from("auto")),
        Some(Value::Null) => None,
        Some(Value::String(value)) if value == "off" => None,
        Some(Value::String(value)) if value == "on" => Some(String::from("auto")),
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => Some(String::from("auto")),
    }
}

/// `buildCodexReasoning`.
pub fn build_codex_reasoning(
    reasoning_effort: Option<&Option<String>>,
    reasoning_summary: Option<&Value>,
    model_supports_reasoning: bool,
    thinking_off: Option<&Option<String>>,
) -> Option<Map<String, Value>> {
    let effort = match reasoning_effort {
        Some(Some(effort)) => effort.clone(),
        Some(None) => return None,
        None => {
            if !model_supports_reasoning {
                return None;
            }
            match thinking_off {
                Some(None) => return None,
                Some(Some(off)) => off.clone(),
                None => String::from("none"),
            }
        }
    };

    let mut reasoning = Map::new();
    reasoning.insert(String::from("effort"), Value::String(effort));
    if let Some(summary) = normalize_codex_reasoning_summary(reasoning_summary) {
        reasoning.insert(String::from("summary"), Value::String(summary));
    }
    Some(reasoning)
}
