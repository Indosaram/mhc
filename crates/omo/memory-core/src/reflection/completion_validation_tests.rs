use pretty_assertions::assert_eq;

use tempfile::TempDir;

use crate::git::exec::system_git_exec;
use crate::git::{GitCommitAuthor, GitMemoryRepo, GitMemoryRepoOptions, InitializeGitRepoOptions};

use crate::reflection::completion_validation::{CompletionValidation, validate_completion};
use crate::reflection::worktree::create_reflection_worktree;

#[test]
fn given_an_uncommitted_worktree_edit_when_validated_then_it_reports_dirty_uncommitted() {
    let temp = TempDir::new().expect("temp dir");
    let parent_dir = temp.path().join("parent");
    let worktrees_dir = temp.path().join("worktrees");

    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: parent_dir,
        agent_id: "agent-validation".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    repo.init(InitializeGitRepoOptions {
        seed_files: Vec::new(),
        author_name: Some("Validation Agent".to_string()),
        install_hooks: None,
    })
    .expect("repo init");

    let exec = system_git_exec();
    let worktree =
        create_reflection_worktree(&repo, "run-dirty", &worktrees_dir, exec.as_ref(), None)
            .expect("create worktree");

    let dirty_file = worktree.dir.join("dirty.txt");
    std::fs::write(&dirty_file, "dirty content\n").expect("write dirty");

    let validation = validate_completion(&worktree, &worktree.base_commit_sha, exec.as_ref());
    assert!(matches!(
        validation,
        CompletionValidation::DirtyUncommitted { .. }
    ));
}

#[test]
fn given_altered_git_administration_file_when_validated_then_validation_fails() {
    let temp = TempDir::new().expect("temp dir");
    let parent_dir = temp.path().join("parent");
    let worktrees_dir = temp.path().join("worktrees");

    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: parent_dir,
        agent_id: "agent-tamper".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    repo.init(InitializeGitRepoOptions {
        seed_files: Vec::new(),
        author_name: Some("Tamper Agent".to_string()),
        install_hooks: None,
    })
    .expect("repo init");

    let exec = system_git_exec();
    let worktree =
        create_reflection_worktree(&repo, "run-tamper", &worktrees_dir, exec.as_ref(), None)
            .expect("create worktree");

    let git_file = &worktree.git_file_path;
    let original = std::fs::read_to_string(git_file).expect("read git file");
    std::fs::write(git_file, format!("{original}# tampered\n")).expect("tamper git file");

    let validation = validate_completion(&worktree, &worktree.base_commit_sha, exec.as_ref());
    match validation {
        CompletionValidation::Failed { detail } => {
            assert!(detail.contains("administration"));
        }
        _ => panic!("Expected Failed validation"),
    }
}

#[test]
fn given_clean_worktree_with_valid_commit_when_validated_then_validation_succeeds() {
    let temp = TempDir::new().expect("temp dir");
    let parent_dir = temp.path().join("parent");
    let worktrees_dir = temp.path().join("worktrees");

    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: parent_dir,
        agent_id: "agent-valid".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    repo.init(InitializeGitRepoOptions {
        seed_files: Vec::new(),
        author_name: Some("Valid Agent".to_string()),
        install_hooks: None,
    })
    .expect("repo init");

    let exec = system_git_exec();
    let worktree =
        create_reflection_worktree(&repo, "run-valid", &worktrees_dir, exec.as_ref(), None)
            .expect("create worktree");

    let valid_file = worktree.dir.join("learned.md");
    std::fs::write(&valid_file, "learned facts\n").expect("write file");

    let child_repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: worktree.dir.clone(),
        agent_id: "agent-valid".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    child_repo
        .commit_write(
            &["learned.md"],
            "reflect: learned facts",
            &GitCommitAuthor {
                agent_id: "agent-valid".to_string(),
                author_name: "Valid Agent".to_string(),
                author_email: None,
            },
        )
        .expect("commit write");

    let validation = validate_completion(&worktree, &worktree.base_commit_sha, exec.as_ref());
    match validation {
        CompletionValidation::Valid {
            tip_sha,
            changed_paths,
        } => {
            assert_eq!(changed_paths, vec!["learned.md"]);
            assert!(!tip_sha.is_empty());
        }
        _ => panic!("Expected Valid completion"),
    }
}
