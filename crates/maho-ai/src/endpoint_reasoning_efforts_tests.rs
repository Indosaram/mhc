//! Port of the `parseEndpointReasoningEfforts` cases of senpi
//! packages/ai/test/endpoint-reasoning-efforts.test.ts.

use super::*;
use ModelThinkingLevel::*;
use serde_json::json;

fn map(entries: [(ModelThinkingLevel, Option<&str>); 7]) -> ThinkingLevelMap {
    entries
        .into_iter()
        .map(|(level, value)| (level, value.map(str::to_owned)))
        .collect()
}

#[test]
fn maps_advertised_values_onto_senpi_levels_keeps_the_endpoint_spelling_and_vetoes_the_rest() {
    let parsed = parse_endpoint_reasoning_efforts(
        &json!([{"value": "low"}, {"value": "High", "default": true}]),
    );
    assert_eq!(
        parsed,
        Some(EndpointReasoningEfforts {
            thinking_level_map: Some(map([
                (Off, None),
                (Minimal, None),
                (Low, Some("low")),
                (Medium, None),
                (High, Some("High")),
                (Xhigh, None),
                (Max, None),
            ])),
            default_thinking_level: Some(High),
            unmapped: vec![],
        })
    );
}

#[test]
fn maps_none_off_to_the_off_level_and_accepts_plain_string_entries() {
    let parsed =
        parse_endpoint_reasoning_efforts(&json!(["none", "medium", "xhigh", "max"])).unwrap();
    assert_eq!(
        parsed.thinking_level_map,
        Some(map([
            (Off, Some("none")),
            (Minimal, None),
            (Low, None),
            (Medium, Some("medium")),
            (High, None),
            (Xhigh, Some("xhigh")),
            (Max, Some("max")),
        ]))
    );
    assert_eq!(parsed.default_thinking_level, None);
}

#[test]
fn reports_names_it_cannot_map_and_never_uses_them_as_the_default() {
    let parsed = parse_endpoint_reasoning_efforts(&json!([
        {"value": "turbo", "default": true},
        {"value": "low"},
        {"value": "LOW"}
    ]))
    .unwrap();
    assert_eq!(parsed.unmapped, vec!["turbo".to_owned()]);
    assert_eq!(parsed.default_thinking_level, None);
    assert_eq!(
        parsed
            .thinking_level_map
            .unwrap()
            .get(&Low)
            .cloned()
            .flatten()
            .as_deref(),
        Some("low")
    );
}

#[test]
fn treats_inherited_object_property_names_as_unknown_names() {
    let parsed = parse_endpoint_reasoning_efforts(&json!([
        {"value": "__proto__", "default": true},
        {"value": "constructor"},
        {"value": "toString"}
    ]));
    assert_eq!(
        parsed,
        Some(EndpointReasoningEfforts {
            unmapped: vec!["__proto__".into(), "constructor".into(), "toString".into()],
            ..Default::default()
        })
    );
}

#[test]
fn distinguishes_an_advertised_ladder_with_nothing_representable_from_absent_metadata() {
    let only = |unmapped: Vec<&str>| {
        Some(EndpointReasoningEfforts {
            unmapped: unmapped.into_iter().map(str::to_owned).collect(),
            ..Default::default()
        })
    };
    assert_eq!(
        parse_endpoint_reasoning_efforts(&json!([{"value": "turbo"}])),
        only(vec!["turbo"])
    );
    assert_eq!(parse_endpoint_reasoning_efforts(&json!([])), only(vec![]));
    assert_eq!(
        parse_endpoint_reasoning_efforts(&json!([{"value": 3}, null, {"default": true}])),
        only(vec![])
    );
    assert_eq!(parse_endpoint_reasoning_efforts(&Value::Null), None);
    assert_eq!(
        parse_endpoint_reasoning_efforts(&json!({"value": "low"})),
        None
    );
}
