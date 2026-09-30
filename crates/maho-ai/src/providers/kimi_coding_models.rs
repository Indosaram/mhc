//! Port of senpi packages/ai/src/providers/kimi-coding.models.ts (fork-owned catalog).

use crate::model::ModelCompat;
use crate::types::{InputModality, Model, ModelCost, ModelThinkingLevel, ThinkingLevelMap};
use serde_json::{Map, Value};

const BASE_URL: &str = "https://api.kimi.com/coding";

fn compat(allow_empty_signature: bool) -> ModelCompat {
    let mut compat = Map::new();
    if allow_empty_signature {
        compat.insert("allowEmptySignature".to_owned(), Value::Bool(true));
    }
    compat.insert("forceAdaptiveThinking".to_owned(), Value::Bool(true));
    ModelCompat(compat)
}

fn thinking_level_map() -> ThinkingLevelMap {
    [
        (ModelThinkingLevel::Off, None),
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, Some("low".to_owned())),
        (ModelThinkingLevel::Medium, None),
        (ModelThinkingLevel::High, Some("high".to_owned())),
        (ModelThinkingLevel::Xhigh, None),
        (ModelThinkingLevel::Max, Some("max".to_owned())),
    ]
    .into_iter()
    .collect()
}

#[allow(clippy::too_many_arguments)]
fn model(
    id: &str,
    name: &str,
    allow_empty_signature: bool,
    input: &[InputModality],
    cost: (f64, f64, f64),
    context_window: u64,
    max_tokens: u64,
    levels: Option<ThinkingLevelMap>,
) -> Model {
    Model {
        id: id.to_owned(),
        name: name.to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: "kimi-coding".to_owned(),
        base_url: BASE_URL.to_owned(),
        reasoning: true,
        thinking_level_map: levels,
        input: input.to_vec(),
        cost: ModelCost { input: cost.0, output: cost.1, cache_read: cost.2, cache_write: 0.0, tiers: None },
        context_window,
        max_tokens,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: Some(compat(allow_empty_signature)),
    }
}

pub fn kimi_coding_models() -> Vec<Model> {
    vec![
        model(
            "k3",
            "Kimi K3",
            true,
            &[InputModality::Text, InputModality::Image, InputModality::Video],
            (3.0, 15.0, 0.3),
            1048576,
            131072,
            Some(thinking_level_map()),
        ),
        model(
            "k3-256k",
            "Kimi K3-256K",
            false,
            &[InputModality::Text, InputModality::Image],
            (0.0, 0.0, 0.0),
            262144,
            131072,
            Some(thinking_level_map()),
        ),
        model(
            "kimi-for-coding",
            "kimi-for-coding",
            true,
            &[InputModality::Text, InputModality::Image],
            (0.95, 4.0, 0.19),
            1048576,
            32768,
            Some(thinking_level_map()),
        ),
        model(
            "kimi-for-coding-highspeed",
            "Kimi For Coding HighSpeed",
            false,
            &[InputModality::Text, InputModality::Image],
            (1.9, 8.0, 0.38),
            262144,
            32768,
            None,
        ),
        model(
            "kimi-k2-thinking",
            "Kimi K2 Thinking",
            false,
            &[InputModality::Text],
            (0.6, 2.5, 0.15),
            262144,
            32768,
            None,
        ),
    ]
}
