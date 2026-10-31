//! The MRTR form schema checks the private presenter path needs (senpi
//! `core/extensions/builtin/mcp/modern/form-schema.ts`).
//!
//! The mcp lane owns the extension-side copy (`maho-ext-mcp` modern/form_schema.rs). maho-rpc cannot depend on
//! maho-ext-mcp, so the host authority validates with this mirror. Keep the two in step.
use serde_json::{Map, Value};

/// `{ value, label }` choices of an enum property (senpi `enums`).
pub fn enums(property: &Map<String, Value>) -> Option<Vec<(String, String)>> {
    if let Some(Value::Array(values)) = property.get("enum")
        && !values.is_empty()
        && values.iter().all(Value::is_string)
    {
        return Some(
            values
                .iter()
                .filter_map(Value::as_str)
                .map(|value| (value.to_owned(), value.to_owned()))
                .collect(),
        );
    }
    // `property.oneOf ?? property.anyOf`: a null or absent oneOf falls through to anyOf.
    let choices = match property.get("oneOf") {
        Some(value) if !value.is_null() => Some(value),
        _ => property.get("anyOf"),
    };
    let Some(Value::Array(choices)) = choices else {
        return None;
    };
    if choices.is_empty()
        || !choices.iter().all(|entry| {
            entry.as_object().is_some_and(|entry| {
                entry.get("const").is_some_and(Value::is_string)
                    && entry.get("title").is_some_and(Value::is_string)
                    && entry.keys().all(|key| key == "const" || key == "title")
            })
        })
    {
        return None;
    }
    Some(
        choices
            .iter()
            .map(|entry| {
                (
                    entry["const"].as_str().unwrap_or_default().to_owned(),
                    entry["title"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect(),
    )
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|value| value.is_finite())
}

fn email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    let plain =
        |part: &str| !part.is_empty() && !part.chars().any(|c| c.is_whitespace() || c == '@');
    // `/^[^\s@]+@[^\s@]+\.[^\s@]+$/`: some dot in the domain with text on both sides.
    plain(local)
        && plain(domain)
        && domain
            .char_indices()
            .any(|(index, c)| c == '.' && index > 0 && index + 1 < domain.len())
}

/// `new URL(value).protocol.length > 1`: an absolute URL with a scheme.
fn uri(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !value.chars().any(char::is_whitespace)
        && !(matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "ws" | "wss" | "ftp"
        ) && rest.trim_start_matches('/').is_empty())
}

fn date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
        && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

fn two_digits(text: &str, max: u32) -> bool {
    text.len() == 2
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && text.parse::<u32>().is_ok_and(|value| value <= max)
}

/// `YYYY-MM-DDTHH:MM:SS(.fraction)?(Z|+HH:MM|-HH:MM)` with a real calendar date.
fn date_time(value: &str) -> bool {
    if value.len() < 20 || !value.is_char_boundary(10) || !date(&value[..10]) {
        return false;
    }
    let rest = &value[10..];
    let Some(rest) = rest.strip_prefix('T') else {
        return false;
    };
    if rest.len() < 9 || !rest.is_char_boundary(8) {
        return false;
    }
    let (time, mut zone) = rest.split_at(8);
    let parts: Vec<&str> = time.split(':').collect();
    if parts.len() != 3
        || !two_digits(parts[0], 23)
        || !two_digits(parts[1], 59)
        || !two_digits(parts[2], 59)
    {
        return false;
    }
    if let Some(fraction) = zone.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return false;
        }
        zone = &fraction[digits..];
    }
    if zone == "Z" {
        return true;
    }
    let Some(offset) = zone.strip_prefix('+').or_else(|| zone.strip_prefix('-')) else {
        return false;
    };
    matches!(offset.split_once(':'), Some((hours, minutes)) if two_digits(hours, 23) && two_digits(minutes, 59))
}

