use comment_checker_core::ApplyPatchFileMetadata;
use comment_checker_core::ApplyPatchOperation;
use comment_checker_core::CheckerEdit;
use comment_checker_core::extract_apply_patch_edits;
use comment_checker_core::get_apply_patch_metadata_files;
use comment_checker_core::get_string;
use comment_checker_core::is_record;
use comment_checker_core::join_patch_lines;
use comment_checker_core::make_accumulator;
use comment_checker_core::parse_apply_patch_requests;
use comment_checker_core::read_apply_patch_metadata_files;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

fn edit(file_path: &str, before: &str, after: &str) -> CheckerEdit {
    CheckerEdit {
        file_path: file_path.to_string(),
        before: before.to_string(),
        after: after.to_string(),
    }
}

fn meta(file_path: &str, before: &str, after: &str) -> ApplyPatchFileMetadata {
    ApplyPatchFileMetadata {
        file_path: file_path.to_string(),
        move_path: None,
        before: before.to_string(),
        after: after.to_string(),
        r#type: None,
    }
}

// TS: "#given array metadata details #when reading apply-patch metadata files #then legacy record semantics still accept the array wrapper"
// A JS array carrying an extra `files` property has no JSON form; the closest JSON value
// is an array, which the legacy guard accepts as a record but which has no `files` key.
#[test]
fn array_metadata_details_are_accepted_by_legacy_record_guard() {
    let details = json!([{ "filePath": "src/example.ts", "before": "before", "after": "after" }]);

    let files = get_apply_patch_metadata_files(&details);

    assert!(is_record(&details));
    assert_eq!(files, Vec::<ApplyPatchFileMetadata>::new());
}

#[test]
fn metadata_files_read_from_direct_files_key() {
    let details = json!({ "files": [{ "filePath": "src/example.ts", "before": "before", "after": "after" }] });

    assert_eq!(
        get_apply_patch_metadata_files(&details),
        vec![meta("src/example.ts", "before", "after")]
    );
}

#[test]
fn metadata_files_fall_back_to_result_then_metadata() {
    let from_result = json!({
        "files": [],
        "result": { "files": [{ "path": "a.ts", "old": "1", "new": "2" }] },
        "metadata": { "files": [{ "path": "b.ts", "old": "x", "new": "y" }] },
    });
    let from_metadata = json!({
        "result": "not a record",
        "metadata": { "files": [{ "file_path": "b.ts", "old_string": "x", "new_string": "y" }] },
    });

    assert_eq!(
        get_apply_patch_metadata_files(&from_result),
        vec![meta("a.ts", "1", "2")]
    );
    assert_eq!(
        get_apply_patch_metadata_files(&from_metadata),
        vec![meta("b.ts", "x", "y")]
    );
}

#[test]
fn metadata_files_empty_for_non_record_details() {
    for details in [Value::Null, json!("text"), json!(3), json!(true)] {
        assert_eq!(get_apply_patch_metadata_files(&details), Vec::new());
    }
}

#[test]
fn read_metadata_files_skips_incomplete_entries_and_keeps_optional_fields() {
    let value = json!([
        "not a record",
        { "filePath": "missing-after.ts", "before": "b" },
        { "before": "b", "after": "a" },
        { "filePath": 7, "path": "fallback.ts", "oldString": "b", "newString": "a", "move_path": "moved.ts", "operation": "update" },
    ]);

    assert_eq!(
        read_apply_patch_metadata_files(Some(&value)),
        vec![ApplyPatchFileMetadata {
            file_path: "fallback.ts".to_string(),
            move_path: Some("moved.ts".to_string()),
            before: "b".to_string(),
            after: "a".to_string(),
            r#type: Some("update".to_string()),
        }]
    );
    assert_eq!(
        read_apply_patch_metadata_files(Some(&json!({ "a": 1 }))),
        Vec::new()
    );
    assert_eq!(read_apply_patch_metadata_files(None), Vec::new());
}

