use crate::lsp::language_mappings::severity_name;
use crate::lsp::language_mappings::symbol_kind_name;
use crate::lsp::types::Diagnostic;
use crate::lsp::types::DocumentSymbol;
use crate::lsp::types::SeverityFilter;
use crate::lsp::types::SymbolInfo;
use crate::lsp::workspace_edit::ApplyResult;
use serde_json::Value;

/// TS `uriToPath` (`fileURLToPath`); non-file URIs are returned unchanged.
pub fn uri_to_path(uri: &str) -> String {
    url::Url::parse(uri)
        .ok()
        .and_then(|url| url.to_file_path().ok())
        .map_or_else(
            || uri.to_string(),
            |path| path.to_string_lossy().into_owned(),
        )
}

fn position_of(value: &Value) -> (i64, i64) {
    let line = value.get("line").and_then(Value::as_i64).unwrap_or(0);
    let character = value.get("character").and_then(Value::as_i64).unwrap_or(0);
    (line, character)
}

/// TS `formatLocation` for a `Location` or `LocationLink` value.
pub fn format_location(location: &Value) -> String {
    let (uri, range) = match location.get("targetUri").and_then(Value::as_str) {
        Some(target_uri) => (target_uri, location.get("targetRange")),
        None => (
            location
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            location.get("range"),
        ),
    };
    let (line, character) = range
        .and_then(|range| range.get("start"))
        .map(position_of)
        .unwrap_or_default();
    format!("{}:{}:{character}", uri_to_path(uri), line + 1)
}

pub fn format_symbol_kind(kind: u32) -> String {
    symbol_kind_name(kind).map_or_else(|| format!("Unknown({kind})"), str::to_string)
}

pub fn format_severity(severity: Option<u32>) -> String {
    match severity {
        None | Some(0) => "unknown".to_string(),
        Some(severity) => {
            severity_name(severity).map_or_else(|| format!("unknown({severity})"), str::to_string)
        }
    }
}

pub fn format_document_symbol(symbol: &DocumentSymbol, indent: usize) -> String {
    let mut result = format!(
        "{}{} ({}) - line {}",
        "  ".repeat(indent),
        symbol.name,
        format_symbol_kind(symbol.kind),
        symbol.range.start.line + 1
    );
    for child in symbol.children.iter().flatten() {
        result.push('\n');
        result.push_str(&format_document_symbol(child, indent + 1));
    }
    result
}

pub fn format_symbol_info(symbol: &SymbolInfo) -> String {
    let location = serde_json::to_value(&symbol.location).unwrap_or(Value::Null);
    let container = symbol
        .container_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(|name| format!(" (in {name})"))
        .unwrap_or_default();
    format!(
        "{} ({}){container} - {}",
        symbol.name,
        format_symbol_kind(symbol.kind),
        format_location(&location)
    )
}

fn js_truthy_code(code: &Value) -> Option<String> {
    match code {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) if number.as_f64() != Some(0.0) => Some(number.to_string()),
        Value::Bool(true) => Some("true".to_string()),
        Value::Array(_) | Value::Object(_) => Some(code.to_string()),
        _ => None,
    }
}

pub fn format_diagnostic(diagnostic: &Diagnostic) -> String {
    let source = diagnostic
        .source
        .as_deref()
        .filter(|source| !source.is_empty())
        .map(|source| format!("[{source}]"))
        .unwrap_or_default();
    let code = diagnostic
        .code
        .as_ref()
        .and_then(js_truthy_code)
        .map(|code| format!(" ({code})"))
        .unwrap_or_default();
    format!(
        "{}{source}{code} at {}:{}: {}",
        format_severity(diagnostic.severity),
        diagnostic.range.start.line + 1,
        diagnostic.range.start.character,
        diagnostic.message
    )
}

pub fn filter_diagnostics_by_severity(
    diagnostics: Vec<Diagnostic>,
    filter: Option<SeverityFilter>,
) -> Vec<Diagnostic> {
    let target = match filter {
        None | Some(SeverityFilter::All) => return diagnostics,
        Some(SeverityFilter::Error) => 1,
        Some(SeverityFilter::Warning) => 2,
        Some(SeverityFilter::Information) => 3,
        Some(SeverityFilter::Hint) => 4,
    };
    diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.severity == Some(target))
        .collect()
}

fn format_range(range: &Value) -> Option<String> {
    let start = range.get("start")?;
    let end = range.get("end")?;
    let (start_line, start_char) = position_of(start);
    let (end_line, end_char) = position_of(end);
    Some(format!(
        "{}:{start_char}-{}:{end_char}",
        start_line + 1,
        end_line + 1
    ))
}

/// TS `formatPrepareRenameResult` over the raw `textDocument/prepareRename` result.
pub fn format_prepare_rename_result(result: &Value) -> String {
    const CANNOT: &str = "Cannot rename at this position";
    let Some(object) = result.as_object() else {
        return CANNOT.to_string();
    };
    if let Some(default_behavior) = object.get("defaultBehavior") {
        return if default_behavior.as_bool() == Some(true) {
            "Rename supported (using default behavior)".to_string()
        } else {
            CANNOT.to_string()
        };
    }
    if let Some(range) = object.get("range").filter(|range| !range.is_null()) {
        let placeholder = object
            .get("placeholder")
            .and_then(Value::as_str)
            .filter(|placeholder| !placeholder.is_empty())
            .map(|placeholder| format!(" (current: \"{placeholder}\")"))
            .unwrap_or_default();
        if let Some(range) = format_range(range) {
            return format!("Rename available at {range}{placeholder}");
        }
    }
    if let Some(range) = format_range(result) {
        return format!("Rename available at {range}");
    }
    CANNOT.to_string()
}

pub fn format_apply_result(result: &ApplyResult) -> String {
    let mut lines = Vec::new();
    if result.success {
        lines.push(format!(
            "Applied {} edit(s) to {} file(s):",
            result.total_edits,
            result.files_modified.len()
        ));
        lines.extend(
            result
                .files_modified
                .iter()
                .map(|file| format!("  - {file}")),
        );
        if result.late_abort == Some(true) {
            lines.push(
                "Cancellation arrived after the filesystem commit began; the committed edit completed.".to_string(),
            );
        }
    } else {
        lines.push("Failed to apply some changes:".to_string());
        lines.extend(
            result
                .errors
                .iter()
                .map(|error| format!("  Error: {error}")),
        );
        if !result.files_modified.is_empty() {
            lines.push(format!(
                "Successfully modified: {}",
                result.files_modified.join(", ")
            ));
        }
    }
    lines.join("\n")
}
