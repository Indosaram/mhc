use super::*;
use serde_json::json;

fn route() -> Value {
    json!({"provider":"route-a","modelOwner":"owner","modelName":"model","inputCostPerToken":0.000001})
}

#[test]
fn parses_envelope_and_legacy_reserved_route_keys() {
    let envelope = json!({"currency":"USD","unit":"USD_PER_TOKEN","entries":{"x":route()}});
    assert_eq!(
        parse_open_gateway_price_table(&envelope).unwrap()[0].route,
        "route-a"
    );
    for key in ["entries", "currency", "unit"] {
        let flat = json!({key:route()});
        assert_eq!(
            parse_open_gateway_price_table(&flat).unwrap()[0].model_id,
            "owner/model"
        );
    }
}

#[test]
fn rejects_unsupported_envelope_metadata() {
    let entries = json!({"x":route()});
    assert_eq!(
        parse_open_gateway_price_table(&json!({"entries":entries,"currency":"EUR"})).unwrap_err(),
        "OpenGateway price table has unsupported currency: EUR (expected USD)"
    );
    assert!(parse_open_gateway_price_table(&json!({"entries":entries,"unit":"OTHER"})).is_err());
}

#[test]
fn removes_float_noise_and_reads_effective_prices_with_sorted_tiers() {
    let table = json!({"a": {
        "provider": "r", "modelOwner": "o", "modelName": "m", "inputCostPerToken": 9.0,
        "pricing": {"current": {"effectivePrice": {
            "inputCostPerToken": 2e-7, "outputCostPerToken": 0.0000012,
            "axisPrices": {
                "AxisKey(threshold=500000, tier=null)": {"input": 4e-7, "output": 0.0000024, "cacheRead": 1e-7},
                "AxisKey(threshold=272000, tier=null)": {"input": 3e-7, "output": 0.0000018},
                "AxisKey(threshold=1, tier=batch)": {"input": 1e-7, "output": 1e-7},
                "AxisKey(threshold=9, tier=null)": {"isEmpty": true, "input": 1e-7, "output": 1e-7}
            }
        }}}
    }});
    let price = &parse_open_gateway_price_table(&table).unwrap()[0].price;
    assert_eq!(price.input, Some(0.2));
    assert_eq!(price.output, Some(1.2));
    assert_eq!(price.cache_read, None);
    let thresholds: Vec<u64> = price
        .tiers
        .iter()
        .map(|tier| tier.input_tokens_above)
        .collect();
    assert_eq!(thresholds, vec![272_000, 500_000]);
    assert_eq!(price.tiers[1].cache_read, 0.1);
    assert_eq!(price.tiers[0].cache_write, 0.0);
}

#[test]
fn parses_the_listing_and_classifies_servable_models_and_tiers() {
    let listing = parse_open_gateway_listing(&json!({"data": [
        {"id": "z-ai/glm-5.3-ultrafast", "status": "active", "modalities": {"input": ["text", "image"]},
         "endpoints": ["chat_completions"], "providers": [{"id": "r1"}, {"id": 3}], "context_window": 1000,
         "max_output_tokens": 0},
        {"id": "no-slash"},
        {"id": "o/retired", "status": "retired", "endpoints": ["chat_completions"]}
    ]}))
    .unwrap();
    assert_eq!(listing.len(), 2);
    assert_eq!(listing[0].routes, vec!["r1".to_owned()]);
    assert_eq!(
        (listing[0].context_window, listing[0].max_output_tokens),
        (Some(1000), None)
    );
    assert!(is_servable_chat_model(&listing[0]));
    assert!(!is_servable_chat_model(&listing[1]));
    assert_eq!(
        serving_tier_base("z-ai/glm-5.3-ultrafast"),
        Some(("z-ai/glm-5.3".to_owned(), "Ultrafast"))
    );
    assert_eq!(serving_tier_base("-ultrafast"), None);
    assert!(parse_open_gateway_listing(&json!({"data": []})).is_err());
    assert!(parse_open_gateway_listing(&json!([])).is_err());
}

#[test]
fn prefers_the_listed_route_price_and_overlays_it_on_the_fallback() {
    let price = |input| OpenGatewayPrice {
        input: Some(input),
        output: Some(2.0),
        ..Default::default()
    };
    let table = vec![
        PriceRoute {
            route: "b".into(),
            model_id: "o/m".into(),
            price: price(1.0),
        },
        PriceRoute {
            route: "a".into(),
            model_id: "o/m".into(),
            price: price(5.0),
        },
    ];
    let model = OpenGatewayListedModel {
        id: "o/m".into(),
        routes: vec!["a".into()],
        ..Default::default()
    };
    assert_eq!(open_gateway_price(&model, &table).unwrap().input, Some(5.0));
    let unrouted = OpenGatewayListedModel {
        id: "o/m".into(),
        ..Default::default()
    };
    assert_eq!(
        open_gateway_price(&unrouted, &table).unwrap().input,
        Some(1.0)
    );

    let tier = ModelCostTier {
        input: 9.0,
        output: 9.0,
        cache_read: 0.0,
        cache_write: 0.0,
        input_tokens_above: 1,
    };
    let fallback = ModelCost {
        input: 7.0,
        output: 8.0,
        cache_read: 0.5,
        tiers: Some(vec![tier]),
        ..Default::default()
    };
    let flat = with_gateway_price(&fallback, Some(&price(1.0)));
    assert_eq!(
        (flat.input, flat.output, flat.cache_read, flat.tiers.clone()),
        (1.0, 2.0, 0.5, None)
    );
    let partial = with_gateway_price(
        &fallback,
        Some(&OpenGatewayPrice {
            input: Some(3.0),
            ..Default::default()
        }),
    );
    assert_eq!(
        (partial.input, partial.output, partial.tiers),
        (3.0, 8.0, Some(vec![tier]))
    );
    assert_eq!(with_gateway_price(&fallback, None), fallback);
}