#[test]
fn extract_prefers_metadata_uses_move_path_and_drops_deletes_case_insensitively() {
    let details = json!({ "files": [
        { "filePath": "old.ts", "movePath": "new.ts", "before": "b", "after": "a", "type": "update" },
        { "filePath": "gone.ts", "before": "b", "after": "", "type": "DELETE" },
    ] });
    let args = json!({ "patchText": "*** Add File: ignored.ts\n+x" });

    assert_eq!(
        extract_apply_patch_edits(&details, args.as_object()),
        vec![edit("new.ts", "b", "a")]
    );
}

#[test]
fn extract_falls_back_to_patch_argument_keys_in_order() {
    let details = json!({ "files": [{ "filePath": "gone.ts", "before": "b", "after": "", "type": "delete" }] });
    let args = json!({ "patchText": 1, "input": "*** Add File: a.ts\n+one", "patch": "*** Add File: b.ts\n+two" });
    let command_only = json!({ "command": "*** Add File: c.ts\n+three" });

    assert_eq!(
        extract_apply_patch_edits(&details, args.as_object()),
        vec![edit("a.ts", "", "one\n")]
    );
    assert_eq!(
        extract_apply_patch_edits(&Value::Null, command_only.as_object()),
        vec![edit("c.ts", "", "three\n")]
    );
}

#[test]
fn extract_returns_empty_without_metadata_or_patch() {
    assert_eq!(extract_apply_patch_edits(&Value::Null, None), Vec::new());
    assert_eq!(
        extract_apply_patch_edits(&Value::Null, json!({ "other": "x" }).as_object()),
        Vec::new()
    );
}

#[test]
fn parse_add_update_move_and_delete_sections() {
    let patch = "*** Begin Patch\r\n\
*** Add File:  src/new.ts \r\n\
+// added\r\n\
ignored line\r\n\
*** Update File: src/old.ts\n\
*** Move to: src/moved.ts\n\
@@ fn main\n\
 context\n\
-old line\n\
+new line\n\
*** Delete File: src/gone.ts\n\
-removed\n\
+ignored\n\
*** End Patch";

    assert_eq!(
        parse_apply_patch_requests(patch),
        vec![
            edit("src/new.ts", "", "// added\n"),
            edit("src/moved.ts", "old line\n", "new line\n"),
        ]
    );
}

#[test]
fn parse_drops_sections_without_added_lines_and_ignores_orphan_lines() {
    let patch = "+orphan\n*** Move to: nowhere.ts\n*** Add File: empty.ts\n*** Update File: pure-delete.ts\n-only removed\n*** Add File: moved-add.ts\n*** Move to: ignored.ts\n+kept";

    assert_eq!(
        parse_apply_patch_requests(patch),
        vec![edit("moved-add.ts", "", "kept\n")]
    );
}

#[test]
fn parse_keeps_empty_added_lines_and_hunk_headers_are_skipped() {
    let patch = "*** Update File: a.ts\n@@\n+\n+second\n@@ next\n+third\n";

    assert_eq!(
        parse_apply_patch_requests(patch),
        vec![edit("a.ts", "", "\nsecond\nthird\n")]
    );
}

#[test]
fn make_accumulator_starts_empty() {
    let acc = make_accumulator(ApplyPatchOperation::Update, "a.ts");

    assert_eq!(
        serde_json::to_value(&acc).ok(),
        Some(json!({ "operation": "update", "filePath": "a.ts", "oldLines": [], "newLines": [] }))
    );
}

#[test]
fn get_string_returns_first_string_valued_key() {
    let input = json!({ "a": 1, "b": "bee", "c": "see" });
    let map = input.as_object().cloned().unwrap_or_default();

    assert_eq!(
        get_string(&map, &["missing", "a", "b", "c"]),
        Some("bee".to_string())
    );
    assert_eq!(get_string(&map, &["a"]), None);
}

#[test]
fn join_patch_lines_appends_trailing_newline_unless_empty() {
    assert_eq!(join_patch_lines(&[]), "");
    assert_eq!(
        join_patch_lines(&["a".to_string(), "b".to_string()]),
        "a\nb\n"
    );
}