/// One submitted or default value against its property (senpi `validValue`).
pub fn valid_value(property: &Map<String, Value>, value: &Value) -> bool {
    let kind = property.get("type").and_then(Value::as_str);
    if kind == Some("boolean") {
        return value.is_boolean();
    }
    if matches!(kind, Some("integer" | "number")) {
        let Some(number) = number(value) else {
            return false;
        };
        return (kind != Some("integer") || number.fract() == 0.)
            && property
                .get("minimum")
                .and_then(Value::as_f64)
                .is_none_or(|minimum| number >= minimum)
            && property
                .get("maximum")
                .and_then(Value::as_f64)
                .is_none_or(|maximum| number <= maximum);
    }
    if kind == Some("array") {
        let choices = property
            .get("items")
            .and_then(Value::as_object)
            .and_then(enums);
        let Some(values) = value.as_array() else {
            return false;
        };
        let mut seen = std::collections::BTreeSet::new();
        return values.iter().all(|entry| {
            entry.as_str().is_some_and(|entry| {
                choices
                    .as_ref()
                    .is_some_and(|choices| choices.iter().any(|(value, _)| value == entry))
                    && seen.insert(entry.to_owned())
            })
        }) && property
            .get("minItems")
            .and_then(Value::as_f64)
            .is_none_or(|min| values.len() as f64 >= min)
            && property
                .get("maxItems")
                .and_then(Value::as_f64)
                .is_none_or(|max| values.len() as f64 <= max);
    }
    let Some(text) = value.as_str() else {
        return false;
    };
    if let Some(choices) = enums(property)
        && !choices.iter().any(|(choice, _)| choice == text)
    {
        return false;
    }
    let length = text.chars().count() as f64;
    if property
        .get("minLength")
        .and_then(Value::as_f64)
        .is_some_and(|min| length < min)
        || property
            .get("maxLength")
            .and_then(Value::as_f64)
            .is_some_and(|max| length > max)
    {
        return false;
    }
    match property.get("format").and_then(Value::as_str) {
        Some("email") => email(text),
        Some("uri") => uri(text),
        Some("date") => date(text),
        Some("date-time") => date_time(text),
        _ => true,
    }
}

/// A complete submitted form, including unknown keys and omitted required fields (senpi `validMcpFormContent`).
pub fn valid_mcp_form_content(schema: &Value, content: &Value) -> bool {
    let (Some(content), Some(properties)) = (
        content.as_object(),
        schema.get("properties").and_then(Value::as_object),
    ) else {
        return false;
    };
    let required_present = match schema.get("required") {
        Some(Value::Array(required)) => required
            .iter()
            .all(|key| key.as_str().is_some_and(|key| content.contains_key(key))),
        _ => true,
    };
    required_present
        && content.iter().all(|(key, value)| {
            properties
                .get(key)
                .and_then(Value::as_object)
                .is_some_and(|property| valid_value(property, value))
        })
}

/// Reject unsupported constraints rather than presenting a weaker schema (senpi `validateMrtrForm`).
pub fn validate_mrtr_form(schema: &Value) -> bool {
    let Some(schema) = schema.as_object() else {
        return false;
    };
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return false;
    };
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return false;
    }
    for key in ["title", "description"] {
        if schema.get(key).is_some_and(|value| !value.is_string()) {
            return false;
        }
    }
    if schema.keys().any(|key| {
        !matches!(
            key.as_str(),
            "type" | "properties" | "required" | "title" | "description" | "$schema"
        )
    }) {
        return false;
    }
    if let Some(required) = schema.get("required") {
        let Some(required) = required.as_array() else {
            return false;
        };
        if !required
            .iter()
            .all(|key| key.as_str().is_some_and(|key| properties.contains_key(key)))
        {
            return false;
        }
    }
    properties
        .values()
        .all(|property| property.as_object().is_some_and(valid_property))
}

