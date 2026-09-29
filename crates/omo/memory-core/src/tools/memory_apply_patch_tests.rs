use pretty_assertions::assert_eq;

use crate::tools::memory_apply_patch_test_support::{
    create_patch_fixture, memory_apply_patch, test_params,
};

#[test]
fn test_synthesizes_description_and_commits_add() {
    let (_temp, repo) = create_patch_fixture(&[]);
    let patch =
        "*** Begin Patch\n*** Add File: system/contact.md\n+Sarah: cofounder\n*** End Patch";

    let res = memory_apply_patch(&repo, &test_params("remember contact", patch)).expect("apply");
    assert!(res.message.contains("memory_apply_patch committed locally"));

    let content = repo.show("HEAD", "system/contact.md").expect("show");
    assert_eq!(
        content,
        "---\ndescription: Memory block system/contact\n---\nSarah: cofounder"
    );
}

#[test]
fn test_supports_add_then_update_in_one_patch() {
    let (_temp, repo) = create_patch_fixture(&[]);
    let patch = "*** Begin Patch\n*** Add File: system/facts.md\n+old fact\n*** Update File: system/facts.md\n@@\n-old fact\n+new fact\n*** End Patch";

    memory_apply_patch(&repo, &test_params("add and update", patch)).expect("apply");
    let content = repo.show("HEAD", "system/facts.md").expect("show");
    assert!(content.contains("new fact"));
}

#[test]
fn test_supports_move_followed_by_an_edit() {
    let memory = "---\ndescription: Notes\n---\nold";
    let (_temp, repo) = create_patch_fixture(&[("system/source.md", memory)]);
    let patch = "*** Begin Patch\n*** Update File: system/source.md\n*** Move to: system/target.md\n@@\n-old\n+middle\n*** Update File: system/target.md\n@@\n-middle\n+final\n*** End Patch";

    memory_apply_patch(&repo, &test_params("move and edit", patch)).expect("apply");

    let ls = repo.ls_tree(None, None).expect("ls_tree");
    assert_eq!(ls, vec!["system/target.md".to_string()]);

    let content = repo.show("HEAD", "system/target.md").expect("show");
    assert!(content.contains("final"));
}

#[test]
fn test_handles_final_line_without_newline() {
    let memory = "---\ndescription: Tail\n---\ntail";
    let (_temp, repo) = create_patch_fixture(&[("system/tail.md", memory)]);
    let patch =
        "*** Begin Patch\n*** Update File: system/tail.md\n@@\n-tail\n+changed\n*** End Patch";

    memory_apply_patch(&repo, &test_params("change tail", patch)).expect("apply");

    let content = repo.show("HEAD", "system/tail.md").expect("show");
    assert_eq!(content, "---\ndescription: Tail\n---\nchanged");
}
