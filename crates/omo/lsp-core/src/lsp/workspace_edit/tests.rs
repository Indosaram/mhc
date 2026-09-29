//! Translations of workspace-edit-{commit,adversarial,contract-evidence,options,
//! prevalidation}.test.ts and workspace-edit.characterization.test.ts.

use super::*;
use crate::abort::AbortController;
use crate::lsp::workspace_apply_edit_failure::CANONICAL_CONCURRENT_WORKSPACE_APPLY_EDIT_FAILURE_REASON;
use crate::lsp::workspace_apply_edit_failure::WorkspaceApplyEditConcurrentPhase;
use crate::lsp::workspace_apply_edit_failure::workspace_apply_edit_concurrent_failure_reason;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

fn uri(path: &str) -> String {
    url::Url::from_file_path(path)
        .expect("absolute path")
        .to_string()
}

fn tmp() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_string_lossy().into_owned();
    (dir, path)
}

fn join(dir: &str, name: &str) -> String {
    format!("{dir}/{name}")
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).expect("read")
}

fn write(path: &str, content: &str) {
    std::fs::write(path, content).expect("write");
}

fn exists(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

fn edit(line: u32, characters: (u32, u32), new_text: &str) -> Value {
    json!({
        "range": {
            "start": {"line": line, "character": characters.0},
            "end": {"line": line, "character": characters.1},
        },
        "newText": new_text,
    })
}

fn replacement(before: &str, after: &str) -> Value {
    edit(0, (6, 6 + before.len() as u32), after)
}

fn text_edit(path: &str, before: &str, after: &str) -> Value {
    json!({"changes": {uri(path): [replacement(before, after)]}})
}

fn opts(workspace: &str) -> ApplyWorkspaceEditOptions {
    ApplyWorkspaceEditOptions {
        workspace_root: Some(workspace.to_string()),
        ..ApplyWorkspaceEditOptions::default()
    }
}

fn apply(value: &Value, workspace: &str) -> ApplyResult {
    apply_workspace_edit(Some(value), opts(workspace))
}

fn commit_fixture() -> (tempfile::TempDir, String, String) {
    let (dir, workspace) = tmp();
    let source = join(&workspace, "source.ts");
    write(&source, "const before = 1;\n");
    (dir, workspace, source)
}

// ---- workspace-edit-commit.test.ts ----

#[test]
fn commit_cancellation_before_commit_mutates_nothing() {
    let (_dir, workspace, source) = commit_fixture();
    let controller = AbortController::new();
    controller.abort();
    let commit = apply_workspace_edit_detailed(
        Some(&text_edit(&source, "before", "after")),
        ApplyWorkspaceEditOptions {
            signal: Some(controller.signal()),
            ..opts(&workspace)
        },
    );
    assert!(!commit.result.success);
    assert!(
        commit
            .result
            .errors
            .join("\n")
            .contains("cancelled before commit")
    );
    assert!(commit.delta.operations.is_empty());
    assert_eq!(read(&source), "const before = 1;\n");
}

#[test]
fn commit_repeated_pre_gate_interruptions_leave_snapshot_untouched() {
    let (_dir, workspace, source) = commit_fixture();
    for _ in 0..3 {
        let controller = AbortController::new();
        controller.abort();
        let commit = apply_workspace_edit_detailed(
            Some(&text_edit(&source, "before", "after")),
            ApplyWorkspaceEditOptions {
                signal: Some(controller.signal()),
                ..opts(&workspace)
            },
        );
        assert!(!commit.result.success);
        assert!(commit.delta.operations.is_empty());
    }
    assert_eq!(read(&source), "const before = 1;\n");
}

#[test]
fn commit_cancellation_after_first_write_finishes_all_operations_once() {
    let (_dir, workspace, source) = commit_fixture();
    let second = join(&workspace, "second.ts");
    write(&second, "const second = 2;\n");
    let controller = AbortController::new();
    let writes = Arc::new(AtomicUsize::new(0));
    let (hook_writes, hook_controller) = (writes.clone(), controller.clone());
    let io = WorkspaceEditCommitIo {
        write_file: Some(Box::new(move |path, content| {
            let count = hook_writes.fetch_add(1, Ordering::SeqCst) + 1;
            std::fs::write(path, content).map_err(|error| error.to_string())?;
            if count == 1 {
                hook_controller.abort();
            }
            Ok(())
        })),
        ..WorkspaceEditCommitIo::default()
    };
    let value = json!({"changes": {
        uri(&source): [replacement("before", "after")],
        uri(&second): [replacement("second", "later_")],
    }});
    let commit = apply_workspace_edit_detailed(
        Some(&value),
        ApplyWorkspaceEditOptions {
            signal: Some(controller.signal()),
            io,
            ..opts(&workspace)
        },
    );
    assert!(commit.result.success);
    assert_eq!(commit.result.late_abort, Some(true));
    assert_eq!(commit.result.total_edits, 2);
    assert_eq!(writes.load(Ordering::SeqCst), 2);
    assert_eq!(read(&source), "const after = 1;\n");
    assert_eq!(read(&second), "const later_ = 2;\n");
}

#[test]
fn commit_injected_write_failure_reports_real_io_failure() {
    let (_dir, workspace, source) = commit_fixture();
    let io = WorkspaceEditCommitIo {
        write_file: Some(Box::new(|_, _| Err("injected write failure".to_string()))),
        ..WorkspaceEditCommitIo::default()
    };
    let commit = apply_workspace_edit_detailed(
        Some(&text_edit(&source, "before", "after")),
        ApplyWorkspaceEditOptions {
            io,
            ..opts(&workspace)
        },
    );
    assert!(!commit.result.success);
    assert_eq!(commit.result.failed_change, Some(0));
    assert!(
        commit
            .result
            .errors
            .join("\n")
            .contains("I/O failure during text: injected write failure")
    );
    assert!(commit.delta.operations.is_empty());
    assert_eq!(read(&source), "const before = 1;\n");
}

#[test]
fn commit_stale_file_snapshot_preserves_newer_bytes() {
    let (_dir, workspace, source) = commit_fixture();
    let WorkspaceEditPlanResult::Success(plan) =
        plan_workspace_edit(&text_edit(&source, "before", "after"), &workspace)
    else {
        panic!("plan should succeed");
    };
    write(&source, "const external = 2;\n");
    let commit = commit_workspace_edit_plan(&plan, None, &mut WorkspaceEditCommitIo::default());
    assert!(!commit.result.success);
    assert!(
        commit
            .result
            .errors
            .join("\n")
            .contains("workspace state changed before commit")
    );
    assert!(commit.delta.operations.is_empty());
    assert_eq!(read(&source), "const external = 2;\n");
}

// ---- workspace-edit-adversarial.test.ts ----

fn insertion() -> Value {
    edit(0, (0, 0), "x")
}

#[test]
fn adversarial_malformed_and_decorated_uris_are_rejected() {
    let (_dir, workspace) = tmp();
    let source = join(&workspace, "source.ts");
    write(&source, "const before = 1;\n");
    let decorated = format!("{}?query=forbidden", uri(&source));
    for candidate in ["untitled:source.ts", decorated.as_str(), "file:///%ZZ"] {
        let result =
            plan_workspace_edit(&json!({"changes": {candidate: [insertion()]}}), &workspace);
        assert!(!result.is_success(), "{candidate} must be rejected");
    }
    assert_eq!(read(&source), "const before = 1;\n");
}

#[test]
fn adversarial_invalid_ranges_and_options_retain_operation_index() {
    let (_dir, workspace) = tmp();
    let source = join(&workspace, "source.ts");
    let created = join(&workspace, "created.ts");
    write(&source, "const before = 1;\n");
    let invalid_range = plan_workspace_edit(
        &json!({"documentChanges": [{
            "textDocument": {"uri": uri(&source), "version": null},
            "edits": [{"range": {"start": {"line": 0, "character": -1}, "end": {"line": 0, "character": 2}}, "newText": "bad"}],
        }]}),
        &workspace,
    );
    let invalid_option = plan_workspace_edit(
        &json!({"documentChanges": [
            {"textDocument": {"uri": uri(&source), "version": null}, "edits": []},
            {"kind": "create", "uri": uri(&created), "options": {"overwrite": "yes"}},
        ]}),
        &workspace,
    );
    let WorkspaceEditPlanResult::Failure(range_result) = invalid_range else {
        panic!("invalid range must fail");
    };
    assert!(range_result.errors[0].contains("change 0"));
    let WorkspaceEditPlanResult::Failure(option_result) = invalid_option else {
        panic!("invalid option must fail");
    };
    assert!(option_result.errors[0].contains("change 1"));
    assert_eq!(read(&source), "const before = 1;\n");
}

#[test]
fn adversarial_outside_target_touches_nothing() {
    let (_dir, workspace) = tmp();
    let (_outside_dir, outside) = tmp();
    let dirty = join(&workspace, "dirty.ts");
    let outside_file = join(&outside, "outside.ts");
    write(&dirty, "user-owned dirty bytes\n");
    write(&outside_file, "outside bytes\n");
    let result = apply(
        &json!({"changes": {uri(&outside_file): [insertion()]}}),
        &workspace,
    );
    assert!(!result.success);
    assert!(result.errors.join("\n").contains("outside workspace"));
    assert_eq!(read(&dirty), "user-owned dirty bytes\n");
    assert_eq!(read(&outside_file), "outside bytes\n");
}

#[test]
fn adversarial_symlink_to_outside_file_is_rejected() {
    let (_dir, workspace) = tmp();
    let (_outside_dir, outside) = tmp();
    let outside_file = join(&outside, "outside.ts");
    let canonical = std::fs::canonicalize(&workspace)
        .expect("canonical")
        .to_string_lossy()
        .into_owned();
    let linked = join(&canonical, "linked");
    write(&outside_file, "outside bytes\n");
    std::os::unix::fs::symlink(&outside, &linked).expect("symlink");
    let result = apply(
        &json!({"changes": {uri(&join(&linked, "outside.ts")): [insertion()]}}),
        &canonical,
    );
    assert!(!result.success);
    assert!(result.errors.join("\n").contains("outside workspace"));
    assert_eq!(read(&outside_file), "outside bytes\n");
}

#[test]
fn adversarial_annotations_and_mixed_representations_are_rejected() {
    let (_dir, workspace) = tmp();
    let unsupported = [
        json!({"changeAnnotations": {"change": {"label": "unsupported"}}}),
        json!({"changes": {}, "documentChanges": []}),
        json!({"documentChanges": [{"kind": "create", "uri": "file:///missing", "annotationId": "change"}]}),
    ];
    for value in unsupported {
        assert!(
            !plan_workspace_edit(&value, &workspace).is_success(),
            "{value}"
        );
    }
}

// ---- workspace-edit-contract-evidence.test.ts ----

#[test]
fn contract_evidence_concurrent_outcomes_share_canonical_hash() {
    let workspace = "/tmp/workspace-edit-contract";
    let applying = workspace_edit_contract_failure_evidence(
        &json!({"applied": false, "failureReason": workspace_apply_edit_concurrent_failure_reason(WorkspaceApplyEditConcurrentPhase::Applying)}),
        workspace,
    );
    let settled = workspace_edit_contract_failure_evidence(
        &json!({"applied": false, "failureReason": workspace_apply_edit_concurrent_failure_reason(WorkspaceApplyEditConcurrentPhase::Settled)}),
        workspace,
    );
    assert_eq!(
        applying.normalized,
        json!({"applied": false, "failureReason": CANONICAL_CONCURRENT_WORKSPACE_APPLY_EDIT_FAILURE_REASON})
    );
    assert_eq!(settled.normalized, applying.normalized);
    assert_eq!(settled.sha256, applying.sha256);
}

#[test]
fn contract_evidence_sanitizes_paths_and_preserves_other_reasons() {
    let workspace = "/tmp/workspace-edit-contract";
    let normalized = normalize_workspace_edit_contract_evidence(
        &json!({
            "applied": false,
            "failureReason": "document version 9 does not match open document version 1 for /tmp/workspace-edit-contract/source.ts",
            "nested": ["/tmp/workspace-edit-contract/source.ts"],
        }),
        workspace,
    );
    assert_eq!(
        normalized,
        json!({
            "applied": false,
            "failureReason": "document version 9 does not match open document version 1 for <workspace>/source.ts",
            "nested": ["<workspace>/source.ts"],
        })
    );
}

// ---- workspace-edit-options.test.ts ----

#[test]
fn options_create_ignore_if_exists_preserves_bytes() {
    let (_dir, workspace) = tmp();
    let target = join(&workspace, "target.ts");
    write(&target, "preserve\n");
    let result = apply(
        &json!({"documentChanges": [{"kind": "create", "uri": uri(&target), "options": {"ignoreIfExists": true}}]}),
        &workspace,
    );
    assert!(result.success);
    assert!(result.files_modified.is_empty());
    assert_eq!(read(&target), "preserve\n");
}

#[test]
fn options_rename_ignore_if_exists_preserves_both() {
    let (_dir, workspace) = tmp();
    let source = join(&workspace, "source.ts");
    let destination = join(&workspace, "destination.ts");
    write(&source, "source\n");
    write(&destination, "destination\n");
    let result = apply(
        &json!({"documentChanges": [{"kind": "rename", "oldUri": uri(&source), "newUri": uri(&destination), "options": {"ignoreIfExists": true}}]}),
        &workspace,
    );
    assert!(result.success);
    assert!(result.files_modified.is_empty());
    assert_eq!(read(&source), "source\n");
    assert_eq!(read(&destination), "destination\n");
}

#[test]
fn options_rename_overwrite_replaces_destination() {
    let (_dir, workspace) = tmp();
    let source = join(&workspace, "source.ts");
    let destination = join(&workspace, "destination.ts");
    write(&source, "source\n");
    write(&destination, "destination\n");
    let result = apply(
        &json!({"documentChanges": [{"kind": "rename", "oldUri": uri(&source), "newUri": uri(&destination), "options": {"overwrite": true}}]}),
        &workspace,
    );
    assert!(result.success);
    assert!(!exists(&source));
    assert_eq!(read(&destination), "source\n");
}

#[test]
fn options_delete_ignore_if_not_exists_is_noop() {
    let (_dir, workspace) = tmp();
    let target = join(&workspace, "missing.ts");
    let result = apply(
        &json!({"documentChanges": [{"kind": "delete", "uri": uri(&target), "options": {"ignoreIfNotExists": true}}]}),
        &workspace,
    );
    assert!(result.success);
    assert!(result.files_modified.is_empty());
}

#[test]
fn options_recursive_delete_removes_subtree() {
    let (_dir, workspace) = tmp();
    let target = join(&workspace, "nested");
    std::fs::create_dir(&target).expect("mkdir");
    write(&join(&target, "child.ts"), "child\n");
    let result = apply(
        &json!({"documentChanges": [{"kind": "delete", "uri": uri(&target), "options": {"recursive": true}}]}),
        &workspace,
    );
    assert!(result.success);
    assert!(!exists(&target));
}

// ---- workspace-edit-prevalidation.test.ts ----

#[test]
fn prevalidation_overlap_blocks_all_files() {
    let (_dir, workspace) = tmp();
    let valid = join(&workspace, "valid.ts");
    let invalid = join(&workspace, "invalid.ts");
    write(&valid, "const valid = 1;\n");
    write(&invalid, "const invalid = 2;\n");
    let result = apply(
        &json!({"changes": {
            uri(&valid): [edit(0, (6, 11), "changed")],
            uri(&invalid): [edit(0, (0, 10), "first"), edit(0, (5, 13), "second")],
        }}),
        &workspace,
    );
    assert!(!result.success);
    assert!(result.files_modified.is_empty());
    assert_eq!(read(&valid), "const valid = 1;\n");
    assert_eq!(read(&invalid), "const invalid = 2;\n");
}

#[test]
fn prevalidation_identical_non_empty_edits_commit_once() {
    let (_dir, workspace) = tmp();
    let source = join(&workspace, "source.ts");
    write(&source, "const before = 1;\n");
    let replacement = edit(0, (6, 12), "after");
    let result = apply(
        &json!({"changes": {uri(&source): [replacement.clone(), replacement]}}),
        &workspace,
    );
    assert!(result.success);
    assert_eq!(result.total_edits, 1);
    assert_eq!(read(&source), "const after = 1;\n");
}

#[test]
fn prevalidation_equal_position_insertions_keep_declared_order() {
    let (_dir, workspace) = tmp();
    let source = join(&workspace, "source.ts");
    write(&source, "abc");
    let result = apply(
        &json!({"changes": {uri(&source): [edit(0, (1, 1), "X"), edit(0, (1, 1), "X"), edit(0, (1, 1), "Y")]}}),
        &workspace,
    );
    assert!(result.success);
    assert_eq!(result.total_edits, 3);
    assert_eq!(read(&source), "aXXYbc");
}

#[test]
fn prevalidation_range_beyond_snapshot_fails_without_mutation() {
    let (_dir, workspace) = tmp();
    let source = join(&workspace, "source.ts");
    write(&source, "short\n");
    let result = apply(
        &json!({"changes": {uri(&source): [edit(4, (0, 1), "invalid")]}}),
        &workspace,
    );
    assert!(!result.success);
    assert!(result.files_modified.is_empty());
    assert_eq!(read(&source), "short\n");
}

#[test]
fn prevalidation_create_then_invalid_edit_creates_nothing() {
    let (_dir, workspace) = tmp();
    let source = join(&workspace, "created.ts");
    let target = uri(&source);
    let result = apply(
        &json!({"documentChanges": [
            {"kind": "create", "uri": target},
            {"textDocument": {"uri": target, "version": null}, "edits": [edit(2, (0, 1), "invalid")]},
        ]}),
        &workspace,
    );
    assert!(!result.success);
    assert!(result.files_modified.is_empty());
    assert_eq!(result.failed_change, Some(1));
    assert!(result.errors.join("\n").contains("change 1"));
    assert!(!exists(&source));
}

#[test]
fn prevalidation_rename_then_edit_uses_virtual_state() {
    let (_dir, workspace) = tmp();
    let source = join(&workspace, "before.ts");
    let destination = join(&workspace, "after.ts");
    write(&source, "const before = 1;\n");
    let result = apply(
        &json!({"documentChanges": [
            {"kind": "rename", "oldUri": uri(&source), "newUri": uri(&destination)},
            {"textDocument": {"uri": uri(&destination), "version": null}, "edits": [edit(0, (6, 12), "after")]},
        ]}),
        &workspace,
    );
    assert!(result.success);
    assert!(!exists(&source));
    assert_eq!(read(&destination), "const after = 1;\n");
}

// ---- workspace-edit.characterization.test.ts ----

#[test]
fn characterization_valid_text_edit_result_is_compatible() {
    let (_dir, workspace) = tmp();
    let source = join(&workspace, "source.ts");
    write(&source, "const before = 1;\n");
    let result = apply(
        &json!({"changes": {uri(&source): [edit(0, (6, 12), "after")]}}),
        &workspace,
    );
    assert_eq!(
        serde_json::to_value(&result).expect("json"),
        json!({"success": true, "filesModified": [source], "totalEdits": 1, "errors": []})
    );
    assert_eq!(read(&source), "const after = 1;\n");
}

#[test]
fn characterization_resource_rename_reports_destination() {
    let (_dir, workspace) = tmp();
    let source = join(&workspace, "before.ts");
    let destination = join(&workspace, "after.ts");
    write(&source, "export const value = 1;\n");
    let result = apply(
        &json!({"documentChanges": [{"kind": "rename", "oldUri": uri(&source), "newUri": uri(&destination)}]}),
        &workspace,
    );
    assert_eq!(
        serde_json::to_value(&result).expect("json"),
        json!({"success": true, "filesModified": [destination], "totalEdits": 0, "errors": []})
    );
    assert!(!exists(&source));
    assert_eq!(read(&destination), "export const value = 1;\n");
}

#[test]
fn characterization_empty_edit_is_successful_noop() {
    let (_dir, workspace) = tmp();
    let result = apply(&json!({}), &workspace);
    assert_eq!(
        serde_json::to_value(&result).expect("json"),
        json!({"success": true, "filesModified": [], "totalEdits": 0, "errors": []})
    );
}

#[test]
fn characterization_missing_edit_failure_is_explicit() {
    let (_dir, workspace) = tmp();
    let result = apply_workspace_edit(None, opts(&workspace));
    assert_eq!(
        serde_json::to_value(&result).expect("json"),
        json!({"success": false, "filesModified": [], "totalEdits": 0, "errors": ["No edit provided"]})
    );
}