fn valid_property(property: &Map<String, Value>) -> bool {
    let mut allowed = vec!["type", "title", "description", "default"];
    match property.get("type").and_then(Value::as_str) {
        Some("string") => {
            allowed.extend(["minLength", "maxLength", "format", "enum", "oneOf"]);
            if let Some(format) = property.get("format") {
                let format = match format {
                    Value::String(format) => format.clone(),
                    other => other.to_string(),
                };
                if !matches!(format.as_str(), "email" | "uri" | "date" | "date-time") {
                    return false;
                }
            }
            let has_enum = property.contains_key("enum");
            let has_one_of = property.contains_key("oneOf");
            if (has_enum || has_one_of) && enums(property).is_none() {
                return false;
            }
            if has_enum && has_one_of {
                return false;
            }
        }
        Some("number" | "integer") => allowed.extend(["minimum", "maximum"]),
        Some("boolean") => {}
        Some("array") => {
            allowed.extend(["items", "minItems", "maxItems"]);
            let Some(items) = property.get("items").and_then(Value::as_object) else {
                return false;
            };
            if enums(items).is_none()
                || items
                    .keys()
                    .any(|key| !matches!(key.as_str(), "type" | "enum" | "anyOf"))
                || items
                    .get("type")
                    .is_some_and(|kind| kind.as_str() != Some("string"))
            {
                return false;
            }
        }
        _ => return false,
    }
    if property.keys().any(|key| !allowed.contains(&key.as_str())) {
        return false;
    }
    for key in ["title", "description"] {
        if property.get(key).is_some_and(|value| !value.is_string()) {
            return false;
        }
    }
    for key in [
        "minimum",
        "maximum",
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
    ] {
        if let Some(value) = property.get(key) {
            let Some(value) = number(value) else {
                return false;
            };
            if !matches!(key, "minimum" | "maximum") && (value.fract() != 0. || value < 0.) {
                return false;
            }
        }
    }
    for (min, max) in [
        ("minimum", "maximum"),
        ("minLength", "maxLength"),
        ("minItems", "maxItems"),
    ] {
        if let (Some(min), Some(max)) = (
            property.get(min).and_then(Value::as_f64),
            property.get(max).and_then(Value::as_f64),
        ) && min > max
        {
            return false;
        }
    }
    !property
        .get("default")
        .is_some_and(|default| !valid_value(property, default))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn schema() -> Value {
        json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"]})
    }
    #[test]
    fn content_rejects_missing_required_unknown_keys_and_wrong_types() {
        assert!(valid_mcp_form_content(&schema(), &json!({"value":"a"})));
        assert!(!valid_mcp_form_content(&schema(), &json!({})));
        assert!(!valid_mcp_form_content(&schema(), &json!({"value":false})));
        assert!(!valid_mcp_form_content(
            &schema(),
            &json!({"value":"ok","extra":"forbidden"})
        ));
    }
    #[test]
    fn schema_rejects_unsupported_constraints_and_bad_defaults() {
        assert!(validate_mrtr_form(&schema()));
        assert!(!validate_mrtr_form(
            &json!({"type":"object","properties":{"v":{"type":"string","pattern":"x"}}})
        ));
        assert!(!validate_mrtr_form(
            &json!({"type":"object","properties":{"v":{"type":"integer","default":1.5}}})
        ));
        assert!(!validate_mrtr_form(
            &json!({"type":"object","properties":{},"required":["missing"]})
        ));
        assert!(validate_mrtr_form(
            &json!({"type":"object","properties":{"v":{"type":"array","items":{"enum":["a","b"]},"maxItems":2}}})
        ));
    }
    #[test]
    fn formats_check_real_values() {
        let property = |format: &str| {
            json!({"type":"string","format":format})
                .as_object()
                .cloned()
                .unwrap_or_default()
        };
        assert!(valid_value(&property("email"), &json!("a@b.co")));
        assert!(!valid_value(&property("email"), &json!("a@b")));
        assert!(valid_value(&property("date"), &json!("2024-02-29")));
        assert!(!valid_value(&property("date"), &json!("2023-02-29")));
        assert!(valid_value(
            &property("date-time"),
            &json!("2024-01-01T10:00:00.5+02:00")
        ));
        assert!(!valid_value(
            &property("date-time"),
            &json!("2024-01-01T24:00:00Z")
        ));
        assert!(valid_value(&property("uri"), &json!("https://example.com")));
        assert!(!valid_value(&property("uri"), &json!("not a uri")));
    }
}
