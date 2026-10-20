use serde_json::{Value, json};

use crate::internal::validate::{
    Node, ObjectSpec, boolean, defaulted, enumeration, integer_between, optional, strict_object, union,
};
use crate::issue::Issues;

fn side_panel_width_schema() -> Node {
    union(vec![
        integer_between(24, 160),
        enumeration(&[
            "10%", "11%", "12%", "13%", "14%", "15%", "16%", "17%", "18%", "19%",
            "20%", "21%", "22%", "23%", "24%", "25%", "26%", "27%", "28%", "29%",
            "30%", "31%", "32%", "33%", "34%", "35%", "36%", "37%", "38%", "39%",
            "40%", "41%", "42%", "43%", "44%", "45%", "46%", "47%", "48%", "49%", "50%",
        ]),
    ])
}

fn default_sections() -> Value {
    json!({
        "session": true, "goal": true, "context": true, "usage": false,
        "agents": true, "tools": true, "files": true, "memory": true,
    })
}

pub fn omo_side_panel_sections_schema() -> Node {
    strict_object(vec![
        defaulted("session", boolean(), || json!(true)),
        defaulted("goal", boolean(), || json!(true)),
        defaulted("context", boolean(), || json!(true)),
        defaulted("usage", boolean(), || json!(false)),
        defaulted("agents", boolean(), || json!(true)),
        defaulted("tools", boolean(), || json!(true)),
        defaulted("files", boolean(), || json!(true)),
        defaulted("memory", boolean(), || json!(true)),
    ])
}

pub fn omo_side_panel_sections_layer_schema() -> Node {
    strict_object(vec![
        optional("session", boolean()), optional("goal", boolean()),
        optional("context", boolean()), optional("usage", boolean()),
        optional("agents", boolean()), optional("tools", boolean()),
        optional("files", boolean()), optional("memory", boolean()),
    ])
}

pub fn omo_side_panel_settings_schema() -> Node {
    strict_object(vec![
        defaulted("enabled", boolean(), || json!(false)),
        defaulted("width", side_panel_width_schema(), || json!("26%")),
        defaulted("min_columns", integer_between(60, 400), || json!(120)),
        defaulted("clickable", boolean(), || json!(true)),
        defaulted("usage_poll_seconds", integer_between(60, 3600), || json!(150)),
        defaulted("sections", omo_side_panel_sections_schema(), default_sections),
    ])
}

pub fn omo_side_panel_settings_layer_schema() -> Node {
    strict_object(vec![
        optional("enabled", boolean()),
        optional("width", side_panel_width_schema()),
        optional("min_columns", integer_between(60, 400)),
        optional("clickable", boolean()),
        optional("usage_poll_seconds", integer_between(60, 3600)),
        optional("sections", omo_side_panel_sections_layer_schema()),
    ])
}

/// Resolve even a partial side-panel layer through the defaulted schema.
pub fn resolve_omo_side_panel_settings(config: &Value) -> Result<Value, Issues> {
    // The config view ignores unrelated keys; retain the validator's object type check.
    let view = Node::Object(Box::new(ObjectSpec {
        fields: vec![optional("side_panel", omo_side_panel_settings_schema())],
        ..ObjectSpec::default()
    }));
    let parsed = crate::internal::validate::safe_parse(&view, config)?;
    match parsed.get("side_panel") {
        Some(settings) => Ok(settings.clone()),
        None => crate::internal::validate::safe_parse(&omo_side_panel_settings_schema(), &json!({})),
    }
}
