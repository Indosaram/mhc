use pretty_assertions::assert_eq;

use crate::git::GitMemoryRepo;
use crate::tools::memory::{MemoryToolParams, run_memory_tool};
use crate::tools::memory_apply_patch_test_support::{
    create_patch_fixture, git, matches_short_sha_message, memory_apply_patch, test_author,
    test_params,
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

#[test]
fn given_an_add_without_frontmatter_when_applied_then_reason_and_author_commit() {
    let (temp, repo) = create_patch_fixture(&[]);
    let reason = "remember a contact";
    let patch =
        "*** Begin Patch\n*** Add File: system/contact.md\n+Sarah: cofounder\n*** End Patch";

    let res = memory_apply_patch(&repo, &test_params(reason, patch)).expect("apply");

    assert!(
        matches_short_sha_message(&res.message, "memory_apply_patch committed locally (", ")."),
        "{}",
        res.message
    );
    let author = test_author();
    assert_eq!(
        git(temp.path(), &["log", "-1", "--pretty=format:%s%n%an%n%ae"]),
        format!(
            "{reason}\n{}\n{}",
            author.author_name,
            author.author_email.expect("email")
        )
    );
    let commit = res.commit.expect("commit");
    assert_eq!(commit.subject, reason);
    assert_eq!(commit.affected_paths, vec!["system/contact.md".to_string()]);
}

#[derive(Clone, Copy)]
enum ChainCommand {
    ApplyPatch,
    MemoryTool,
}

fn apply_chain(repo: &GitMemoryRepo, command: ChainCommand, input: &str) -> Vec<String> {
    let commit = match command {
        ChainCommand::ApplyPatch => {
            memory_apply_patch(repo, &test_params("chain moves", input))
                .expect("apply_patch")
                .commit
        }
        ChainCommand::MemoryTool => {
            run_memory_tool(
                repo,
                &test_author(),
                &MemoryToolParams {
                    command: Some("apply_patch".to_string()),
                    reason: "chain moves".to_string(),
                    input: Some(input.to_string()),
                    ..Default::default()
                },
                None,
            )
            .expect("memory apply_patch")
            .commit
        }
    };
    let mut paths = commit.expect("commit").affected_paths;
    paths.sort();
    paths
}

fn assert_chained_moves(command: ChainCommand, tracked_intermediate: bool) {
    let memory = "---\ndescription: Notes\n---\nold";
    let mut seeds = vec![("system/a.md", memory)];
    if tracked_intermediate {
        seeds.push(("system/c.md", memory));
    }
    let (temp, repo) = create_patch_fixture(&seeds);
    let mut lines = vec!["*** Begin Patch"];
    if tracked_intermediate {
        lines.push("*** Delete File: system/c.md");
    }
    lines.extend([
        "*** Update File: system/a.md",
        "*** Move to: system/c.md",
        "@@",
        "-old",
        "+middle",
        "*** Update File: system/c.md",
        "*** Move to: system/d.md",
        "@@",
        "-middle",
        "+final",
        "*** End Patch",
    ]);

    let affected = apply_chain(&repo, command, &lines.join("\n"));

    let expected: &[&str] = if tracked_intermediate {
        &["system/a.md", "system/c.md", "system/d.md"]
    } else {
        &["system/a.md", "system/d.md"]
    };
    assert_eq!(affected, expected);
    assert_eq!(
        repo.ls_tree(None, None).expect("ls_tree"),
        vec!["system/d.md"]
    );
    assert!(
        repo.show("HEAD", "system/d.md")
            .expect("show")
            .contains("final")
    );
    assert_eq!(repo.status(&[] as &[&str]).expect("status").trim(), "");
    git(temp.path(), &["diff", "--cached", "--quiet"]);

    apply_chain(
        &repo,
        command,
        "*** Begin Patch\n*** Add File: system/followup.md\n+after chain\n*** End Patch",
    );
    assert_eq!(
        repo.ls_tree(None, None).expect("ls_tree"),
        vec!["system/d.md", "system/followup.md"]
    );
    assert_eq!(repo.status(&[] as &[&str]).expect("status").trim(), "");
}

#[test]
fn given_an_absent_intermediate_when_memory_apply_patch_chains_moves_then_only_real_paths_commit() {
    assert_chained_moves(ChainCommand::ApplyPatch, false);
}

#[test]
fn given_a_tracked_intermediate_when_memory_apply_patch_chains_moves_then_only_real_paths_commit() {
    assert_chained_moves(ChainCommand::ApplyPatch, true);
}

#[test]
fn given_an_absent_intermediate_when_memory_chains_moves_then_only_real_paths_commit() {
    assert_chained_moves(ChainCommand::MemoryTool, false);
}

#[test]
fn given_a_tracked_intermediate_when_memory_chains_moves_then_only_real_paths_commit() {
    assert_chained_moves(ChainCommand::MemoryTool, true);
}

#[test]
fn given_an_added_credential_assignment_when_applied_then_it_is_refused_and_index_and_worktree_restored()
 {
    let (temp, repo) = create_patch_fixture(&[]);
    let head_before = repo.head().expect("head");
    let patch = "*** Begin Patch\n*** Add File: notes/a.md\n+---\n+description: Note\n+---\n+the token is token=abc123456\n*** End Patch";

    let error = memory_apply_patch(&repo, &test_params("add secret note", patch)).unwrap_err();

    assert!(
        error.message.contains(
            "refused: notes/a.md contains secret-like content (credential_assignment); remove it and retry"
        ),
        "{}",
        error.message
    );
    assert!(!error.message.contains("abc123456"));
    assert_eq!(repo.head().expect("head"), head_before);
    assert_eq!(repo.status(&[] as &[&str]).expect("status").trim(), "");
    git(temp.path(), &["diff", "--cached", "--quiet"]);
}

#[test]
fn given_a_memory_create_carrying_a_credential_when_committed_then_it_is_refused_and_restored() {
    let (temp, repo) = create_patch_fixture(&[]);
    let head_before = repo.head().expect("head");

    let error = run_memory_tool(
        &repo,
        &test_author(),
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "save secret".to_string(),
            file_path: Some("notes/b.md".to_string()),
            description: Some("Note".to_string()),
            file_text: Some("password=hunter2Abrx9".to_string()),
            ..Default::default()
        },
        None,
    )
    .unwrap_err();

    assert!(
        error
            .message
            .contains("refused: notes/b.md contains secret-like content"),
        "{}",
        error.message
    );
    assert!(!error.message.contains("hunter2Abrx9"));
    assert_eq!(repo.head().expect("head"), head_before);
    assert_eq!(repo.status(&[] as &[&str]).expect("status").trim(), "");
    git(temp.path(), &["diff", "--cached", "--quiet"]);
}
