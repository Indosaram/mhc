//! Port of senpi packages/ai/src/providers/devin.models.ts (the credential-free model seed).

use crate::model::ModelCompat;
use crate::types::{InputModality, Model, ModelCost};
use serde_json::{Map, Value};

const SWE_2_CONTEXT_WINDOW: u64 = 262_000;
const SWE_1_6_CONTEXT_WINDOW: u64 = 200_000;
const SWE_MAX_TOKENS: u64 = 64_000;

fn devin_model(id: &str, name: &str, context_window: u64) -> Model {
    let mut compat = Map::new();
    compat.insert("supportsParallelToolCalls".to_owned(), Value::Bool(true));
    Model {
        id: id.to_owned(),
        name: name.to_owned(),
        api: "devin-agent".to_owned(),
        provider: "devin".to_owned(),
        base_url: "https://server.codeium.com".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![InputModality::Text],
        cost: ModelCost::default(),
        context_window,
        max_tokens: SWE_MAX_TOKENS,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: Some(ModelCompat(compat)),
    }
}

pub fn devin_models() -> Vec<Model> {
    vec![
        devin_model("swe-2-high", "SWE-2 (high)", SWE_2_CONTEXT_WINDOW),
        devin_model("swe-2-max", "SWE-2 (max)", SWE_2_CONTEXT_WINDOW),
        devin_model("swe-2-low", "SWE-2 (low)", SWE_2_CONTEXT_WINDOW),
        devin_model("swe-2-high-lite", "SWE-2 (high, lite)", SWE_2_CONTEXT_WINDOW),
        devin_model("swe-1-6", "SWE-1.6", SWE_1_6_CONTEXT_WINDOW),
        devin_model("swe-1-6-fast", "SWE-1.6 Fast", SWE_1_6_CONTEXT_WINDOW),
    ]
}
