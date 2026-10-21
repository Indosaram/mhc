//! Port of senpi packages/ai/src/google-thinking-generation.ts.

use crate::types::{Model, ModelThinkingLevel, ThinkingLevelMap};
use std::sync::LazyLock;

const GOOGLE_THINKING_APIS: &[&str] = &["google-generative-ai", "google-vertex"];

/// A complete or slash-namespaced Gemini ID, generation 4 or later.
pub fn is_google_generation_4_or_later_model_id(model_id: &str) -> bool {
    static GENERATION_4_ID: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?:^|/)gemini-(?:[4-9]|[1-9][0-9])(?:\.[0-9]+)?(?:-|$)")
            .unwrap_or_else(|error| panic!("invalid Gemini generation regex: {error}"))
    });
    GENERATION_4_ID.is_match(&model_id.to_lowercase())
}

/// Generated metadata wins; otherwise generation 4+ accepts only low, medium and high.
pub fn infer_google_thinking_level_map(model: &Model) -> Option<ThinkingLevelMap> {
    if let Some(map) = &model.thinking_level_map {
        return Some(map.clone());
    }
    if GOOGLE_THINKING_APIS.contains(&model.api.as_str())
        && is_google_generation_4_or_later_model_id(&model.id)
    {
        use ModelThinkingLevel::{High, Low, Max, Medium, Minimal, Off, Xhigh};
        return Some(
            [
                (Off, None),
                (Minimal, None),
                (Low, Some("low".into())),
                (Medium, Some("medium".into())),
                (High, Some("high".into())),
                (Xhigh, None),
                (Max, None),
            ]
            .into_iter()
            .collect(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        clamp_thinking_level, get_supported_thinking_levels, infer_thinking_level_map,
    };
    use ModelThinkingLevel::{High, Low, Max, Medium, Minimal, Off, Xhigh};

    fn model(api: &str, id: &str) -> Model {
        let mut model = crate::models_generated::get_builtin_model("xai", "grok-4.5")
            .expect("builtin model")
            .clone();
        model.api = api.into();
        model.id = id.into();
        model.reasoning = true;
        model.thinking_level_map = None;
        model
    }

    #[test]
    fn matches_generation_four_ids_only_at_documented_boundaries() {
        for (id, expected) in [
            ("gemini-4", true),
            ("models/GEMINI-4.5-pro", true),
            ("publishers/google/models/gemini-10-preview", true),
            ("gemini-99.12", true),
            ("gemini-3.9-pro", false),
            ("prefix-gemini-4-pro", false),
            ("gemini-04-pro", false),
            ("gemini-100-pro", false),
            ("gemini-4pro", false),
            ("gemini-4.0.1", false),
        ] {
            assert_eq!(
                is_google_generation_4_or_later_model_id(id),
                expected,
                "{id}"
            );
        }
    }

    #[test]
    fn exposes_only_discrete_ladder_when_google_generation_four_has_no_map() {
        for api in GOOGLE_THINKING_APIS {
            let model = model(api, "models/gemini-4-pro");
            let levels = get_supported_thinking_levels(&model);
            assert_eq!(levels, [Low, Medium, High]);
            for (requested, expected) in [(Off, Low), (Minimal, Low), (Xhigh, High), (Max, High)] {
                assert_eq!(clamp_thinking_level(&model, requested), expected);
            }
        }
    }

    #[test]
    fn leaves_other_apis_and_older_generations_uninferred() {
        for (api, id) in [
            ("openai-completions", "gemini-4-pro"),
            ("google-generative-ai", "gemini-3.5-pro"),
            ("google-vertex", "custom-gemini-4-pro"),
        ] {
            let model = model(api, id);
            assert_eq!(infer_thinking_level_map(&model), None);
        }
    }

    #[test]
    fn preserves_explicit_maps_including_empty_maps() {
        for map in [
            ThinkingLevelMap::new(),
            [(Off, Some("disabled".into())), (Low, None)]
                .into_iter()
                .collect(),
        ] {
            let mut model = model("google-generative-ai", "gemini-4-pro");
            model.thinking_level_map = Some(map.clone());
            let inferred = infer_thinking_level_map(&model);
            assert_eq!(inferred, Some(map));
        }
    }

    #[test]
    fn exposes_only_off_when_generation_four_does_not_reason() {
        let mut model = model("google-vertex", "gemini-4-pro");
        model.reasoning = false;
        let levels = get_supported_thinking_levels(&model);
        assert_eq!(levels, [Off]);
    }
}
