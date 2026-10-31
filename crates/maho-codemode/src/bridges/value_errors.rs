//! The subset of `typebox/value` `Errors` (typebox 1.3.34, the version senpi 903b8336 pins) the
//! reserved bridges use to summarise invalid arguments. It walks a JSON schema in the same keyword
//! order as typebox's `ErrorSchema` (type; object: required, additionalProperties, properties;
//! array: items, minItems; string: minLength; number: minimum; const; anyOf), keeps the en_US
//! messages, encodes instance paths like `Pathing.EncodeFragment`, visits object keys in
//! `Object.getOwnPropertyNames` order and stops at the default `maxErrors` of 8.

use serde_json::{Map, Value};

const MAX_ERRORS: usize = 8;

struct Context {
    errors: Vec<(String, String)>,
}

impl Context {
    fn at_capacity(&self) -> bool {
        self.errors.len() >= MAX_ERRORS
    }
    fn add(&mut self, path: &str, message: String) -> bool {
        if !self.at_capacity() {
            self.errors.push((path.to_owned(), message));
        }
        false
    }
}

/// `Errors(schema, value).map(e => `${e.instancePath || "/"} ${e.message}`).join("; ")`, or
/// `None` when `Check(schema, value)` passes.
pub fn summarize(schema: &Value, value: &Value) -> Option<String> {
    let mut context = Context { errors: Vec::new() };
    if check(&mut context, "", schema, value) {
        return None;
    }
    let summary = context
        .errors
        .iter()
        .map(|(path, message)| format!("{} {message}", if path.is_empty() { "/" } else { path }))
        .collect::<Vec<_>>()
        .join("; ");
    Some(if summary.is_empty() {
        "invalid value".into()
    } else {
        summary
    })
}

fn check(context: &mut Context, path: &str, schema: &Value, value: &Value) -> bool {
    if context.at_capacity() {
        return false;
    }
    let schema = match schema {
        Value::Bool(true) => return true,
        Value::Bool(false) => return context.add(path, "schema is false".into()),
        Value::Object(schema) => schema,
        _ => return true,
    };
    let mut valid = true;
    if let Some(Value::String(kind)) = schema.get("type") {
        valid &= type_matches(kind, value) || context.add(path, format!("must be {kind}"));
    }
    if let Value::Object(object) = value {
        if let Some(Value::Array(required)) = schema.get("required") {
            let missing = required
                .iter()
                .filter_map(Value::as_str)
                .filter(|key| !object.contains_key(*key))
                .collect::<Vec<_>>();
            valid &= missing.is_empty()
                || context.add(
                    path,
                    format!("must have required properties {}", missing.join(", ")),
                );
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(additional) = schema.get("additionalProperties") {
            let mut rejected = false;
            for key in own_property_names(object) {
                if properties.is_some_and(|properties| properties.contains_key(key)) {
                    continue;
                }
                let child = format!("{path}/{}", encode_fragment(key));
                rejected |= !check(context, &child, additional, &object[key]);
            }
            valid &= !rejected || context.add(path, "must not have additional properties".into());
        }
        if let Some(properties) = properties {
            for (key, property) in properties {
                if let Some(child_value) = object.get(key) {
                    let child = format!("{path}/{}", encode_fragment(key));
                    valid &= check(context, &child, property, child_value);
                }
            }
        }
    }
    if let Value::Array(items) = value {
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in items.iter().enumerate() {
                valid &= check(context, &format!("{path}/{index}"), item_schema, item);
            }
        }
        if let Some(limit) = schema.get("minItems").and_then(Value::as_u64) {
            valid &= items.len() as u64 >= limit
                || context.add(path, format!("must not have fewer than {limit} items"));
        }
    }
    if let Value::String(text) = value
        && let Some(limit) = schema.get("minLength").and_then(Value::as_u64)
    {
        valid &= text.chars().count() as u64 >= limit
            || context.add(path, format!("must not have fewer than {limit} characters"));
    }
    if let Some(number) = value.as_f64()
        && let Some(limit) = schema.get("minimum")
        && let Some(minimum) = limit.as_f64()
    {
        valid &= number >= minimum || context.add(path, format!("must be >= {limit}"));
    }
    if let Some(constant) = schema.get("const") {
        valid &= value == constant || context.add(path, "must be equal to constant".into());
    }
    if let Some(Value::Array(variants)) = schema.get("anyOf") {
        let mut failed = Vec::new();
        let mut matched = false;
        for variant in variants {
            let mut operand = Context { errors: Vec::new() };
            if check(&mut operand, path, variant, value) {
                matched = true;
            } else {
                failed.push(operand);
            }
        }
        if !matched {
            for operand in failed {
                for (error_path, message) in operand.errors {
                    context.add(&error_path, message);
                }
            }
            valid &= context.add(path, "must match a schema in anyOf".into());
        }
    }
    valid
}

