//! Apply-patch edit extraction (port of `src/apply-patch-edits.ts`).

use serde_json::Map;
use serde_json::Value;
pub use utils::record_type_guard::is_record;

use crate::types::ApplyPatchAccumulator;
use crate::types::ApplyPatchFileMetadata;
use crate::types::ApplyPatchOperation;
use crate::types::CheckerEdit;

const ADD_FILE: &str = "*** Add File: ";
const UPDATE_FILE: &str = "*** Update File: ";
const DELETE_FILE: &str = "*** Delete File: ";
const MOVE_TO: &str = "*** Move to: ";

/// Edits from tool metadata when present, else parsed from the patch text in `args`.
pub fn extract_apply_patch_edits(
    details: &Value,
    args: Option<&Map<String, Value>>,
) -> Vec<CheckerEdit> {
    let metadata_edits: Vec<CheckerEdit> = get_apply_patch_metadata_files(details)
        .into_iter()
        .filter(|file| {
            file.r#type
                .as_deref()
                .is_none_or(|kind| kind.to_lowercase() != "delete")
        })
        .map(|file| CheckerEdit {
            file_path: file.move_path.unwrap_or(file.file_path),
            before: file.before,
            after: file.after,
        })
        .collect();

    if !metadata_edits.is_empty() {
        return metadata_edits;
    }

    args.and_then(|args| get_string(args, &["patchText", "input", "patch", "command"]))
        .map(|patch| parse_apply_patch_requests(&patch))
        .unwrap_or_default()
}

/// Reads `files` from `details`, then `details.result`, then `details.metadata`.
pub fn get_apply_patch_metadata_files(details: &Value) -> Vec<ApplyPatchFileMetadata> {
    if !is_record(details) {
        return Vec::new();
    }

    let direct = read_apply_patch_metadata_files(details.get("files"));
    if !direct.is_empty() {
        return direct;
    }

    let result = details
        .get("result")
        .filter(|value| is_record(value))
        .map(|value| read_apply_patch_metadata_files(value.get("files")))
        .unwrap_or_default();
    if !result.is_empty() {
        return result;
    }

    details
        .get("metadata")
        .filter(|value| is_record(value))
        .map(|value| read_apply_patch_metadata_files(value.get("files")))
        .unwrap_or_default()
}

/// Parses an array of file entries, skipping entries without a path, before, or after.
pub fn read_apply_patch_metadata_files(value: Option<&Value>) -> Vec<ApplyPatchFileMetadata> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|item| {
            Some(ApplyPatchFileMetadata {
                file_path: get_string(item, &["filePath", "file_path", "path"])?,
                move_path: get_string(item, &["movePath", "move_path"]),
                before: get_string(item, &["before", "old", "oldString", "old_string"])?,
                after: get_string(item, &["after", "new", "newString", "new_string"])?,
                r#type: get_string(item, &["type", "operation"]),
            })
        })
        .collect()
}

/// Parses the `*** Begin Patch` protocol into edits; deletes and empty results are dropped.
pub fn parse_apply_patch_requests(patch: &str) -> Vec<CheckerEdit> {
    let mut edits = Vec::new();
    let mut current: Option<ApplyPatchAccumulator> = None;

    let pieces: Vec<&str> = patch.split('\n').collect();
    let last = pieces.len() - 1;
    for (index, piece) in pieces.into_iter().enumerate() {
        let line = if index < last {
            piece.strip_suffix('\r').unwrap_or(piece)
        } else {
            piece
        };

        if line == "*** Begin Patch" || line == "*** End Patch" {
            continue;
        }

        let header = [
            (ADD_FILE, ApplyPatchOperation::Add),
            (UPDATE_FILE, ApplyPatchOperation::Update),
            (DELETE_FILE, ApplyPatchOperation::Delete),
        ]
        .into_iter()
        .find_map(|(prefix, operation)| line.strip_prefix(prefix).map(|path| (operation, path)));
        if let Some((operation, path)) = header {
            flush(current.take(), &mut edits);
            current = Some(make_accumulator(operation, path.trim()));
            continue;
        }

        if let Some(path) = line.strip_prefix(MOVE_TO) {
            if let Some(acc) = current
                .as_mut()
                .filter(|acc| acc.operation == ApplyPatchOperation::Update)
            {
                acc.move_path = Some(path.trim().to_string());
            }
            continue;
        }

        let Some(acc) = current.as_mut() else {
            continue;
        };
        if line.starts_with("@@") {
            continue;
        }

        match acc.operation {
            ApplyPatchOperation::Add => {
                if let Some(added) = line.strip_prefix('+') {
                    acc.new_lines.push(added.to_string());
                }
            }
            ApplyPatchOperation::Update => {
                if let Some(removed) = line.strip_prefix('-') {
                    acc.old_lines.push(removed.to_string());
                }
                if let Some(added) = line.strip_prefix('+') {
                    acc.new_lines.push(added.to_string());
                }
            }
            ApplyPatchOperation::Delete => {}
        }
    }

    flush(current, &mut edits);
    edits
}

fn flush(current: Option<ApplyPatchAccumulator>, edits: &mut Vec<CheckerEdit>) {
    let Some(acc) = current else {
        return;
    };
    match acc.operation {
        ApplyPatchOperation::Add => {
            let after = join_patch_lines(&acc.new_lines);
            if !after.is_empty() {
                edits.push(CheckerEdit {
                    file_path: acc.file_path,
                    before: String::new(),
                    after,
                });
            }
        }
        ApplyPatchOperation::Update => {
            let after = join_patch_lines(&acc.new_lines);
            if !after.is_empty() {
                edits.push(CheckerEdit {
                    file_path: acc.move_path.unwrap_or(acc.file_path),
                    before: join_patch_lines(&acc.old_lines),
                    after,
                });
            }
        }
        ApplyPatchOperation::Delete => {}
    }
}

pub fn make_accumulator(operation: ApplyPatchOperation, file_path: &str) -> ApplyPatchAccumulator {
    ApplyPatchAccumulator {
        operation,
        file_path: file_path.to_string(),
        move_path: None,
        old_lines: Vec::new(),
        new_lines: Vec::new(),
    }
}

/// First key whose value is a JSON string.
pub fn get_string(input: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str))
        .map(str::to_string)
}

/// Joins lines with `\n` and a trailing newline; empty input yields `""`.
pub fn join_patch_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}
