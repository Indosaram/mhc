use crate::tools::memory::MemoryToolProvenanceInput;
use crate::tools::memory_apply_patch::MemoryApplyPatchParams;
use crate::tools::memory_apply_patch_test_support::{
    create_patch_fixture, memory_apply_patch, test_author, test_params,
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