fn type_matches(kind: &str, value: &Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "boolean" => value.is_boolean(),
        "integer" => {
            value.is_i64()
                || value.is_u64()
                || value
                    .as_f64()
                    .is_some_and(|number| number.is_finite() && number.fract() == 0.0)
        }
        "number" => value.as_f64().is_some_and(f64::is_finite),
        "null" => value.is_null(),
        "string" => value.is_string(),
        _ => true,
    }
}

/// `Object.getOwnPropertyNames` order: array-index keys ascending, then insertion order.
fn own_property_names(object: &Map<String, Value>) -> Vec<&str> {
    let index = |key: &str| {
        (key == "0" || (!key.starts_with('0') && key.bytes().all(|byte| byte.is_ascii_digit())))
            .then(|| key.parse::<u32>().ok().filter(|index| *index != u32::MAX))
            .flatten()
    };
    let mut indexed = object
        .keys()
        .filter_map(|key| index(key).map(|value| (value, key.as_str())))
        .collect::<Vec<_>>();
    indexed.sort_unstable_by_key(|(value, _)| *value);
    indexed
        .into_iter()
        .map(|(_, key)| key)
        .chain(
            object
                .keys()
                .map(String::as_str)
                .filter(|key| index(key).is_none()),
        )
        .collect()
}

fn encode_fragment(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::summarize;
    use serde_json::json;

    fn output_schema() -> serde_json::Value {
        json!({"type":"object","required":["ids"],"properties":{"ids":{"type":"array","items":{"type":"string","minLength":1},"minItems":1},"format":{"anyOf":[{"type":"string","const":"raw"},{"type":"string","const":"tail"}]},"offset":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1}},"additionalProperties":false})
    }

    // Expected strings are typebox 1.3.34 output captured at senpi 903b8336.
    #[test]
    fn matches_typebox_multi_error_order() {
        let schema = output_schema();
        for (value, expected) in [
            (
                json!({"z":1,"extra":1,"offset":0}),
                "/ must have required properties ids; /z schema is false; /extra schema is false; / must not have additional properties; /offset must be >= 1",
            ),
            (
                json!({"ids":["a"],"format":1}),
                "/format must be string; /format must be equal to constant; /format must be string; /format must be equal to constant; /format must match a schema in anyOf",
            ),
            (
                json!({"ids":[],"offset":-0.5}),
                "/ids must not have fewer than 1 items; /offset must be integer; /offset must be >= 1",
            ),
            (
                json!({"ids":["a"],"~":1,"a/b":1}),
                "/~0 schema is false; /a~1b schema is false; / must not have additional properties",
            ),
            (json!(null), "/ must be object"),
        ] {
            assert_eq!(
                summarize(&schema, &value).as_deref(),
                Some(expected),
                "{value}"
            );
        }
        assert_eq!(
            summarize(&schema, &json!({"ids":["a"],"format":"tail","offset":2})),
            None
        );
    }

    #[test]
    fn integer_keys_come_first_and_errors_cap_at_eight() {
        let schema = json!({"type":"object","properties":{"name":{"type":"string","minLength":1}},"additionalProperties":false});
        let value: serde_json::Value =
            serde_json::from_str(r#"{"b":1,"2":1,"a":1,"1":1,"name":""}"#).unwrap();
        assert_eq!(
            summarize(&schema, &value).unwrap(),
            "/1 schema is false; /2 schema is false; /b schema is false; /a schema is false; / must not have additional properties; /name must not have fewer than 1 characters"
        );
        let value = json!({"a":1,"b":2,"c":3,"d":4,"e":5,"f":6,"g":7,"h":8,"i":9});
        assert_eq!(
            summarize(&schema, &value).unwrap(),
            "/a schema is false; /b schema is false; /c schema is false; /d schema is false; /e schema is false; /f schema is false; /g schema is false; /h schema is false"
        );
    }
}
