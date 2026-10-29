//! camelCase <-> snake_case key conversion.

use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTransformDepth {
    Deep,
    Shallow,
}

pub fn camel_to_snake(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_uppercase() {
            out.push('_');
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn snake_to_camel(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match chars.peek() {
            Some(next) if ch == '_' && next.is_ascii_lowercase() => {
                out.push(next.to_ascii_uppercase());
                chars.next();
            }
            _ => out.push(ch),
        }
    }
    out
}

pub fn transform_object_keys(
    obj: &Map<String, Value>,
    transformer: &dyn Fn(&str) -> String,
    depth: KeyTransformDepth,
) -> Map<String, Value> {
    let mut result = Map::new();
    for (key, value) in obj {
        let transformed = match (depth, value) {
            (KeyTransformDepth::Deep, Value::Object(inner)) => {
                Value::Object(transform_object_keys(inner, transformer, depth))
            }
            (KeyTransformDepth::Deep, Value::Array(items)) => Value::Array(
                items
                    .iter()
                    .map(|item| match item {
                        Value::Object(inner) => {
                            Value::Object(transform_object_keys(inner, transformer, depth))
                        }
                        other => other.clone(),
                    })
                    .collect(),
            ),
            _ => value.clone(),
        };
        result.insert(transformer(key), transformed);
    }
    result
}

pub fn object_to_snake_case(
    obj: &Map<String, Value>,
    depth: KeyTransformDepth,
) -> Map<String, Value> {
    transform_object_keys(obj, &camel_to_snake, depth)
}

pub fn object_to_camel_case(
    obj: &Map<String, Value>,
    depth: KeyTransformDepth,
) -> Map<String, Value> {
    transform_object_keys(obj, &snake_to_camel, depth)
}
