//! Port of senpi packages/ai/src/utils/openai-input-cap.ts.

use crate::types::Model;
use regex::Regex;
use std::sync::LazyLock;

static GATEWAY_ID_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[a-z]{2}\.)?(?:global\.)?openai[./]").expect("valid regex"));
static GPT5_OR_6: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^gpt-(?:5|6)(?:[.-]|$)").expect("valid regex"));

fn documented_input_cap(context_window: u64) -> Option<u64> {
    match context_window {
        400_000 => Some(272_000),
        1_050_000 => Some(922_000),
        _ => None,
    }
}

/// OpenAI's input/output split follows GPT-5/6 models across gateway providers.
pub fn apply_openai_input_cap(model: &mut Model) {
    let bare = GATEWAY_ID_PREFIX.replace(&model.id, "").into_owned();
    if !GPT5_OR_6.is_match(&bare) {
        return;
    }
    // Historical models.dev output metadata duplicated GPT-5 Pro's input sub-limit.
    if bare == "gpt-5-pro" && model.max_tokens == 272_000 {
        model.max_tokens = 128_000;
    }
    if model.max_tokens == 128_000 {
        model.context_window =
            documented_input_cap(model.context_window).unwrap_or(model.context_window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(id: &str, context_window: u64, max_tokens: u64) -> Model {
        serde_json::from_value(json!({
            "id": id, "name": id, "api": "openai-completions", "provider": "opengateway",
            "baseUrl": "https://example.test", "reasoning": false, "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": context_window, "maxTokens": max_tokens
        }))
        .expect("model")
    }

    #[test]
    fn caps_gpt5_and_gpt6_inputs_across_gateway_prefixes() {
        for id in [
            "gpt-5",
            "openai/gpt-5.5",
            "us.openai.gpt-6",
            "global.openai/gpt-5-mini",
        ] {
            let mut capped = model(id, 400_000, 128_000);
            apply_openai_input_cap(&mut capped);
            assert_eq!(capped.context_window, 272_000, "{id}");
        }
        let mut wide = model("gpt-6", 1_050_000, 128_000);
        apply_openai_input_cap(&mut wide);
        assert_eq!(wide.context_window, 922_000);
    }

    #[test]
    fn repairs_gpt5_pro_output_and_leaves_other_models_alone() {
        let mut pro = model("gpt-5-pro", 400_000, 272_000);
        apply_openai_input_cap(&mut pro);
        assert_eq!((pro.max_tokens, pro.context_window), (128_000, 272_000));
        for (id, window, max) in [
            ("gpt-4o", 400_000, 128_000),
            ("gpt-50", 400_000, 128_000),
            ("gpt-5", 400_000, 64_000),
        ] {
            let mut untouched = model(id, window, max);
            apply_openai_input_cap(&mut untouched);
            assert_eq!(untouched.context_window, window, "{id}");
        }
    }
}
