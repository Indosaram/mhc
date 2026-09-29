//! Port of TS `workspace-edit-fingerprint.ts`: sha256 of `JSON.stringify` of the ops.

use super::types::ParsedTextEdit;
use super::types::ParsedWorkspaceOperation as Parsed;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;

/// JS number semantics: integral values serialize without a fraction.
pub(crate) fn js_number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        json!(value as i64)
    } else {
        json!(value)
    }
}

fn edit_json(edit: &ParsedTextEdit) -> Value {
    json!({
        "range": {
            "start": {"line": js_number(edit.start_line), "character": js_number(edit.start_character)},
            "end": {"line": js_number(edit.end_line), "character": js_number(edit.end_character)},
        },
        "newText": edit.new_text,
    })
}

/// TS `canonicalFingerprint`.
pub fn canonical_fingerprint(operations: &[Parsed]) -> String {
    let canonical: Vec<Value> = operations
        .iter()
        .map(|operation| match operation {
            Parsed::Text {
                change_index,
                path,
                edits,
                version,
                ..
            } => json!({
                "kind": "text",
                "changeIndex": change_index,
                "path": path,
                "edits": edits.iter().map(edit_json).collect::<Vec<_>>(),
                "version": version,
            }),
            Parsed::Rename {
                change_index,
                old_path,
                new_path,
                overwrite,
                ignore_if_exists,
                ..
            } => json!({
                "kind": "rename",
                "changeIndex": change_index,
                "oldPath": old_path,
                "newPath": new_path,
                "overwrite": overwrite,
                "ignoreIfExists": ignore_if_exists,
            }),
            Parsed::Create {
                change_index,
                path,
                overwrite,
                ignore_if_exists,
                ..
            } => json!({
                "kind": "create",
                "changeIndex": change_index,
                "path": path,
                "overwrite": overwrite,
                "ignoreIfExists": ignore_if_exists,
            }),
            Parsed::Delete {
                change_index,
                path,
                recursive,
                ignore_if_not_exists,
                ..
            } => json!({
                "kind": "delete",
                "changeIndex": change_index,
                "path": path,
                "recursive": recursive,
                "ignoreIfNotExists": ignore_if_not_exists,
            }),
        })
        .collect();
    let serialized = serde_json::to_string(&canonical).expect("JSON values always serialize");
    hex::encode(Sha256::digest(serialized.as_bytes()))
}
