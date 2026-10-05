use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum ToolSchema {
    Unknown, Literal(Value), Union(Vec<ToolSchema>), String, Number, Integer, Boolean, Null,
    Array(Box<ToolSchema>), Object(Vec<(String, ToolSchema, bool)>),
}

pub fn json_schema_to_shape(schema: &Value) -> Vec<(String, ToolSchema, bool)> {
    let required = schema["required"].as_array();
    schema["properties"].as_object().into_iter().flat_map(Map::iter).map(|(key, value)| {
        let mandatory = required.is_some_and(|keys| keys.iter().any(|item| item.as_str() == Some(key)));
        (key.clone(), schema_to_type(value), mandatory)
    }).collect()
}

fn schema_to_type(schema: &Value) -> ToolSchema {
    if let Some(value) = schema.get("const") { return ToolSchema::Literal(value.clone()); }
    if let Some(values) = schema["enum"].as_array() {
        let variants: Vec<_> = values.iter().filter(|v| v.is_string() || v.is_number() || v.is_boolean())
            .map(|v| ToolSchema::Literal(v.clone())).collect();
        if !variants.is_empty() { return union(variants); }
    }
    if let Some(variants) = schema.get("anyOf").or_else(|| schema.get("oneOf")).and_then(Value::as_array)
        && !variants.is_empty() { return union(variants.iter().map(schema_to_type).collect()); }
    let kind = schema["type"].as_str().or_else(|| schema["type"].as_array().and_then(|v| v.first()).and_then(Value::as_str));
    match kind {
        Some("string") => ToolSchema::String,
        Some("number") => ToolSchema::Number,
        Some("integer") => ToolSchema::Integer,
        Some("boolean") => ToolSchema::Boolean,
        Some("null") => ToolSchema::Null,
        Some("array") => ToolSchema::Array(Box::new(schema.get("items").map(schema_to_type).unwrap_or(ToolSchema::Unknown))),
        Some("object") => ToolSchema::Object(json_schema_to_shape(schema)),
        _ => ToolSchema::Unknown,
    }
}

fn union(mut variants: Vec<ToolSchema>) -> ToolSchema {
    if variants.len() == 1 { variants.remove(0) } else { ToolSchema::Union(variants) }
}

impl ToolSchema {
    pub fn accepts(&self, value: &Value) -> bool {
        match self {
            Self::Unknown => true,
            Self::Literal(literal) => value == literal,
            Self::Union(variants) => variants.iter().any(|schema| schema.accepts(value)),
            Self::String => value.is_string(), Self::Number => value.is_number(),
            Self::Integer => value.as_f64().is_some_and(|n| n.fract().abs() < f64::EPSILON),
            Self::Boolean => value.is_boolean(), Self::Null => value.is_null(),
            Self::Array(schema) => value.as_array().is_some_and(|values| values.iter().all(|value| schema.accepts(value))),
            Self::Object(shape) => value.as_object().is_some_and(|object| shape.iter().all(|(key, schema, required)| {
                object.get(key).map(|value| schema.accepts(value)).unwrap_or(!required)
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn required_optional_fields() {
        let schema = ToolSchema::Object(json_schema_to_shape(&json!({"properties": {
            "path":{"type":"string"},"count":{"type":"integer"},"verbose":{"type":"boolean"},
            "mode":{"enum":["fast","slow"]},"tags":{"type":"array","items":{"type":"string"}}
        }, "required":["path","count"]})));
        assert!(schema.accepts(&json!({"path":"a","count":2})));
        assert!(!schema.accepts(&json!({"count":2})));
        assert!(schema.accepts(&json!({"path":"a","count":2,"mode":"fast","tags":["x"]})));
        assert!(!schema.accepts(&json!({"path":"a","count":2,"mode":"other"})));
        assert!(schema.accepts(&json!({"path":"a","count":2,"verbose":true})));
    }
    #[test]
    fn literal_unions_and_constants() {
        let schema = ToolSchema::Object(json_schema_to_shape(&json!({"properties": {
            "action":{"anyOf":[{"const":"start"},{"const":"stop"},{"const":"status"}]},
            "mode":{"const":"safe"}, "note":{"type":"string"}
        },"required":["action","mode"]})));
        assert!(schema.accepts(&json!({"action":"start","mode":"safe"})));
        assert!(!schema.accepts(&json!({"action":"delete","mode":"safe"})));
        assert!(!schema.accepts(&json!({"action":"stop","mode":"yolo"})));
    }
    #[test]
    fn missing_properties_empty_shape() {
        assert!(json_schema_to_shape(&Value::Null).is_empty());
        assert!(json_schema_to_shape(&json!({"type":"object"})).is_empty());
    }
}
