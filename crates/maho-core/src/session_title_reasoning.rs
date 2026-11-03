//! Port of senpi `packages/coding-agent/src/core/session-title-reasoning.ts`.

use std::sync::LazyLock;

use maho_ai::model::Model;
use maho_ai::models::{clamp_thinking_level, get_supported_thinking_levels};
use maho_ai::types::{ModelThinkingLevel, ThinkingLevel};
use regex::Regex;

static REASONING_MANDATORY_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)reasoning is mandatory").unwrap_or_else(|e| panic!("reasoning mandatory: {e}"))
});

fn as_reasoning_level(level: Option<ModelThinkingLevel>) -> Option<ThinkingLevel> {
    match level? {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
    }
}

/// Reasoning level for the first title request. Titles stay reasoning-free unless the catalog says the
/// model cannot turn reasoning off; then the lowest supported level is requested, because an unset
/// level maps to the provider's "disabled" value, which those endpoints reject with 400.
pub fn initial_title_reasoning(model: &Model) -> Option<ThinkingLevel> {
    if !model.reasoning {
        return None;
    }
    let levels = get_supported_thinking_levels(model);
    if levels.contains(&ModelThinkingLevel::Off) {
        return None;
    }
    as_reasoning_level(levels.first().copied())
}

/// Reasoning level for one retry after a reasoning-free title request was rejected because the endpoint
/// mandates reasoning, which means its catalog entry is stale. `low` is the effort every such endpoint
/// seen so far accepts, clamped to what the catalog still allows.
pub fn mandatory_reasoning_retry_level(
    model: &Model,
    sent_reasoning: Option<ThinkingLevel>,
    error_message: Option<&str>,
) -> Option<ThinkingLevel> {
    if sent_reasoning.is_some() {
        return None;
    }
    if !REASONING_MANDATORY_ERROR.is_match(error_message?) {
        return None;
    }
    as_reasoning_level(Some(clamp_thinking_level(model, ModelThinkingLevel::Low)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::types::ThinkingLevelMap;

    fn model(reasoning: bool, map: Option<ThinkingLevelMap>) -> Model {
        let mut model =
            maho_ai::models_generated::get_builtin_model("anthropic", "claude-sonnet-4-5")
                .expect("builtin anthropic model")
                .clone();
        model.reasoning = reasoning;
        model.thinking_level_map = map;
        model
    }

    fn mandatory() -> Model {
        let mut map = ThinkingLevelMap::new();
        map.insert(ModelThinkingLevel::Off, None);
        map.insert(ModelThinkingLevel::Minimal, None);
        model(true, Some(map))
    }

    #[test]
    fn titles_stay_reasoning_free_unless_off_is_unsupported() {
        assert_eq!(initial_title_reasoning(&model(false, None)), None);
        assert_eq!(initial_title_reasoning(&model(true, None)), None);
        assert_eq!(
            initial_title_reasoning(&mandatory()),
            Some(ThinkingLevel::Low)
        );
    }

    #[test]
    fn a_mandatory_reasoning_rejection_retries_once_at_low() {
        let model = model(true, None);
        assert_eq!(
            mandatory_reasoning_retry_level(
                &model,
                None,
                Some("400: Reasoning is mandatory for this endpoint")
            ),
            Some(ThinkingLevel::Low)
        );
        assert_eq!(
            mandatory_reasoning_retry_level(
                &model,
                Some(ThinkingLevel::Low),
                Some("reasoning is mandatory")
            ),
            None
        );
        assert_eq!(
            mandatory_reasoning_retry_level(&model, None, Some("other error")),
            None
        );
        assert_eq!(mandatory_reasoning_retry_level(&model, None, None), None);
    }
}
