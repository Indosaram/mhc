use maho_codemode::bridges::schema_hint::{append_schema_hint, render_schema_hint};
use serde_json::json;

#[test]
fn required_properties_precede_optional_properties() {
    let rendered = render_schema_hint("t", &json!({"properties":{"optional":{"type":"string"},"needed":{"type":"number"}},"required":["needed"]})).unwrap();
    assert!(rendered.find("needed").unwrap() < rendered.find("optional").unwrap());
}

#[test]
fn const_union_and_long_enum_labels() {
    let rendered = render_schema_hint("t", &json!({"properties":{"kind":{"const":"batch"},"mode":{"enum":["a","b","c","d","e","f","g","h","i","j"]},"either":{"oneOf":[{"type":"string"},{"type":"number"}]}}})).unwrap();
    assert!(rendered.contains("kind?: \"batch\""));
    assert!(rendered.contains("2 more)"));
    assert!(rendered.contains("either?: string | number"));
}

#[test]
fn large_schema_respects_budget_and_names_full_schema_tool() {
    let properties = (0..60).map(|index| (format!("property_{index}"), json!({"type":"string","description":"x".repeat(70)}))).collect::<serde_json::Map<_,_>>();
    let rendered = render_schema_hint("big_tool", &json!({"properties":properties})).unwrap();
    assert!(rendered.encode_utf16().count() <= 1200);
    assert!(rendered.contains("[truncated; call tool_schema(\"big_tool\") for the full schema]"));
}

#[test]
fn empty_schema_leaves_error_untouched() {
    assert_eq!(append_schema_hint("validation failed", "t", &json!({})), "validation failed");
    assert!(render_schema_hint("t", &json!([])).is_none());
}

#[test]
fn array_items_show_nested_required_fields() {
    let rendered = render_schema_hint("t", &json!({"properties":{"steps":{"type":"array","items":{"type":"object","properties":{"tool":{"type":"string"}},"required":["tool"]}}},"required":["steps"]})).unwrap();
    assert!(rendered.contains("steps: array<object>"));
    assert!(rendered.contains("    tool: string"));
}

#[test]
fn description_truncation_counts_utf16_units() {
    let description = "😀".repeat(41);
    let rendered = render_schema_hint("t", &json!({"properties":{"value":{"type":"string","description":description}}})).unwrap();
    let description = rendered.split(" — ").nth(1).unwrap();
    assert_eq!(description.encode_utf16().count(), 81);
    assert_eq!(description.chars().filter(|character| *character == '😀').count(), 40);
}
