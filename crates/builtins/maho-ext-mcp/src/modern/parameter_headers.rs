//! Port of senpi mcp/modern/parameter-headers.ts: declared `x-mcp-header` tool parameters.
use base64::Engine;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParameterType {
    String,
    Integer,
    Boolean,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct ParameterHeader {
    name: String,
    path: Vec<String>,
    kind: ParameterType,
}

static FIELD_TOKEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$").expect("token pattern")
});

/// RFC 9110 field values, preserving whitespace and literal sentinel strings.
pub fn encode_mcp_header_value(value: &str) -> String {
    let visible = value.chars().all(|c| c == '\t' || (' '..='~').contains(&c));
    // JS `trim()` strips Unicode whitespace too, but those already fail the ASCII check.
    if visible
        && value.trim_matches(maho_ai::utils::js::is_js_whitespace) == value
        && !(value.starts_with("=?base64?") && value.ends_with("?="))
    {
        value.into()
    } else {
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
        )
    }
}

/// Configured headers must never supply parameter metadata absent from the request body.
pub fn clear_mcp_parameter_headers(headers: &mut BTreeMap<String, String>) {
    headers.retain(|name, _| !name.to_ascii_lowercase().starts_with("mcp-param-"));
}

fn parse_parameter_headers(schema: &Value) -> Result<Vec<ParameterHeader>, String> {
    let mut headers = Vec::new();
    let mut names = BTreeSet::new();
    fn visit(
        node: &Value,
        path: Option<Vec<String>>,
        headers: &mut Vec<ParameterHeader>,
        names: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if let Some(children) = node.as_array() {
            return children
                .iter()
                .try_for_each(|child| visit(child, None, headers, names));
        }
        let Some(node) = node.as_object() else {
            return Ok(());
        };
        if let Some(name) = node.get("x-mcp-header") {
            let kind = match node.get("type").and_then(Value::as_str) {
                Some("string") => Some(ParameterType::String),
                Some("integer") => Some(ParameterType::Integer),
                Some("boolean") => Some(ParameterType::Boolean),
                _ => None,
            };
            let Some(path) = path.as_ref().filter(|path| !path.is_empty()) else {
                return Err("x-mcp-header must be on a properties-only path".into());
            };
            let Some(name) = name.as_str().filter(|name| FIELD_TOKEN.is_match(name)) else {
                return Err("x-mcp-header must be a nonempty HTTP field-name token".into());
            };
            if names.contains(&name.to_ascii_lowercase()) {
                return Err("x-mcp-header names must be case-insensitively unique".into());
            }
            let Some(kind) = kind else {
                return Err("x-mcp-header type must be string, integer, or boolean".into());
            };
            names.insert(name.to_ascii_lowercase());
            headers.push(ParameterHeader {
                name: name.into(),
                path: path.clone(),
                kind,
            });
        }
        // "Anywhere else" includes unknown keywords; instance-data keywords are not schemas.
        for (keyword, value) in node {
            if [
                "properties",
                "patternProperties",
                "$defs",
                "definitions",
                "dependentSchemas",
                "dependencies",
            ]
            .contains(&keyword.as_str())
            {
                let Some(children) = value.as_object() else {
                    continue;
                };
                for (key, child) in children {
                    let child_path = (keyword == "properties")
                        .then_some(path.as_ref())
                        .flatten()
                        .map(|path| path.iter().cloned().chain([key.clone()]).collect());
                    visit(child, child_path, headers, names)?;
                }
            } else if !["examples", "default", "const", "enum"].contains(&keyword.as_str()) {
                visit(value, None, headers, names)?;
            }
        }
        Ok(())
    }
    visit(schema, Some(Vec::new()), &mut headers, &mut names)?;
    Ok(headers)
}

/// A local argument error, not a failed transport or an untrusted server diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ParameterHeaderValidationError(pub String);

/// Connection-local catalog: never shared with another server or transport generation.
#[derive(Default)]
pub struct ModernParameterHeaders {
    tools: Mutex<BTreeMap<String, Vec<ParameterHeader>>>,
}
impl ModernParameterHeaders {
    pub fn filter_tools(
        &self,
        tools: &[Value],
        first_page: bool,
        warn: &dyn Fn(&str),
    ) -> Vec<Value> {
        let mut catalog = self
            .tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if first_page {
            catalog.clear();
        }
        tools
            .iter()
            .filter(|tool| {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    return true;
                };
                match parse_parameter_headers(tool.get("inputSchema").unwrap_or(&Value::Null)) {
                    Err(reason) => {
                        catalog.remove(name);
                        warn(&format!(
                            "Rejected MCP tool {}: {reason}",
                            Value::String(name.into())
                        ));
                        false
                    }
                    Ok(headers) => {
                        catalog.insert(name.into(), headers);
                        true
                    }
                }
            })
            .cloned()
            .collect()
    }
    pub fn append(
        &self,
        headers: &mut BTreeMap<String, String>,
        name: Option<&Value>,
        args: Option<&Value>,
    ) -> Result<(), ParameterHeaderValidationError> {
        let Some(name) = name.and_then(Value::as_str) else {
            return Ok(());
        };
        let catalog = self
            .tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for parameter in catalog.get(name).map(Vec::as_slice).unwrap_or_default() {
            let mut value = args;
            for key in &parameter.path {
                value = value
                    .and_then(|value| value.as_object())
                    .and_then(|value| value.get(key));
            }
            let Some(value) = value else {
                continue;
            };
            let text = match (parameter.kind, value) {
                (ParameterType::String, Value::String(text)) => Some(text.clone()),
                (ParameterType::Boolean, Value::Bool(flag)) => Some(flag.to_string()),
                (ParameterType::Integer, Value::Number(number)) => number
                    .as_f64()
                    .filter(|number| {
                        number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_991.0
                    })
                    .map(maho_ai::utils::js::number_to_string),
                _ => None,
            };
            let Some(text) = text else {
                let path = serde_json::to_string(&parameter.path).unwrap_or_default();
                let expected = match parameter.kind {
                    ParameterType::Integer => "a safe integer",
                    ParameterType::String => "a string",
                    ParameterType::Boolean => "a boolean",
                };
                return Err(ParameterHeaderValidationError(format!(
                    "MCP tool {} argument {path} must be {expected}",
                    Value::String(name.into())
                )));
            };
            headers.insert(
                format!("Mcp-Param-{}", parameter.name),
                encode_mcp_header_value(&text),
            );
        }
        Ok(())
    }
}
