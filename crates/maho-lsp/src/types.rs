//! LSP protocol shapes used by the tools.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationLink {
    pub target_uri: String,
    pub target_range: Range,
    pub target_selection_range: Range,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_selection_range: Option<Range>,
}

/// A definition target: `Location` or `LocationLink`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnyLocation {
    Link(LocationLink),
    Plain(Location),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolInfo {
    pub name: String,
    pub kind: u32,
    pub location: Location,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: u32,
    pub range: Range,
    pub selection_range: Range,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<DocumentSymbol>>,
}

/// `textDocument/documentSymbol` item: hierarchical or flat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnySymbol {
    Document(DocumentSymbol),
    Info(SymbolInfo),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

/// Severity filter of `lsp_diagnostics`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeverityFilter {
    Error,
    Warning,
    Information,
    Hint,
    All,
}

impl SeverityFilter {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("error") => Self::Error,
            Some("warning") => Self::Warning,
            Some("information") => Self::Information,
            Some("hint") => Self::Hint,
            _ => Self::All,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Information => "information",
            Self::Hint => "hint",
            Self::All => "all",
        }
    }
    pub fn level(self) -> Option<u32> {
        match self {
            Self::Error => Some(1),
            Self::Warning => Some(2),
            Self::Information => Some(3),
            Self::Hint => Some(4),
            Self::All => None,
        }
    }
}
