use super::*;
use crate::providers::opengateway_catalog::{OpenGatewayPrice, PriceRoute};
use serde_json::json;

fn shipped(id: &str, context_window: u64, max_tokens: u64) -> Model {
    serde_json::from_value(json!({
        "id": id, "name": "GLM 5.3", "api": "openai-completions", "provider": "opengateway",
        "baseUrl": OPENGATEWAY_BASE_URL, "reasoning": true, "input": ["text"],
        "cost": {"input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": context_window, "maxTokens": max_tokens
    }))
    .expect("model")
}

fn listed(id: &str, status: &str, context_window: Option<u64>) -> OpenGatewayListedModel {
    OpenGatewayListedModel {
        id: id.into(),
        status: Some(status.into()),
        input_modalities: vec!["text".into(), "image".into()],
        endpoints: vec!["chat_completions".into()],
        routes: vec!["r".into()],
        context_window,
        max_output_tokens: Some(1_000_000),
    }
}

fn priced(id: &str) -> PriceRoute {
    PriceRoute {
        route: "r".into(),
        model_id: id.into(),
        price: OpenGatewayPrice {
            input: Some(0.5),
            output: Some(1.5),
            ..Default::default()
        },
    }
}

#[test]
fn removes_retired_and_unlisted_rows_and_adds_priced_serving_tiers() {
    let shipped = vec![
        shipped("z-ai/glm-5.3", 200_000, 64_000),
        shipped("o/gone", 1, 1),
        shipped("o/retired", 1, 1),
    ];
    let listing = vec![
        listed("z-ai/glm-5.3", "active", None),
        listed("o/retired", "retired", None),
        listed("z-ai/glm-5.3-ultrafast", "active", None),
        listed("o/unpriced", "active", Some(1000)),
    ];
    let unshipped: Vec<&str> = unshipped_servable_models(&shipped, &listing)
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(unshipped, vec!["z-ai/glm-5.3-ultrafast", "o/unpriced"]);
    let overlay =
        overlay_open_gateway_catalog(&shipped, &listing, &vec![priced("z-ai/glm-5.3-ultrafast")]);
    let ids: Vec<&str> = overlay.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["z-ai/glm-5.3", "z-ai/glm-5.3-ultrafast"]);
    let tier = &overlay[1];
    assert_eq!(tier.name, "GLM 5.3 Ultrafast");
    assert!(tier.reasoning);
    assert_eq!(tier.context_window, 200_000);
    assert_eq!(tier.max_tokens, 200_000);
    assert_eq!(tier.input, vec![InputModality::Text, InputModality::Image]);
    assert_eq!((tier.cost.input, tier.cost.output), (0.5, 1.5));
}

#[test]
fn builds_a_bare_model_for_an_unknown_base_and_skips_models_without_a_context_window() {
    let listing = vec![
        listed("o/new", "active", Some(400_000)),
        listed("o/blind", "active", None),
    ];
    let mut item = listing[0].clone();
    item.max_output_tokens = None;
    let overlay = overlay_open_gateway_catalog(
        &[],
        &[item, listing[1].clone()],
        &vec![priced("o/new"), priced("o/blind")],
    );
    assert_eq!(overlay.len(), 1);
    let model = &overlay[0];
    assert_eq!(
        (model.name.as_str(), model.max_tokens, model.reasoning),
        ("o/new", UNPUBLISHED_MAX_OUTPUT_TOKENS, false)
    );
    assert_eq!(
        model
            .compat
            .as_ref()
            .and_then(|compat| compat.0.get("supportsDeveloperRole")),
        Some(&Value::Bool(false))
    );
}

#[test]
fn applies_the_openai_input_cap_to_added_gpt_models() {
    let mut item = listed("openai/gpt-5.5", "active", Some(400_000));
    item.max_output_tokens = Some(128_000);
    let overlay = overlay_open_gateway_catalog(&[], &[item], &vec![priced("openai/gpt-5.5")]);
    assert_eq!(overlay[0].context_window, 272_000);
}
