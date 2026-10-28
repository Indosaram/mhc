//! Text rendering of LSP results (upstream wording).
use crate::client::uri_to_path;
use crate::edit::ApplyResult;
use crate::servers::SYMBOL_KINDS;
use crate::types::{AnyLocation, Diagnostic, DocumentSymbol, SeverityFilter, SymbolInfo};
use serde_json::Value;

pub fn format_location(location: &AnyLocation) -> String {
    let (uri, range) = match location {
        AnyLocation::Link(link) => (&link.target_uri, link.target_range),
        AnyLocation::Plain(plain) => (&plain.uri, plain.range),
    };
    format!(
        "{}:{}:{}",
        uri_to_path(uri).display(),
        range.start.line + 1,
        range.start.character
    )
}

pub fn format_symbol_kind(kind: u32) -> String {
    kind.checked_sub(1)
        .and_then(|index| SYMBOL_KINDS.get(index as usize))
        .map_or_else(|| format!("Unknown({kind})"), |name| (*name).to_owned())
}

pub fn format_severity(severity: Option<u32>) -> String {
    match severity {
        None | Some(0) => "unknown".to_owned(),
        Some(1) => "error".to_owned(),
        Some(2) => "warning".to_owned(),
        Some(3) => "information".to_owned(),
        Some(4) => "hint".to_owned(),
        Some(other) => format!("unknown({other})"),
    }
}

pub fn format_document_symbol(symbol: &DocumentSymbol, indent: usize) -> String {
    let mut text = format!(
        "{}{} ({}) - line {}",
        "  ".repeat(indent),
        symbol.name,
        format_symbol_kind(symbol.kind),
        symbol.range.start.line + 1
    );
    for child in symbol.children.iter().flatten() {
        text.push('\n');
        text.push_str(&format_document_symbol(child, indent + 1));
    }
    text
}

pub fn format_symbol_info(symbol: &SymbolInfo) -> String {
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
        format_location(&AnyLocation::Plain(symbol.location.clone()))
    )
}

pub fn format_diagnostic(diagnostic: &Diagnostic) -> String {
    let source = diagnostic
        .source
        .as_deref()
        .filter(|source| !source.is_empty())
        .map(|source| format!("[{source}]"))
        .unwrap_or_default();
    let code = match &diagnostic.code {
        Some(Value::String(code)) if !code.is_empty() => format!(" ({code})"),
        Some(Value::Number(code)) if code.as_f64() != Some(0.0) => format!(" ({code})"),
        _ => String::new(),
    };
    format!(
        "{}{source}{code} at {}:{}: {}",
        format_severity(diagnostic.severity),
        diagnostic.range.start.line + 1,
        diagnostic.range.start.character,
        diagnostic.message
    )
}

pub fn filter_by_severity(diagnostics: Vec<Diagnostic>, filter: SeverityFilter) -> Vec<Diagnostic> {
    match filter.level() {
        None => diagnostics,
        Some(level) => diagnostics
            .into_iter()
            .filter(|diagnostic| diagnostic.severity == Some(level))
            .collect(),
    }
}

fn range_text(range: &Value) -> Option<String> {
    let number = |path: [&str; 2]| range.get(path[0])?.get(path[1])?.as_u64();
    Some(format!(
        "{}:{}-{}:{}",
        number(["start", "line"])? + 1,
        number(["start", "character"])?,
        number(["end", "line"])? + 1,
        number(["end", "character"])?
    ))
}

/// `textDocument/prepareRename` result as text.
pub fn format_prepare_rename(result: &Value) -> String {
    const CANNOT: &str = "Cannot rename at this position";
    if result.is_null() {
        return CANNOT.to_owned();
    }
    if let Some(default) = result.get("defaultBehavior") {
        return if default.as_bool() == Some(true) {
            "Rename supported (using default behavior)".to_owned()
        } else {
            CANNOT.to_owned()
        };
    }
    if let Some(range) = result.get("range").and_then(range_text) {
        let placeholder = result
            .get("placeholder")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(|text| format!(" (current: \"{text}\")"))
            .unwrap_or_default();
        return format!("Rename available at {range}{placeholder}");
    }
    if let Some(range) = range_text(result) {
        return format!("Rename available at {range}");
    }
    CANNOT.to_owned()
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
    } else {
        lines.push("Failed to apply some changes:".to_owned());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Location, Position, Range};
    use serde_json::json;

    fn range(line: u32, character: u32) -> Range {
        Range {
            start: Position { line, character },
            end: Position {
                line,
                character: character + 1,
            },
        }
    }

    #[test]
    fn diagnostics_and_symbols() {
        let diagnostic = Diagnostic {
            range: range(13, 4),
            severity: Some(1),
            code: Some(json!(2322)),
            source: Some("ts".into()),
            message: "bad".into(),
        };
        assert_eq!(
            format_diagnostic(&diagnostic),
            "error[ts] (2322) at 14:4: bad"
        );
        let plain = Diagnostic {
            range: range(0, 0),
            severity: None,
            code: None,
            source: None,
            message: "m".into(),
        };
        assert_eq!(format_diagnostic(&plain), "unknown at 1:0: m");
        assert_eq!(format_symbol_kind(12), "Function");
        assert_eq!(format_symbol_kind(99), "Unknown(99)");
        let child = DocumentSymbol {
            name: "inner".into(),
            kind: 6,
            range: range(2, 0),
            selection_range: range(2, 0),
            children: None,
        };
        let parent = DocumentSymbol {
            name: "Outer".into(),
            kind: 5,
            range: range(0, 0),
            selection_range: range(0, 0),
            children: Some(vec![child]),
        };
        assert_eq!(
            format_document_symbol(&parent, 0),
            "Outer (Class) - line 1\n  inner (Method) - line 3"
        );
        let info = SymbolInfo {
            name: "f".into(),
            kind: 12,
            location: Location {
                uri: "file:///w/a.rs".into(),
                range: range(4, 2),
            },
            container_name: Some("m".into()),
        };
        assert_eq!(
            format_symbol_info(&info),
            "f (Function) (in m) - /w/a.rs:5:2"
        );
    }

    #[test]
    fn prepare_rename_shapes() {
        assert_eq!(
            format_prepare_rename(&Value::Null),
            "Cannot rename at this position"
        );
        assert_eq!(
            format_prepare_rename(&json!({"defaultBehavior": true})),
            "Rename supported (using default behavior)"
        );
        let range = json!({"start":{"line":41,"character":6},"end":{"line":41,"character":12}});
        assert_eq!(
            format_prepare_rename(&json!({"range": range, "placeholder": "oldFoo"})),
            "Rename available at 42:6-42:12 (current: \"oldFoo\")"
        );
        assert_eq!(
            format_prepare_rename(&range),
            "Rename available at 42:6-42:12"
        );
    }
}
