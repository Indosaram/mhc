use crate::lsp::types::Diagnostic;
use serde_json::Value;
use std::collections::BTreeMap;

/// TS `ConfigurationItem`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfigurationItem {
    pub section: Option<String>,
}

/// TS `DiagnosticsParams` (`textDocument/publishDiagnostics`).
#[derive(Debug, Clone, PartialEq)]
pub struct DiagnosticsParams {
    pub uri: String,
    pub diagnostics: Vec<Diagnostic>,
    pub version: Option<f64>,
}

/// TS `parseConfigurationItems`.
pub fn parse_configuration_items(params: &Value) -> Vec<ConfigurationItem> {
    let Some(items) = params.get("items").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|item| item.is_object())
        .map(|item| ConfigurationItem {
            section: item
                .get("section")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
        .collect()
}

/// TS `parseDiagnosticsParams`: drops malformed diagnostics.
pub fn parse_diagnostics_params(params: &Value) -> Option<DiagnosticsParams> {
    let uri = params.get("uri")?.as_str()?.to_string();
    let diagnostics = params
        .get("diagnostics")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(parse_diagnostic).collect())
        .unwrap_or_default();
    Some(DiagnosticsParams {
        uri,
        diagnostics,
        version: params.get("version").and_then(Value::as_f64),
    })
}

/// Accepts a value only if it has a numeric `range` and a string `message` (TS `isDiagnostic`).
pub fn parse_diagnostic(value: &Value) -> Option<Diagnostic> {
    let range = value.get("range")?;
    let position_ok = |position: Option<&Value>| {
        position.is_some_and(|position| {
            position.get("line").is_some_and(Value::is_number)
                && position.get("character").is_some_and(Value::is_number)
        })
    };
    if !position_ok(range.get("start")) || !position_ok(range.get("end")) {
        return None;
    }
    value.get("message")?.as_str()?;
    serde_json::from_value(value.clone()).ok()
}

/// TS `createLspSpawnEnv`: currently a copy of the input environment.
pub fn create_lsp_spawn_env(
    _root: &str,
    input: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    input.clone()
}
