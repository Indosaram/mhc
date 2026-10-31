//! Port of senpi mcp/modern/form-schema.ts: the closed elicitation form subset MRTR accepts.
use serde_json::{Map, Value};

pub type Property = Map<String, Value>;
/// Upstream `ValidatedMcpFormSchema`: an object schema `validate_mrtr_form` accepted.
pub type ValidatedMcpFormSchema = Map<String, Value>;

pub fn is_record(value: &Value) -> bool {
    value.is_object()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormChoice {
    pub value: String,
    pub label: String,
}

pub fn enums(property: &Property) -> Option<Vec<FormChoice>> {
    if let Some(values) = property.get("enum").and_then(Value::as_array)
        && !values.is_empty()
        && values.iter().all(Value::is_string)
    {
        return Some(
            values
                .iter()
                .filter_map(Value::as_str)
                .map(|value| FormChoice {
                    value: value.into(),
                    label: value.into(),
                })
                .collect(),
        );
    }
    let choices = property
        .get("oneOf")
        .or_else(|| property.get("anyOf"))?
        .as_array()?;
    let valid = !choices.is_empty()
        && choices.iter().all(|entry| {
            entry.as_object().is_some_and(|entry| {
                entry.get("const").is_some_and(Value::is_string)
                    && entry.get("title").is_some_and(Value::is_string)
                    && entry.keys().all(|key| key == "const" || key == "title")
            })
        });
    valid.then(|| {
        choices
            .iter()
            .map(|entry| FormChoice {
                value: entry["const"].as_str().unwrap_or_default().into(),
                label: entry["title"].as_str().unwrap_or_default().into(),
            })
            .collect()
    })
}

fn number(property: &Property, key: &str) -> Option<f64> {
    property.get(key).and_then(Value::as_f64)
}

fn valid_date(date: &str) -> bool {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .is_ok_and(|parsed| parsed.format("%Y-%m-%d").to_string() == date)
}

static EMAIL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[^\s@]+@[^\s@]+\.[^\s@]+$").expect("email pattern")
});
static DATE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("date pattern"));
static DATE_TIME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T(?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d(?:\.\d+)?(?:Z|[+-](?:[01]\d|2[0-3]):[0-5]\d)$")
        .expect("date-time pattern")
});

pub fn valid_value(property: &Property, value: &Value) -> bool {
    let kind = property.get("type").and_then(Value::as_str);
    if kind == Some("boolean") {
        return value.is_boolean();
    }
    if matches!(kind, Some("integer" | "number")) {
        let Some(number) = value.as_f64().filter(|number| number.is_finite()) else {
            return false;
        };
        return (kind != Some("integer") || number.fract() == 0.0)
            && self::number(property, "minimum").is_none_or(|minimum| number >= minimum)
            && self::number(property, "maximum").is_none_or(|maximum| number <= maximum);
    }
    if kind == Some("array") {
        let choices = property
            .get("items")
            .and_then(Value::as_object)
            .and_then(enums);
        let Some(values) = value.as_array() else {
            return false;
        };
        let unique = values.iter().collect::<Vec<_>>();
        return values.iter().all(|entry| {
            entry.as_str().is_some_and(|entry| {
                choices
                    .as_ref()
                    .is_some_and(|choices| choices.iter().any(|choice| choice.value == entry))
            })
        }) && unique
            .iter()
            .enumerate()
            .all(|(index, entry)| !unique[..index].contains(entry))
            && number(property, "minItems").is_none_or(|minimum| values.len() as f64 >= minimum)
            && number(property, "maxItems").is_none_or(|maximum| values.len() as f64 <= maximum);
    }
    let Some(text) = value.as_str() else {
        return false;
    };
    if let Some(choices) = enums(property)
        && !choices.iter().any(|choice| choice.value == text)
    {
        return false;
    }
    let length = text.chars().count() as f64;
    if number(property, "minLength").is_some_and(|minimum| length < minimum)
        || number(property, "maxLength").is_some_and(|maximum| length > maximum)
    {
        return false;
    }
    match property.get("format").and_then(Value::as_str) {
        Some("email") => EMAIL.is_match(text),
        Some("uri") => url::Url::parse(text).is_ok_and(|url| !url.scheme().is_empty()),
        Some("date") => DATE.is_match(text) && valid_date(text),
        Some("date-time") => DATE_TIME.is_match(text) && valid_date(&text[..10]),
        _ => true,
    }
}

/// Validate a complete submitted form, including unknown keys and omitted required fields.
pub fn valid_mcp_form_content(schema: &ValidatedMcpFormSchema, content: &Value) -> bool {
    let Some(content) = content.as_object() else {
        return false;
    };
    let empty = Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    schema
        .get("required")
        .and_then(Value::as_array)
        .is_none_or(|required| {
            required
                .iter()
                .filter_map(Value::as_str)
                .all(|key| content.contains_key(key))
        })
        && content.iter().all(|(key, value)| {
            properties
                .get(key)
                .and_then(Value::as_object)
                .is_some_and(|property| valid_value(property, value))
        })
}

/// Reject unsupported constraints rather than presenting a weaker schema.
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
        ![
            "type",
            "properties",
            "required",
            "title",
            "description",
            "$schema",
        ]
        .contains(&key.as_str())
    }) {
        return false;
    }
    if let Some(required) = schema.get("required")
        && !required.as_array().is_some_and(|required| {
            required
                .iter()
                .all(|key| key.as_str().is_some_and(|key| properties.contains_key(key)))
        })
    {
        return false;
    }
    for property in properties.values() {
        let Some(property) = property.as_object() else {
            return false;
        };
        let mut allowed = vec!["type", "title", "description", "default"];
        match property.get("type").and_then(Value::as_str) {
            Some("string") => {
                allowed.extend(["minLength", "maxLength", "format", "enum", "oneOf"]);
                if let Some(format) = property.get("format")
                    && !format.as_str().is_some_and(|format| {
                        ["email", "uri", "date", "date-time"].contains(&format)
                    })
                {
                    return false;
                }
                if (property.contains_key("enum") || property.contains_key("oneOf"))
                    && enums(property).is_none()
                {
                    return false;
                }
                if property.contains_key("enum") && property.contains_key("oneOf") {
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
                        .any(|key| !["type", "enum", "anyOf"].contains(&key.as_str()))
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
                let Some(value) = value.as_f64().filter(|value| value.is_finite()) else {
                    return false;
                };
                if !["minimum", "maximum"].contains(&key) && (value.fract() != 0.0 || value < 0.0) {
                    return false;
                }
            }
        }
        for (min, max) in [
            ("minimum", "maximum"),
            ("minLength", "maxLength"),
            ("minItems", "maxItems"),
        ] {
            if let (Some(min), Some(max)) = (number(property, min), number(property, max))
                && min > max
            {
                return false;
            }
        }
        if let Some(default) = property.get("default")
            && !valid_value(property, default)
        {
            return false;
        }
    }
    true
}
