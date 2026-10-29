//! Section-wise config parsing: keep every valid top-level section when the whole config fails.

use serde_json::{Map, Value};

use crate::deep_merge::is_unsafe_object_key;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigParseIssue {
    pub path: Vec<String>,
    pub message: String,
}

/// A schema that can validate a whole config object (the Rust shape of zod's `safeParse`).
///
/// Implementations return the parsed object on success or every issue found on failure;
/// issue paths start with the top-level section key.
pub trait ConfigSectionParser {
    fn safe_parse(&self, input: &Value) -> Result<Map<String, Value>, Vec<ConfigParseIssue>>;
}

pub type InvalidSectionsFn<'a> = &'a dyn Fn(&[String]);

#[derive(Default)]
pub struct ParseConfigSectionsOptions<'a> {
    pub on_invalid_sections: Option<InvalidSectionsFn<'a>>,
}

pub fn parse_config_sections(
    schema: &dyn ConfigSectionParser,
    raw_config: &Map<String, Value>,
    options: &ParseConfigSectionsOptions<'_>,
) -> Map<String, Value> {
    if let Ok(data) = schema.safe_parse(&Value::Object(raw_config.clone())) {
        return data;
    }
    let mut partial = Map::new();
    let mut invalid_sections = Vec::new();
    for (key, value) in raw_config {
        if is_unsafe_object_key(key) {
            continue;
        }
        let mut section = Map::new();
        section.insert(key.clone(), value.clone());
        match schema.safe_parse(&Value::Object(section)) {
            Ok(mut data) => {
                if let Some(parsed) = data.remove(key) {
                    partial.insert(key.clone(), parsed);
                }
            }
            Err(issues) => {
                let section_errors = issues
                    .iter()
                    .filter(|issue| issue.path.first() == Some(key))
                    .map(|issue| format!("{}: {}", issue.path.join("."), issue.message))
                    .collect::<Vec<_>>()
                    .join(", ");
                if !section_errors.is_empty() {
                    invalid_sections.push(format!("{key}: {section_errors}"));
                }
            }
        }
    }
    if !invalid_sections.is_empty()
        && let Some(callback) = options.on_invalid_sections
    {
        callback(&invalid_sections);
    }
    partial
}
