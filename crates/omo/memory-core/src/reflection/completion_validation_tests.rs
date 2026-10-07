use pretty_assertions::assert_eq;

use tempfile::TempDir;

use crate::git::exec::system_git_exec;
use crate::git::{
    GitCommitAuthor, GitMemoryRepo, GitMemoryRepoOptions, GitSeedFile, InitializeGitRepoOptions,
};

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

fn frontmatter_fixture(
    seed_files: Vec<GitSeedFile>,
) -> (
    TempDir,
    GitMemoryRepo,
    crate::reflection::worktree::ReflectionWorktree,
    std::sync::Arc<dyn crate::git::exec::GitExec>,
) {
    let temp = TempDir::new().expect("temp dir");
    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: temp.path().join("parent"),
        agent_id: "agent-frontmatter".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");
    repo.init(InitializeGitRepoOptions {
        seed_files,
        author_name: Some("Validation Agent".to_string()),
        install_hooks: None,
    })
    .expect("repo init");
    let exec = system_git_exec();
    let worktree = create_reflection_worktree(
        &repo,
        "run-frontmatter",
        &temp.path().join("worktrees"),
        exec.as_ref(),
        None,
    )
    .expect("create worktree");
    (temp, repo, worktree, exec)
}

fn commit_worktree(
    worktree: &crate::reflection::worktree::ReflectionWorktree,
    paths: &[&str],
    reason: &str,
) {
    let child = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: worktree.dir.clone(),
        agent_id: "agent-frontmatter".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open child repo");
    child
        .commit_write(
            paths,
            reason,
            &GitCommitAuthor {
                agent_id: "agent-frontmatter".to_string(),
                author_name: "Validation Agent".to_string(),
                author_email: None,
            },
        )
        .expect("commit write");
}

#[test]
fn given_a_commit_with_malformed_frontmatter_when_validated_then_it_fails() {
    let (_temp, _repo, worktree, exec) = frontmatter_fixture(Vec::new());
    std::fs::create_dir_all(worktree.dir.join("system")).expect("system dir");
    std::fs::write(
        worktree.dir.join("system/notes.md"),
        "---\ndescription: by default: verify\n---\nbody\n",
    )
    .expect("write");
    commit_worktree(&worktree, &["system/notes.md"], "reflect: notes");

    match validate_completion(&worktree, &worktree.base_commit_sha, exec.as_ref()) {
        CompletionValidation::Failed { detail } => {
            assert!(detail.contains("invalid frontmatter in system/notes.md"), "{detail}");
            assert!(detail.contains("safe YAML plain scalar"), "{detail}");
        }
        other => panic!("expected Failed completion, got {other:?}"),
    }
}

#[test]
fn given_a_body_only_edit_of_legacy_frontmatter_when_validated_then_it_passes() {
    let seed = vec![GitSeedFile {
        relative_path: "system/legacy.md".to_string(),
        content: "---\ndescription: true\n---\nfirst\n".to_string(),
    }];
    let (_temp, _repo, worktree, exec) = frontmatter_fixture(seed);
    std::fs::write(
        worktree.dir.join("system/legacy.md"),
        "---\ndescription: true\n---\nsecond\n",
    )
    .expect("write");
    commit_worktree(&worktree, &["system/legacy.md"], "reflect: body only");

    match validate_completion(&worktree, &worktree.base_commit_sha, exec.as_ref()) {
        CompletionValidation::Valid { changed_paths, .. } => {
            assert_eq!(changed_paths, vec!["system/legacy.md"])
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
}

#[test]
fn given_malformed_frontmatter_outside_the_content_set_when_validated_then_it_passes() {
    let (_temp, _repo, worktree, exec) = frontmatter_fixture(Vec::new());
    std::fs::create_dir_all(worktree.dir.join("notes")).expect("notes dir");
    std::fs::write(
        worktree.dir.join("notes/scratch.md"),
        "---\ndescription: by default: verify\n---\nbody\n",
    )
    .expect("write");
    commit_worktree(&worktree, &["notes/scratch.md"], "reflect: scratch");

    match validate_completion(&worktree, &worktree.base_commit_sha, exec.as_ref()) {
        CompletionValidation::Valid { changed_paths, .. } => {
            assert_eq!(changed_paths, vec!["notes/scratch.md"])
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
}
