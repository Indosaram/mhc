use crate::tools::memory::{MemoryToolLock, MemoryToolProvenanceInput};
use crate::tools::memory_apply_patch::{MemoryApplyPatchParams, run_memory_apply_patch};
use crate::tools::memory_apply_patch_test_support::{
    RecordingLock, create_patch_fixture, git, matches_short_sha_message, memory_apply_patch,
    test_author, test_params,
};

#[test]
fn test_applies_delete_patch_operation() {
    let memory = "---\ndescription: Obsolete\n---\nto be removed";
    let (_temp, repo) = create_patch_fixture(&[("system/obsolete.md", memory)]);

    let patch = "*** Begin Patch\n*** Delete File: system/obsolete.md\n*** End Patch";
    let res = memory_apply_patch(&repo, &test_params("delete obsolete file", patch))
        .expect("apply delete");

    assert!(res.message.contains("memory_apply_patch committed locally"));
    let ls = repo.ls_tree(None, None).expect("ls_tree");
    assert!(!ls.contains(&"system/obsolete.md".to_string()));
}

#[test]
fn test_remote_sync_message_formatting() {
    let (_temp, repo) = create_patch_fixture(&[]);
    repo.config_set("remote.origin.url", "https://example.com/repo.git")
        .expect("config_set");

    let patch = "*** Begin Patch\n*** Add File: remote.md\n+content\n*** End Patch";
    let res = memory_apply_patch(&repo, &test_params("remote test", patch)).expect("apply");

    assert!(res.message.contains("harness will sync after the turn"));
}

#[test]
fn test_commits_omo_trailers_when_provenance_is_present() {
    // Pin `memory-apply-patch.ts:242-249`: present provenance appends the same three trailers the
    // memory tool writes.
    let (_temp, repo) = create_patch_fixture(&[]);
    let params = MemoryApplyPatchParams {
        reason: "patched with provenance".to_string(),
        input: "*** Begin Patch\n*** Add File: traced.md\n+content\n*** End Patch".to_string(),
        author: test_author(),
        provenance: Some(MemoryToolProvenanceInput {
            session_id: "session-9".to_string(),
            user_turns: 2,
            claim: None,
        }),
    };

    let res = memory_apply_patch(&repo, &params).expect("apply");

    let head = res.commit.expect("commit").sha;
    let commit = repo
        .log(None)
        .expect("log")
        .into_iter()
        .find(|commit| commit.sha == head)
        .expect("head commit");
    assert_eq!(
        commit.trailers.get("Omo-Writer").map(String::as_str),
        Some("memory-tool")
    );
    assert_eq!(
        commit.trailers.get("Omo-Session").map(String::as_str),
        Some("session-9")
    );
    assert_eq!(
        commit.trailers.get("Omo-Turn").map(String::as_str),
        Some("2")
    );
}

#[test]
fn test_non_origin_remote_reports_harness_sync() {
    let (_temp, repo) = create_patch_fixture(&[]);
    repo.config_set("remote.upstream.url", "https://example.com/upstream.git")
        .expect("config_set upstream remote");

    let patch = "*** Begin Patch\n*** Add File: upstream.md\n+content\n*** End Patch";
    let res = memory_apply_patch(&repo, &test_params("upstream test", patch)).expect("apply");

    assert!(res.message.contains("harness will sync after the turn"));
}

#[test]
fn given_a_delete_and_configured_remote_when_applied_then_writer_lock_deletion_and_remote_shape_are_pinned()
 {
    let (temp, repo) =
        create_patch_fixture(&[("system/delete.md", "---\ndescription: Delete me\n---\nbody")]);
    let remote = temp.path().join("remote.git");
    git(
        temp.path(),
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("utf-8 path"),
        ],
    );
    let lock = RecordingLock::default();
    let patch = "*** Begin Patch\n*** Delete File: system/delete.md\n*** End Patch";

    let res = run_memory_apply_patch(
        &repo,
        &test_params("remove stale memory", patch),
        Some(&lock as &dyn MemoryToolLock),
    )
    .expect("apply");

    assert_eq!(*lock.domains.borrow(), vec!["memory-write".to_string()]);
    assert!(repo.ls_tree(None, None).expect("ls_tree").is_empty());
    assert!(
        matches_short_sha_message(
            &res.message,
            "memory_apply_patch committed (",
            "); harness will sync after the turn."
        ),
        "{}",
        res.message
    );
    let commit = res.commit.expect("commit");
    assert_eq!(commit.subject, "remove stale memory");
    assert_eq!(commit.affected_paths, vec!["system/delete.md".to_string()]);
}

#[test]
fn given_a_rejected_reason_when_locked_then_the_lock_is_never_taken() {
    let (_temp, repo) = create_patch_fixture(&[]);
    let lock = RecordingLock::default();

    let error = run_memory_apply_patch(
        &repo,
        &test_params("  ", "*** Begin Patch\n*** End Patch"),
        Some(&lock as &dyn MemoryToolLock),
    )
    .unwrap_err();

    assert!(
        error
            .message
            .contains("'reason' must be a non-empty string")
    );
    assert!(lock.domains.borrow().is_empty());
}
