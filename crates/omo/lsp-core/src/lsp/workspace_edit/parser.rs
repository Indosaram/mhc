//! Port of TS `workspace-edit-parser.ts`, `workspace-edit-parse-helpers.ts` and
//! `workspace-edit-resource-parser.ts`.

use super::path::uri_to_canonical_workspace_path;
use super::types::ApplyResult;
use super::types::ParseFailure;
use super::types::ParsedTextEdit;
use super::types::ParsedWorkspaceEdit;
use super::types::ParsedWorkspaceOperation;
use super::types::WorkspaceEditValidationError as VErr;
use serde_json::Map;
use serde_json::Value;

type Record = Map<String, Value>;

/// TS `failureResult`: failures sorted by change index, first one is `failedChange`.
pub fn failure_result(failures: &[ParseFailure]) -> ApplyResult {
    let mut sorted = failures.to_vec();
    sorted.sort_by_key(|failure| failure.change_index);
    ApplyResult {
        success: false,
        files_modified: Vec::new(),
        total_edits: 0,
        errors: sorted
            .iter()
            .map(|failure| format!("change {}: {}", failure.change_index, failure.message))
            .collect(),
        failed_change: sorted.first().map(|failure| failure.change_index),
        late_abort: None,
    }
}

fn single_failure(message: &str) -> ParsedWorkspaceEdit {
    ParsedWorkspaceEdit {
        operations: Vec::new(),
        failures: vec![ParseFailure {
            change_index: 0,
            message: message.to_string(),
        }],
    }
}

/// TS `parseWorkspaceEdit` over an untrusted JSON value.
pub fn parse_workspace_edit(edit: &Value, workspace_root: &str) -> ParsedWorkspaceEdit {
    let Some(edit) = edit.as_object() else {
        return single_failure("No edit provided");
    };
    if edit.contains_key("changeAnnotations") {
        return single_failure("change annotations are unsupported");
    }
    let changes = edit.get("changes");
    let document_changes = edit.get("documentChanges");
    if changes.is_some() && document_changes.is_some() {
        return single_failure("changes and documentChanges cannot be combined");
    }
    let mut target = ParsedWorkspaceEdit::default();
    if let Some(changes) = changes {
        parse_changes(changes, workspace_root, &mut target);
    } else if let Some(document_changes) = document_changes {
        parse_document_changes(document_changes, workspace_root, &mut target);
    }
    target
}

fn push_failure(target: &mut ParsedWorkspaceEdit, change_index: usize, message: String) {
    target.failures.push(ParseFailure {
        change_index,
        message,
    });
}

fn parse_changes(value: &Value, workspace_root: &str, target: &mut ParsedWorkspaceEdit) {
    let Some(record) = value.as_object() else {
        *target = single_failure("changes must be an object");
        return;
    };
    let mut entries: Vec<(&String, &Value)> = record.iter().collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    for (change_index, (uri, raw_edits)) in entries.into_iter().enumerate() {
        let resolved = match uri_to_canonical_workspace_path(uri, workspace_root) {
            Ok(resolved) => resolved,
            Err(error) => {
                push_failure(target, change_index, error);
                continue;
            }
        };
        match parse_text_edits(raw_edits, change_index) {
            Ok(edits) => target.operations.push(ParsedWorkspaceOperation::Text {
                change_index,
                path: resolved.path,
                reported_path: resolved.requested_path,
                edits,
                version: None,
            }),
            Err(error) => push_failure(target, change_index, error.detail),
        }
    }
}

fn parse_document_changes(value: &Value, workspace_root: &str, target: &mut ParsedWorkspaceEdit) {
    let Some(changes) = value.as_array() else {
        *target = single_failure("documentChanges must be an array");
        return;
    };
    for (change_index, change) in changes.iter().enumerate() {
        if let Err(error) = parse_document_change(change, change_index, workspace_root, target) {
            push_failure(target, change_index, error.detail);
        }
    }
}

fn parse_document_change(
    change: &Value,
    change_index: usize,
    workspace_root: &str,
    target: &mut ParsedWorkspaceEdit,
) -> Result<(), VErr> {
    let Some(change) = change.as_object() else {
        return Err(VErr::new(change_index, "document change must be an object"));
    };
    if change.contains_key("annotationId") {
        return Err(VErr::new(
            change_index,
            "annotated resource operations are unsupported",
        ));
    }
    if let Some(Value::String(kind)) = change.get("kind") {
        return parse_resource_change(change, kind, change_index, workspace_root, target);
    }
    let identifier = change.get("textDocument").and_then(Value::as_object);
    let Some(uri) = identifier
        .and_then(|identifier| identifier.get("uri"))
        .and_then(Value::as_str)
    else {
        return Err(VErr::new(change_index, "textDocument.uri is required"));
    };
    let version = match identifier.and_then(|identifier| identifier.get("version")) {
        Some(Value::Null) => None,
        Some(Value::Number(number)) if non_negative_integer(number).is_some() => {
            non_negative_integer(number)
        }
        _ => {
            return Err(VErr::new(
                change_index,
                "document version must be null or a non-negative integer",
            ));
        }
    };
    let resolved = match uri_to_canonical_workspace_path(uri, workspace_root) {
        Ok(resolved) => resolved,
        Err(error) => {
            push_failure(target, change_index, error);
            return Ok(());
        }
    };
    let edits = parse_text_edits(change.get("edits").unwrap_or(&Value::Null), change_index)?;
    target.operations.push(ParsedWorkspaceOperation::Text {
        change_index,
        path: resolved.path,
        reported_path: resolved.requested_path,
        edits,
        version,
    });
    Ok(())
}

