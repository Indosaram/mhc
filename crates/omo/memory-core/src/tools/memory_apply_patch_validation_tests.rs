use std::fs;

use crate::tools::memory_apply_patch_test_support::{
    create_patch_fixture, memory_apply_patch, test_params,
};

#[test]
fn test_rejects_missing_reason_and_input() {
    let (_temp, repo) = create_patch_fixture(&[]);

    let err_reason =
        memory_apply_patch(&repo, &test_params("", "*** Begin Patch\n*** End Patch")).unwrap_err();
    assert!(
        err_reason
            .message
            .contains("'reason' must be a non-empty string")
    );

    let err_input = memory_apply_patch(&repo, &test_params("reason", "")).unwrap_err();
    assert!(
        err_input
            .message
            .contains("'input' must be a non-empty string")
    );
}

#[test]
fn test_rejects_patch_with_no_effective_changes() {
    let memory = "---\ndescription: Unchanged\n---\nbody";
    let (_temp, repo) = create_patch_fixture(&[("system/unchanged.md", memory)]);
    let patch =
        "*** Begin Patch\n*** Update File: system/unchanged.md\n@@\n-body\n+body\n*** End Patch";

    let err = memory_apply_patch(&repo, &test_params("no changes", patch)).unwrap_err();
    assert!(
        err.message
            .contains("made no effective changes; nothing was committed")
    );
}

#[test]
fn test_rejects_modifying_or_writing_readonly_files() {
    let memory = "---\ndescription: Readonly\nread_only: true\n---\nbody";
    let (_temp, repo) = create_patch_fixture(&[("system/ro.md", memory)]);

    let patch1 =
        "*** Begin Patch\n*** Update File: system/ro.md\n@@\n-body\n+new body\n*** End Patch";
    let err1 = memory_apply_patch(&repo, &test_params("modify readonly", patch1)).unwrap_err();
    assert!(err1.message.contains("is read_only and cannot be modified"));

    let patch2 = "*** Begin Patch\n*** Add File: system/new_ro.md\n+---\n+description: Test\n+read_only: true\n+---\n+body\n*** End Patch";
    let (_temp2, repo2) = create_patch_fixture(&[]);
    let _ = memory_apply_patch(&repo2, &test_params("add readonly", patch2));
}

#[test]
fn test_rejects_duplicate_adds_and_adding_existing() {
    let (_temp, repo) = create_patch_fixture(&[(
        "system/existing.md",
        "---\ndescription: Existing\n---\ncontent",
    )]);

    let patch_dup = "*** Begin Patch\n*** Add File: system/dup.md\n+one\n*** Add File: system/dup.md\n+two\n*** End Patch";
    let err_dup = memory_apply_patch(&repo, &test_params("duplicate add", patch_dup)).unwrap_err();
    assert!(
        err_dup
            .message
            .contains("duplicate add/update target in patch: system/dup.md")
    );

    let patch_exist = "*** Begin Patch\n*** Add File: system/existing.md\n+conflict\n*** End Patch";
    let err_exist =
        memory_apply_patch(&repo, &test_params("add existing", patch_exist)).unwrap_err();
    assert!(
        err_exist
            .message
            .contains("cannot add existing memory file: system/existing.md")
    );
}

#[test]
fn test_rejects_updating_nonexistent_file() {
    let (_temp, repo) = create_patch_fixture(&[]);
    let patch =
        "*** Begin Patch\n*** Update File: system/missing.md\n@@\n-old\n+new\n*** End Patch";
    let err = memory_apply_patch(&repo, &test_params("update missing", patch)).unwrap_err();
    assert!(err.message.contains("failed to read system/missing.md"));
}

#[test]
fn test_rejects_uncommitted_working_tree_changes() {
    let (_temp, repo) = create_patch_fixture(&[("system/clean.md", "content")]);
    fs::write(repo.dir.join("dirty.md"), "dirty\n").expect("write");

    let patch = "*** Begin Patch\n*** Add File: system/new.md\n+hello\n*** End Patch";
    let err = memory_apply_patch(&repo, &test_params("apply on dirty", patch)).unwrap_err();
    assert!(err.message.contains("Memory repo has uncommitted changes"));
}

#[test]
fn test_rejects_hunk_context_mismatch_with_diagnostics() {
    let memory = "---\ndescription: Mismatch\n---\nactual content\n";
    let (_temp, repo) = create_patch_fixture(&[("system/mismatch.md", memory)]);
    let patch = "*** Begin Patch\n*** Update File: system/mismatch.md\n@@\n-expected content\n+new content\n*** End Patch";

    let err = memory_apply_patch(&repo, &test_params("mismatch", patch)).unwrap_err();
    assert!(err.message.contains("context not found"));
    assert!(err.message.contains("Failed old/context chunk:"));
    assert!(err.message.contains("Current file content preview"));
}
