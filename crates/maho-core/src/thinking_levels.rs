//! Port of senpi packages/coding-agent/src/core/thinking-levels.ts.

use maho_ai::model::Model;
use maho_ai::models::{get_supported_thinking_levels as model_supported_levels, supports_max as model_supports_max, supports_xhigh as model_supports_xhigh};
use maho_ai::types::{ModelThinkingLevel, ThinkingLevel};

pub fn supports_xhigh(model: &Model) -> bool {
    model_supports_xhigh(model)
}

pub fn supports_max(model: &Model) -> bool {
    model_supports_max(model)
}

/// Supported levels, always non-empty: the model's own levels, else ["off"].
pub fn get_supported_thinking_levels(model: &Model) -> Vec<ModelThinkingLevel> {
    let levels = model_supported_levels(model);
    if levels.is_empty() { vec![ModelThinkingLevel::Off] } else { levels }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningCapabilityKind {
    None,
    AlwaysOn,
    OnOff,
    Graded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningCapability {
    pub kind: ReasoningCapabilityKind,
    pub levels: Vec<ModelThinkingLevel>,
    pub non_off_levels: Vec<ThinkingLevel>,
}

fn non_off(level: ModelThinkingLevel) -> Option<ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
    }
}

/// Classifies a model's reasoning capability purely from its reasoning flag and supported levels.
pub fn classify_reasoning_capability(model: &Model) -> ReasoningCapability {
    if !model.reasoning {
        return ReasoningCapability { kind: ReasoningCapabilityKind::None, levels: vec![ModelThinkingLevel::Off], non_off_levels: Vec::new() };
    }
    let levels = get_supported_thinking_levels(model);
    let non_off_levels: Vec<ThinkingLevel> = levels.iter().copied().filter_map(non_off).collect();
    if !levels.contains(&ModelThinkingLevel::Off) {
        return ReasoningCapability { kind: ReasoningCapabilityKind::AlwaysOn, levels, non_off_levels };
    }
    if non_off_levels.len() == 1 {
        return ReasoningCapability { kind: ReasoningCapabilityKind::OnOff, levels, non_off_levels };
    }
    if non_off_levels.is_empty() {
        return ReasoningCapability { kind: ReasoningCapabilityKind::None, levels, non_off_levels };
    }
    ReasoningCapability { kind: ReasoningCapabilityKind::Graded, levels, non_off_levels }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(reasoning: bool) -> Model {
        let mut model: Model = serde_json::from_value(serde_json::json!({
            "id": "m", "name": "M", "api": "openai-completions", "provider": "p", "baseUrl": "",
            "reasoning": reasoning, "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000, "maxTokens": 100
        })).expect("model");
        model.reasoning = reasoning;
        model
    }

    #[test]
    fn a_non_reasoning_model_is_kind_none() {
        let capability = classify_reasoning_capability(&model(false));
        assert_eq!(capability.kind, ReasoningCapabilityKind::None);
        assert_eq!(capability.levels, vec![ModelThinkingLevel::Off]);
        assert!(capability.non_off_levels.is_empty());
    }

    #[test]
    fn supported_levels_are_never_empty() {
        assert!(!get_supported_thinking_levels(&model(false)).is_empty());
    }

    #[test]
    fn a_reasoning_model_classifies_from_its_levels() {
        let capability = classify_reasoning_capability(&model(true));
        assert!(matches!(
            capability.kind,
            ReasoningCapabilityKind::AlwaysOn | ReasoningCapabilityKind::OnOff | ReasoningCapabilityKind::Graded | ReasoningCapabilityKind::None
        ));
    }
}
