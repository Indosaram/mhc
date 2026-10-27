use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Checker exit code for "nothing to report".
pub const EXIT_PASS: i32 = 0;
/// Checker exit code for "comments found"; the message is on stderr.
pub const EXIT_WARNING: i32 = 2;

/// One old/new replacement of a multi-edit.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditPair {
    pub old_string: String,
    pub new_string: String,
}

/// The `tool_input` object of a PostToolUse hook payload. Absent fields are omitted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookToolInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_string: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_string: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<Vec<EditPair>>,
}

/// PostToolUse hook payload written to the checker's stdin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookInput {
    pub session_id: String,
    /// `Write`, `Edit` or `MultiEdit`.
    pub tool_name: String,
    pub transcript_path: String,
    pub cwd: String,
    pub hook_event_name: String,
    pub tool_input: HookToolInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<Value>,
}

/// First string value among `keys`.
pub fn get_string(input: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str).map(str::to_owned))
}

/// One file entry of apply_patch result metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyPatchFile {
    pub file_path: String,
    pub move_path: Option<String>,
    pub before: String,
    pub after: String,
    /// `add`, `update`, `delete`, ... when the tool reports it.
    pub r#type: Option<String>,
}

/// Files listed in apply_patch details: `files`, then `result.files`, then `metadata.files`.
pub fn get_apply_patch_metadata_files(details: &Value) -> Vec<ApplyPatchFile> {
    let Some(details) = details.as_object() else {
        return Vec::new();
    };
    let direct = read_files(details.get("files"));
    if !direct.is_empty() {
        return direct;
    }
    for parent in ["result", "metadata"] {
        let nested = read_files(
            details
                .get(parent)
                .and_then(Value::as_object)
                .and_then(|object| object.get("files")),
        );
        if !nested.is_empty() {
            return nested;
        }
    }
    Vec::new()
}

fn read_files(value: Option<&Value>) -> Vec<ApplyPatchFile> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|item| {
            let file_path = get_string(item, &["filePath", "file_path", "path"])
                .filter(|path| !path.is_empty())?;
            let before = get_string(item, &["before", "old", "oldString", "old_string"])?;
            let after = get_string(item, &["after", "new", "newString", "new_string"])?;
            Some(ApplyPatchFile {
                file_path,
                move_path: get_string(item, &["movePath", "move_path"]),
                before,
                after,
                r#type: get_string(item, &["type", "operation"]),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absent_tool_input_fields_are_omitted() {
        let input = HookInput {
            session_id: "s".into(),
            tool_name: "Write".into(),
            transcript_path: String::new(),
            cwd: "/w".into(),
            hook_event_name: "PostToolUse".into(),
            tool_input: HookToolInput {
                file_path: Some("a.py".into()),
                content: Some("x".into()),
                ..Default::default()
            },
            tool_response: None,
        };
        assert_eq!(
            serde_json::to_value(&input).expect("serialize"),
            json!({"session_id":"s","tool_name":"Write","transcript_path":"","cwd":"/w","hook_event_name":"PostToolUse","tool_input":{"file_path":"a.py","content":"x"}})
        );
    }

    #[test]
    fn get_string_takes_first_string_alias() {
        let map = json!({"file_path": 3, "path": "p", "filePath": "f"});
        let map = map.as_object().expect("object");
        assert_eq!(
            get_string(map, &["file_path", "path", "filePath"]).as_deref(),
            Some("p")
        );
        assert_eq!(get_string(map, &["missing"]), None);
    }

    #[test]
    fn metadata_falls_back_to_result_then_metadata() {
        let nested = json!({"result": {"files": [{"path": "a", "old": "", "new": "x"}]}});
        let files = get_apply_patch_metadata_files(&nested);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_path, "a");
        assert_eq!(files[0].after, "x");
        let meta = json!({"files": [], "metadata": {"files": [{"file_path": "b", "before": "1", "after": "2", "operation": "update", "move_path": "c"}]}});
        let files = get_apply_patch_metadata_files(&meta);
        assert_eq!(files[0].move_path.as_deref(), Some("c"));
        assert_eq!(files[0].r#type.as_deref(), Some("update"));
    }

    #[test]
    fn metadata_skips_incomplete_entries() {
        let value =
            json!({"files": [{"filePath": "a", "before": "x"}, {"before": "", "after": ""}, 7]});
        assert!(get_apply_patch_metadata_files(&value).is_empty());
        assert!(get_apply_patch_metadata_files(&json!("nope")).is_empty());
    }
}
