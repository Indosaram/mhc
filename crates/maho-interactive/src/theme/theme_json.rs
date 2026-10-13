//! Port of theme-json.ts: typed document validation.
use std::collections::BTreeMap;
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ColorValue {
    Index(u8),
    Text(String),
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThemeJson {
    pub name: String,
    #[serde(default)]
    pub vars: BTreeMap<String, ColorValue>,
    pub colors: BTreeMap<String, ColorValue>,
    #[serde(default, rename = "export")]
    pub export_colors: BTreeMap<String, ColorValue>,
}
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("Invalid theme: {0}")]
    Invalid(String),
    /// senpi `validateThemeJson`: the full multi-line schema diagnostic, shown verbatim.
    #[error("{0}")]
    Schema(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
const OPTIONAL_COLORS: [&str; 6] = [
    "scrollbarTrack",
    "scrollbarThumb",
    "thinkingMax",
    "searchMatchText",
    "skillMention",
    "searchMatchBg",
];

/// `ColorValueSchema`: a string, or an integer 256-color index in `0..=255`.
fn is_color_value(value: &serde_json::Value) -> bool {
    value.is_string() || value.as_u64().is_some_and(|index| index <= 255)
}

/// Schema errors other than missing color tokens, as `(instance path, message)` pairs in the
/// order the TypeBox `ThemeJsonSchema` walk reports them.
fn collect_schema_errors(value: &serde_json::Value, missing: &mut Vec<String>) -> Vec<(String, String)> {
    let mut errors = Vec::new();
    let Some(root) = value.as_object() else {
        errors.push((String::new(), "must be object".to_owned()));
        return errors;
    };
    let absent: Vec<&str> = ["name", "colors"].into_iter().filter(|key| !root.contains_key(*key)).collect();
    if !absent.is_empty() {
        errors.push((String::new(), format!("must have required properties {}", absent.join(", "))));
    }
    if root.get("$schema").is_some_and(|schema| !schema.is_string()) {
        errors.push(("/$schema".to_owned(), "must be string".to_owned()));
    }
    if root.get("name").is_some_and(|name| !name.is_string()) {
        errors.push(("/name".to_owned(), "must be string".to_owned()));
    }
    let mut record = |key: &str, required: &[&str], optional_keys: Option<&[&str]>| {
        let Some(entry) = root.get(key) else { return };
        let Some(object) = entry.as_object() else {
            errors.push((format!("/{key}"), "must be object".to_owned()));
            return;
        };
        for token in required {
            if !object.contains_key(*token) {
                missing.push((*token).to_owned());
            }
        }
        for (name, value) in object {
            let checked = optional_keys.is_none_or(|keys| keys.iter().any(|known| *known == name.as_str()) || required.iter().any(|known| *known == name.as_str()));
            if checked && !is_color_value(value) {
                errors.push((format!("/{key}/{name}"), "must match a schema in anyOf".to_owned()));
            }
        }
    };
    record("vars", &[][..], None);
    let required: Vec<&str> = super::theme::ThemeColor::ALL
        .iter()
        .map(|c| c.key())
        .chain(super::theme::ThemeBg::ALL.iter().map(|c| c.key()))
        .filter(|key| !OPTIONAL_COLORS.contains(key))
        .collect();
    record("colors", &required[..], Some(&OPTIONAL_COLORS[..]));
    let export_keys = ["pageBg", "cardBg", "infoBg"];
    if let Some(export) = root.get("export") {
        match export.as_object() {
            None => errors.push(("/export".to_owned(), "must be object".to_owned())),
            Some(object) => {
                for (name, value) in object {
                    if export_keys.iter().any(|known| *known == name.as_str()) && !is_color_value(value) {
                        errors.push((format!("/export/{name}"), "must match a schema in anyOf".to_owned()));
                    }
                }
            }
        }
    }
    errors
}

/// senpi `validateThemeJson`: validate one theme document, failing with a message that names the
/// offending tokens.
pub fn validate_theme_json(label: &str, value: serde_json::Value) -> Result<ThemeJson, ThemeError> {
    let mut missing = Vec::new();
    let other_errors = collect_schema_errors(&value, &mut missing);
    if !missing.is_empty() || !other_errors.is_empty() {
        let mut message = format!("Invalid theme \"{label}\":\n");
        if !missing.is_empty() {
            missing.sort();
            missing.dedup();
            message.push_str("\nMissing required color tokens:\n");
            message.push_str(&missing.iter().map(|color| format!("  - {color}")).collect::<Vec<_>>().join("\n"));
            message.push_str("\n\nPlease add these colors to your theme's \"colors\" object.");
            message.push_str("\nSee the built-in themes (dark.json, light.json) for reference values.");
        }
        if !other_errors.is_empty() {
            let lines: Vec<String> = other_errors
                .into_iter()
                .map(|(path, error)| format!("  - {}: {error}", if path.is_empty() { "/" } else { path.as_str() }))
                .collect();
            message.push_str(&format!("\n\nOther errors:\n{}", lines.join("\n")));
        }
        return Err(ThemeError::Schema(message));
    }
    let document: ThemeJson = serde_json::from_value(value)?;
    if document.name.contains('/') {
        return Err(ThemeError::Schema(format!(
            "Invalid theme name \"{}\": theme names cannot contain \"/\" because it is reserved for automatic light/dark theme settings.",
            document.name
        )));
    }
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark() -> serde_json::Value {
        serde_json::from_str(include_str!("dark.json")).expect("dark theme json")
    }

    #[test]
    fn missing_tokens_are_listed_sorted_with_the_upstream_hint() {
        let mut doc = dark();
        let colors = doc["colors"].as_object_mut().expect("colors object");
        colors.remove("text");
        colors.remove("accent");
        let error = validate_theme_json("mine", doc).expect_err("missing tokens").to_string();
        assert_eq!(
            error,
            "Invalid theme \"mine\":\n\nMissing required color tokens:\n  - accent\n  - text\n\nPlease add these colors to your theme's \"colors\" object.\nSee the built-in themes (dark.json, light.json) for reference values."
        );
    }

    #[test]
    fn wrong_types_are_reported_with_their_instance_path() {
        let mut doc = dark();
        doc["colors"]["accent"] = serde_json::json!(true);
        doc["name"] = serde_json::json!(7);
        let error = validate_theme_json("mine", doc).expect_err("wrong types").to_string();
        assert_eq!(
            error,
            "Invalid theme \"mine\":\n\n\nOther errors:\n  - /name: must be string\n  - /colors/accent: must match a schema in anyOf"
        );
    }
}
