use crate::tools::memory_apply_patch_test_support::{
    create_patch_fixture, memory_apply_patch, test_params,
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