fn non_negative_integer(number: &serde_json::Number) -> Option<i64> {
    if let Some(value) = number.as_i64() {
        return (value >= 0).then_some(value);
    }
    let value = number.as_f64()?;
    (value.fract() == 0.0 && value >= 0.0 && value <= i64::MAX as f64).then_some(value as i64)
}

fn parse_position(value: Option<&Value>) -> Option<(f64, f64)> {
    let record = value?.as_object()?;
    Some((
        record.get("line")?.as_f64()?,
        record.get("character")?.as_f64()?,
    ))
}

/// TS `parseTextEdits`.
pub fn parse_text_edits(value: &Value, change_index: usize) -> Result<Vec<ParsedTextEdit>, VErr> {
    let Some(candidates) = value.as_array() else {
        return Err(VErr::new(change_index, "text edits must be an array"));
    };
    let mut edits = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let record = candidate.as_object();
        let Some(new_text) = record
            .and_then(|record| record.get("newText"))
            .and_then(Value::as_str)
        else {
            return Err(VErr::new(
                change_index,
                "text edit requires range and newText",
            ));
        };
        let record = record.expect("checked above");
        if record.contains_key("annotationId") {
            return Err(VErr::new(
                change_index,
                "annotated text edits are unsupported",
            ));
        }
        let range = record.get("range").and_then(Value::as_object);
        let start = parse_position(range.and_then(|range| range.get("start")));
        let end = parse_position(range.and_then(|range| range.get("end")));
        let (Some((start_line, start_character)), Some((end_line, end_character))) = (start, end)
        else {
            return Err(VErr::new(change_index, "text edit range is malformed"));
        };
        edits.push(ParsedTextEdit {
            start_line,
            start_character,
            end_line,
            end_character,
            new_text: new_text.to_string(),
        });
    }
    Ok(edits)
}

/// TS `parseOptions`: every allowed key defaults to false.
pub fn parse_options(
    value: Option<&Value>,
    allowed: &[&str],
    change_index: usize,
) -> Result<Vec<bool>, VErr> {
    let Some(value) = value else {
        return Ok(vec![false; allowed.len()]);
    };
    let Some(record) = value.as_object() else {
        return Err(VErr::new(
            change_index,
            "resource options must be an object",
        ));
    };
    for key in record.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(VErr::new(
                change_index,
                format!("unsupported resource option {key}"),
            ));
        }
    }
    allowed
        .iter()
        .map(|key| match record.get(*key) {
            None => Ok(false),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(_) => Err(VErr::new(change_index, format!("{key} must be boolean"))),
        })
        .collect()
}

fn parse_resource_change(
    change: &Record,
    kind: &str,
    change_index: usize,
    workspace_root: &str,
    target: &mut ParsedWorkspaceEdit,
) -> Result<(), VErr> {
    match kind {
        "create" | "delete" => {
            let Some(uri) = change.get("uri").and_then(Value::as_str) else {
                return Err(VErr::new(change_index, format!("{kind}.uri is required")));
            };
            let resolved = match uri_to_canonical_workspace_path(uri, workspace_root) {
                Ok(resolved) => resolved,
                Err(error) => {
                    push_failure(target, change_index, error);
                    return Ok(());
                }
            };
            if kind == "create" {
                let options = parse_options(
                    change.get("options"),
                    &["overwrite", "ignoreIfExists"],
                    change_index,
                )?;
                target.operations.push(ParsedWorkspaceOperation::Create {
                    change_index,
                    path: resolved.path,
                    reported_path: resolved.requested_path,
                    overwrite: options[0],
                    ignore_if_exists: options[1],
                    followed_symbolic_link: resolved.followed_symbolic_link,
                });
            } else {
                let options = parse_options(
                    change.get("options"),
                    &["recursive", "ignoreIfNotExists"],
                    change_index,
                )?;
                target.operations.push(ParsedWorkspaceOperation::Delete {
                    change_index,
                    path: resolved.path,
                    reported_path: resolved.requested_path,
                    recursive: options[0],
                    ignore_if_not_exists: options[1],
                    followed_symbolic_link: resolved.followed_symbolic_link,
                });
            }
            Ok(())
        }
        "rename" => {
            let old_uri = change.get("oldUri").and_then(Value::as_str);
            let new_uri = change.get("newUri").and_then(Value::as_str);
            let (Some(old_uri), Some(new_uri)) = (old_uri, new_uri) else {
                return Err(VErr::new(change_index, "rename requires oldUri and newUri"));
            };
            let old_path = uri_to_canonical_workspace_path(old_uri, workspace_root);
            let new_path = uri_to_canonical_workspace_path(new_uri, workspace_root);
            let (old_path, new_path) = match (old_path, new_path) {
                (Ok(old_path), Ok(new_path)) => (old_path, new_path),
                (Err(error), _) | (_, Err(error)) => {
                    push_failure(target, change_index, error);
                    return Ok(());
                }
            };
            let options = parse_options(
                change.get("options"),
                &["overwrite", "ignoreIfExists"],
                change_index,
            )?;
            target.operations.push(ParsedWorkspaceOperation::Rename {
                change_index,
                followed_symbolic_link: old_path.followed_symbolic_link
                    || new_path.followed_symbolic_link,
                old_path: old_path.path,
                new_path: new_path.path,
                reported_old_path: old_path.requested_path,
                reported_new_path: new_path.requested_path,
                overwrite: options[0],
                ignore_if_exists: options[1],
            });
            Ok(())
        }
        other => Err(VErr::new(
            change_index,
            format!("unsupported resource operation {other}"),
        )),
    }
}
