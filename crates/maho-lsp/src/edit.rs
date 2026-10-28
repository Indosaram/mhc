//! Applying a `WorkspaceEdit` to disk.
use crate::client::uri_to_path;
use crate::types::TextEdit;
use serde::Serialize;
use serde_json::Value;

/// Outcome of [`apply_workspace_edit`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub success: bool,
    pub files_modified: Vec<String>,
    pub total_edits: usize,
    pub errors: Vec<String>,
}

/// Byte index of UTF-16 column `column` in `line` (clamped to the line end).
fn utf16_byte_index(line: &str, column: u32) -> usize {
    let mut units = 0u32;
    for (index, character) in line.char_indices() {
        if units >= column {
            return index;
        }
        units += character.len_utf16() as u32;
    }
    line.len()
}

/// Applies `edits` (positions in UTF-16 columns) to `content`, last edit first.
pub fn apply_text_edits(content: &str, edits: &[TextEdit]) -> String {
    let mut lines: Vec<String> = content.split('\n').map(str::to_owned).collect();
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by(|a, b| {
        (b.range.start.line, b.range.start.character)
            .cmp(&(a.range.start.line, a.range.start.character))
    });
    for edit in sorted {
        let (start_line, end_line) = (edit.range.start.line as usize, edit.range.end.line as usize);
        while lines.len() <= end_line.max(start_line) {
            lines.push(String::new());
        }
        let first = lines[start_line].clone();
        let last = lines[end_line].clone();
        let prefix = &first[..utf16_byte_index(&first, edit.range.start.character)];
        let suffix = &last[utf16_byte_index(&last, edit.range.end.character)..];
        let replaced = format!("{prefix}{}{suffix}", edit.new_text);
        let end = end_line.max(start_line);
        lines.splice(start_line..=end, replaced.split('\n').map(str::to_owned));
    }
    lines.join("\n")
}

fn apply_to_file(path: &std::path::Path, edits: &[TextEdit]) -> Result<usize, String> {
    let content = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    std::fs::write(path, apply_text_edits(&content, edits)).map_err(|error| error.to_string())?;
    Ok(edits.len())
}

fn parse_edits(value: Option<&Value>) -> Vec<TextEdit> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value::<TextEdit>(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Applies `changes` and `documentChanges` (text edits, create, rename, delete).
pub fn apply_workspace_edit(edit: &Value) -> ApplyResult {
    if edit.is_null() {
        return ApplyResult {
            success: false,
            errors: vec!["No edit provided".to_owned()],
            ..ApplyResult::default()
        };
    }
    let mut result = ApplyResult {
        success: true,
        ..ApplyResult::default()
    };
    let record = |path: std::path::PathBuf,
                  outcome: Result<usize, String>,
                  result: &mut ApplyResult| match outcome {
        Ok(count) => {
            result.files_modified.push(path.display().to_string());
            result.total_edits += count;
        }
        Err(error) => {
            result.success = false;
            result.errors.push(format!("{}: {error}", path.display()));
        }
    };
    if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            let path = uri_to_path(uri);
            let outcome = apply_to_file(&path, &parse_edits(Some(edits)));
            record(path, outcome, &mut result);
        }
    }
    for change in edit
        .get("documentChanges")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let uri_of = |key: &str| {
            change
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        match change.get("kind").and_then(Value::as_str) {
            None => {
                let uri = change
                    .get("textDocument")
                    .and_then(|doc| doc.get("uri"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let path = uri_to_path(uri);
                let outcome = apply_to_file(&path, &parse_edits(change.get("edits")));
                record(path, outcome, &mut result);
            }
            Some("create") => {
                let path = uri_to_path(&uri_of("uri"));
                match std::fs::write(&path, "") {
                    Ok(()) => result.files_modified.push(path.display().to_string()),
                    Err(error) => {
                        result.success = false;
                        result
                            .errors
                            .push(format!("Create {}: {error}", uri_of("uri")));
                    }
                }
            }
            Some("rename") => {
                let (old, new) = (
                    uri_to_path(&uri_of("oldUri")),
                    uri_to_path(&uri_of("newUri")),
                );
                let moved = std::fs::read(&old)
                    .and_then(|bytes| std::fs::write(&new, bytes))
                    .and_then(|()| std::fs::remove_file(&old));
                match moved {
                    Ok(()) => result.files_modified.push(new.display().to_string()),
                    Err(error) => {
                        result.success = false;
                        result
                            .errors
                            .push(format!("Rename {}: {error}", uri_of("oldUri")));
                    }
                }
            }
            Some("delete") => {
                let path = uri_to_path(&uri_of("uri"));
                match std::fs::remove_file(&path) {
                    Ok(()) => result.files_modified.push(path.display().to_string()),
                    Err(error) => {
                        result.success = false;
                        result
                            .errors
                            .push(format!("Delete {}: {error}", uri_of("uri")));
                    }
                }
            }
            Some(_) => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::path_to_uri;
    use crate::types::{Position, Range};
    use serde_json::json;

    fn edit(start: (u32, u32), end: (u32, u32), text: &str) -> TextEdit {
        TextEdit {
            range: Range {
                start: Position {
                    line: start.0,
                    character: start.1,
                },
                end: Position {
                    line: end.0,
                    character: end.1,
                },
            },
            new_text: text.to_owned(),
        }
    }

    #[test]
    fn edits_apply_bottom_up_with_utf16_columns() {
        let content = "let a = 1;\nlet é😀b = a;\n";
        let edits = [
            edit((0, 4), (0, 5), "x"),
            edit((1, 7), (1, 8), "y"),
            edit((1, 11), (1, 12), "x"),
        ];
        assert_eq!(
            apply_text_edits(content, &edits),
            "let x = 1;\nlet é😀y = x;\n"
        );
        assert_eq!(
            apply_text_edits("a\nb\nc", &[edit((0, 1), (2, 0), "-")]),
            "a-c"
        );
    }

    #[test]
    fn workspace_edit_changes_and_document_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "old here").expect("write a");
        std::fs::write(&b, "old there").expect("write b");
        let moved = dir.path().join("c.txt");
        let edit_value = json!({
            "changes": {path_to_uri(&a): [{"range": {"start": {"line":0,"character":0},"end":{"line":0,"character":3}}, "newText": "new"}]},
            "documentChanges": [
                {"textDocument": {"uri": path_to_uri(&b), "version": 1}, "edits": [{"range": {"start": {"line":0,"character":0},"end":{"line":0,"character":3}}, "newText": "new"}]},
                {"kind": "rename", "oldUri": path_to_uri(&b), "newUri": path_to_uri(&moved)},
                {"kind": "delete", "uri": path_to_uri(&dir.path().join("missing"))}
            ]
        });
        let result = apply_workspace_edit(&edit_value);
        assert!(!result.success);
        assert_eq!(result.total_edits, 2);
        assert_eq!(result.files_modified.len(), 3);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(std::fs::read_to_string(&a).expect("a"), "new here");
        assert_eq!(std::fs::read_to_string(&moved).expect("c"), "new there");
        assert!(!b.exists());
        assert_eq!(
            apply_workspace_edit(&Value::Null).errors,
            vec!["No edit provided"]
        );
    }
}
