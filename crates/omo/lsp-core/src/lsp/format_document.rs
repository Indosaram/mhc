//! Port of TS `lsp/format-document.ts`: whole-document formatting workflow,
//! line delta calculation, and atomic write with mtime preservation.

use crate::abort::AbortSignal;
use crate::lsp::client::LspClient;
use crate::lsp::errors::LspError;
use crate::lsp::types::FormattingOptions;
use crate::lsp::workspace_edit::text::normalize_text_edits;
use crate::lsp::workspace_edit::types::ParsedTextEdit;
use serde::Deserialize;
use serde::Serialize;
use std::fs;

/// TS `DEFAULT_FORMATTING_OPTIONS`.
pub const DEFAULT_FORMATTING_OPTIONS: FormattingOptions = FormattingOptions {
    tab_size: 4,
    insert_spaces: false,
    trim_trailing_whitespace: Some(true),
    insert_final_newline: Some(true),
    trim_final_newlines: Some(true),
};

/// TS `FormatDocumentResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FormatDocumentResult {
    Formatted {
        #[serde(rename = "linesAdded")]
        lines_added: usize,
        #[serde(rename = "linesRemoved")]
        lines_removed: usize,
    },
    Unchanged {
        #[serde(rename = "linesAdded")]
        lines_added: usize,
        #[serde(rename = "linesRemoved")]
        lines_removed: usize,
    },
    Unavailable {
        reason: &'static str,
    },
}

impl FormatDocumentResult {
    pub const UNCHANGED: Self = Self::Unchanged {
        lines_added: 0,
        lines_removed: 0,
    };
    pub const CAPABILITY_NOT_ADVERTISED: Self = Self::Unavailable {
        reason: "capability_not_advertised",
    };

    pub fn status_str(&self) -> &'static str {
        match self {
            Self::Formatted { .. } => "formatted",
            Self::Unchanged { .. } => "unchanged",
            Self::Unavailable { .. } => "unavailable",
        }
    }

    pub fn lines_added(&self) -> usize {
        match self {
            Self::Formatted { lines_added, .. } | Self::Unchanged { lines_added, .. } => *lines_added,
            Self::Unavailable { .. } => 0,
        }
    }

    pub fn lines_removed(&self) -> usize {
        match self {
            Self::Formatted { lines_removed, .. } | Self::Unchanged { lines_removed, .. } => *lines_removed,
            Self::Unavailable { .. } => 0,
        }
    }

    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Unavailable { reason } => Some(*reason),
            _ => None,
        }
    }
}

/// TS `FormatDocumentOptions`.
#[derive(Debug, Clone, Default)]
pub struct FormatDocumentOptions<'a> {
    pub formatting_options: Option<FormattingOptions>,
    pub signal: Option<&'a AbortSignal>,
}

/// TS `formatDocumentWithClient`.
///
/// Formats one document through the resident language server and commits the result to disk.
///
/// The server is asked for whole-document edits rather than formatted text, so the edits are
/// validated and applied with the same normalization the workspace-edit path uses. Nothing is
/// written when the server declines the capability or when the edits reproduce the current bytes,
/// which keeps a no-op format from touching file mtimes that downstream tooling watches.
pub async fn format_document_with_client(
    client: &LspClient,
    file_path: &str,
    options: FormatDocumentOptions<'_>,
) -> Result<FormatDocumentResult, LspError> {
    let formatting_options = options
        .formatting_options
        .unwrap_or(DEFAULT_FORMATTING_OPTIONS);
    let edits = client
        .format_document(file_path, formatting_options, options.signal)
        .await?;
    let Some(edits) = edits else {
        return Ok(FormatDocumentResult::CAPABILITY_NOT_ADVERTISED);
    };
    if edits.is_empty() {
        return Ok(FormatDocumentResult::UNCHANGED);
    }

    let before = fs::read_to_string(file_path)
        .map_err(|err| LspError::other(format!("Failed to read file '{file_path}': {err}")))?;

    let parsed_edits: Vec<ParsedTextEdit> = edits
        .into_iter()
        .map(|edit| ParsedTextEdit {
            start_line: f64::from(edit.range.start.line),
            start_character: f64::from(edit.range.start.character),
            end_line: f64::from(edit.range.end.line),
            end_character: f64::from(edit.range.end.character),
            new_text: edit.new_text,
        })
        .collect();

    let normalized = normalize_text_edits(&before, &parsed_edits, 0)
        .map_err(|v_err| LspError::other(v_err.detail))?;

    if normalized.text == before {
        return Ok(FormatDocumentResult::UNCHANGED);
    }

    write_atomically(file_path, &normalized.text)?;
    client.open_file(file_path).await?;

    let (lines_added, lines_removed) = line_delta(&normalized.edits);
    Ok(FormatDocumentResult::Formatted {
        lines_added,
        lines_removed,
    })
}

/// TS `lineDelta`.
///
/// Counts the lines each edit removes from the document and adds back in its place.
///
/// Counting from the edits instead of diffing the whole file keeps the report proportional to what
/// the server actually rewrote, so an unrelated line never shows up as churn.
pub fn line_delta(edits: &[ParsedTextEdit]) -> (usize, usize) {
    let mut lines_added = 0;
    let mut lines_removed = 0;
    for edit in edits {
        let removed = (edit.end_line - edit.start_line + 1.0) as usize;
        lines_removed += removed;
        lines_added += edit.new_text.split('\n').count();
    }
    (lines_added, lines_removed)
}

fn write_atomically(file_path: &str, content: &str) -> Result<(), LspError> {
    let temp_path = format!("{file_path}.omo-format.tmp");
    fs::write(&temp_path, content.as_bytes())
        .map_err(|err| LspError::other(format!("Failed to write temporary file '{temp_path}': {err}")))?;

    if let Err(err) = fs::rename(&temp_path, file_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(LspError::other(format!(
            "Failed to rename '{temp_path}' to '{file_path}': {err}"
        )));
    }
    Ok(())
}
