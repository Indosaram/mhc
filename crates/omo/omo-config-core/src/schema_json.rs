//! Published-schema projection, the native counterpart of `script/build-omo-schema.ts`: draft-7
//! JSON Schema over the [`Node`] tree with defaulted properties optionalized. The generated asset's
//! write path is non-owned, so this module stops at the document.

use serde_json::{Map, Value, json};

use crate::internal::validate::{
    ArraySpec, Field, IntBounds, Node, NumberBounds, ObjectSpec, Pattern, StringRules,
};

pub const OMO_SCHEMA_ID: &str =
    "https://raw.githubusercontent.com/code-yeongyu/oh-my-openagent/dev/assets/omo.schema.json";

fn number_bound(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        json!(value as i64)
    } else {
        json!(value)
    }
}

fn number_schema(bounds: NumberBounds) -> Value {
    let mut schema = Map::new();
    schema.insert("type".into(), json!("number"));
    if let Some(min) = bounds.min {
        schema.insert("minimum".into(), number_bound(min));
    }
    if let Some(max) = bounds.max {
        schema.insert("maximum".into(), number_bound(max));
    }
    Value::Object(schema)
}

fn integer_schema(bounds: IntBounds) -> Value {
    let mut schema = Map::new();
    schema.insert("type".into(), json!("integer"));
    if let Some(min) = bounds.min {
        let key = if bounds.exclusive_min {
            "exclusiveMinimum"
        } else {
            "minimum"
        };
        schema.insert(key.into(), number_bound(min as f64));
    }
    if let Some(max) = bounds.max {
        schema.insert("maximum".into(), number_bound(max as f64));
    }
    Value::Object(schema)
}

fn string_schema(rules: StringRules) -> Value {
    let mut schema = Map::new();
    schema.insert("type".into(), json!("string"));
    if let Some(min_len) = rules.min_len {
        schema.insert("minLength".into(), json!(min_len));
    }
    if let Some(pattern) = rules.pattern {
        schema.insert(
            "pattern".into(),
            json!(match pattern {
                Pattern::LowerSlug => "^[a-z0-9-]+$",
            }),
        );
    }
    Value::Object(schema)
}

fn array_schema(spec: &ArraySpec) -> Value {
    let mut schema = Map::new();
    schema.insert("type".into(), json!("array"));
    if let Some(item) = &spec.item {
        schema.insert("items".into(), node_schema(item));
    }
    if let Some(min_len) = spec.min_len {
        schema.insert("minItems".into(), json!(min_len));
    }
    if let Some(max_len) = spec.max_len {
        schema.insert("maxItems".into(), json!(max_len));
    }
    Value::Object(schema)
}

fn field_schema(field: &Field) -> Value {
    let mut schema = node_schema(&field.node);
    if let (Some(default), Value::Object(map)) = (field.default, &mut schema) {
        map.insert("default".into(), default());
    }
    schema
}

fn object_schema(spec: &ObjectSpec) -> Value {
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    for field in &spec.fields {
        properties.insert(field.key.to_string(), field_schema(field));
        if field.required || field.default.is_some() {
            required.push(json!(field.key));
        }
    }
    let mut schema = Map::new();
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), Value::Array(required));
    }
    schema.insert("additionalProperties".into(), Value::Bool(!spec.strict));
    Value::Object(schema)
}

pub fn node_schema(node: &Node) -> Value {
    match node {
        Node::Any => json!({}),
        Node::Null => json!({ "type": "null" }),
        Node::Boolean => json!({ "type": "boolean" }),
        Node::Number(bounds) => number_schema(*bounds),
        Node::Integer(bounds) => integer_schema(*bounds),
        Node::String(rules) => string_schema(*rules),
        Node::Literal(value) => json!({ "const": value }),
        Node::LiteralNumber(value) => json!({ "const": value }),
        Node::Enumeration(values) => json!({ "enum": values }),
        Node::Array(spec) => array_schema(spec),
        Node::Record(inner) => json!({ "type": "object", "additionalProperties": node_schema(inner) }),
        Node::Object(spec) => object_schema(spec),
        Node::Union(branches) => {
            json!({ "anyOf": branches.iter().map(node_schema).collect::<Vec<_>>() })
        }
    }
}

/// Zod v4 marks a defaulted property required; the published schema drops it.
pub fn optionalize_defaulted_properties(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            let defaulted: Vec<String> = match map.get("properties") {
                Some(Value::Object(properties)) => properties
                    .iter()
                    .filter(|(_, property)| {
                        property.get("default").is_some_and(|value| !value.is_null())
                    })
                    .map(|(key, _)| key.clone())
                    .collect(),
                _ => Vec::new(),
            };
            if !defaulted.is_empty()
                && let Some(Value::Array(required)) = map.get_mut("required")
            {
                required.retain(|entry| {
                    entry
                        .as_str()
                        .is_none_or(|key| !defaulted.iter().any(|name| name.as_str() == key))
                });
            }
            for (key, value) in map.iter_mut() {
                if key == "default" {
                    continue;
                }
                optionalize_defaulted_properties(value);
            }
        }
        Value::Array(items) => {
            for item in items {
                optionalize_defaulted_properties(item);
            }
        }
        _ => {}
    }
}

pub fn omo_config_json_schema() -> Value {
    let mut document = node_schema(&crate::schema::config::omo_config_schema());
    optionalize_defaulted_properties(&mut document);
    let mut root = Map::new();
    root.insert(
        "$schema".into(),
        json!("http://json-schema.org/draft-07/schema#"),
    );
    root.insert("$id".into(), json!(OMO_SCHEMA_ID));
    root.insert("title".into(), json!("OmO Configuration"));
    root.insert(
        "description".into(),
        json!("Configuration schema for the omo.json / omo.jsonc harness-neutral config surface"),
    );
    if let Value::Object(map) = document {
        for (key, value) in map {
            root.insert(key, value);
        }
    }
    Value::Object(root)
}

pub fn object_property_keys(node: &Node) -> Option<Vec<String>> {
    match node {
        Node::Object(spec) => Some(spec.fields.iter().map(|field| field.key.to_string()).collect()),
        _ => None,
    }
}
