use pretty_assertions::assert_eq;

use tempfile::TempDir;

use crate::git::exec::system_git_exec;
use crate::git::{GitCommitAuthor, GitMemoryRepo, GitMemoryRepoOptions, InitializeGitRepoOptions};

use crate::reflection::machine::ReflectionOutcome;
use crate::reflection::worktree::{
    ReflectionFinalizeMode, create_reflection_worktree, discard_reflection_worktree,
    finalize_reflection_worktree,
};

#[test]
fn given_an_absolute_worktrees_dir_when_created_then_worktree_files_and_branch_are_established() {
    let temp = TempDir::new().expect("temp dir");
    let parent_dir = temp.path().join("parent");
    let worktrees_dir = temp.path().join("worktrees");

    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: parent_dir,
        agent_id: "agent-wt".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    repo.init(InitializeGitRepoOptions {
        seed_files: Vec::new(),
        author_name: Some("Worktree Agent".to_string()),
        install_hooks: None,
    })
    .expect("repo init");

    let exec = system_git_exec();
    let worktree =
        create_reflection_worktree(&repo, "run-create-wt", &worktrees_dir, exec.as_ref(), None)
            .expect("create worktree");

    assert!(worktree.dir.exists());
    assert!(worktree.git_file_path.exists());
    assert!(worktree.branch.starts_with("memory/reflection-"));

    let cleanup =
        discard_reflection_worktree(&repo, &worktree.dir, &worktree.branch, exec.as_ref());
    assert!(cleanup.worktree_removed);
    assert!(cleanup.branch_removed);
}

#[test]
fn given_a_worktree_with_committed_changes_when_finalized_then_it_merges_and_cleans_up() {
    let temp = TempDir::new().expect("temp dir");
    let parent_dir = temp.path().join("parent");
    let worktrees_dir = temp.path().join("worktrees");

    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: parent_dir,
        agent_id: "agent-wt-finalize".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    repo.init(InitializeGitRepoOptions {
        seed_files: Vec::new(),
        author_name: Some("Finalize Agent".to_string()),
        install_hooks: None,
    })
    .expect("repo init");

    let exec = system_git_exec();
    let worktree =
        create_reflection_worktree(&repo, "run-finalize", &worktrees_dir, exec.as_ref(), None)
            .expect("create worktree");

    let test_file = worktree.dir.join("test.md");
    std::fs::write(&test_file, "content\n").expect("write test file");

    let child_repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: worktree.dir.clone(),
        agent_id: "agent-wt-finalize".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    child_repo
        .commit_write(
            &["test.md"],
            "reflect test",
            &GitCommitAuthor {
                agent_id: "agent-wt-finalize".to_string(),
                author_name: "Finalize Agent".to_string(),
                author_email: None,
            },
        )
        .expect("child commit");

    let finalize_mode = ReflectionFinalizeMode::Auto {
        summary: "test summary".to_string(),
        run_id: Some("run-finalize".to_string()),
        allowed_paths: None,
    };

    let result = finalize_reflection_worktree(&worktree, finalize_mode, |op| op());
    assert_eq!(result.status, ReflectionOutcome::Merged);
    assert!(result.cleanup.worktree_removed);
    assert!(result.cleanup.branch_removed);
}
