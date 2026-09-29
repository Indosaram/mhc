use pretty_assertions::assert_eq;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::errors::GitError;
use super::repo::GitMemoryRepo;
use super::repo_types::{
    GitCommitAuthor, GitLogOptions, GitMemoryRepoOptions, GitMergeOptions, GitSeedFile,
    InitializeGitRepoOptions,
};

fn create_repo() -> (tempfile::TempDir, GitMemoryRepo) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(temp_dir.path(), "agent-one").expect("repo");
    (temp_dir, repo)
}

#[test]
fn test_initializes_repository_idempotently_with_seed_files() {
    let (_temp, repo) = create_repo();

    let sha = repo
        .init(InitializeGitRepoOptions {
            author_name: Some("Bootstrap Agent".to_string()),
            seed_files: vec![
                GitSeedFile {
                    relative_path: "system/seed.md".to_string(),
                    content: "seeded\n".to_string(),
                },
                GitSeedFile {
                    relative_path: "empty.md".to_string(),
                    content: String::new(),
                },
            ],
            install_hooks: None,
        })
        .expect("init");

    assert_eq!(sha.len(), 40);
    assert_eq!(repo.head().expect("head"), Some(sha.clone()));
    assert_eq!(
        repo.config_get("omo.agentId").expect("agentId"),
        Some("agent-one".to_string())
    );
    assert_eq!(
        repo.config_get("commit.gpgsign").expect("gpgsign"),
        Some("false".to_string())
    );
    assert_eq!(
        repo.config_get("gc.auto").expect("gc.auto"),
        Some("0".to_string())
    );

    let second_sha = repo.init(None).expect("second init");
    assert_eq!(second_sha, sha);
}

#[test]
fn test_supports_custom_author_names_for_empty_initial_commit() {
    let (_temp, repo) = create_repo();

    let sha = repo
        .init(InitializeGitRepoOptions {
            author_name: Some("Custom Agent Name".to_string()),
            seed_files: vec![],
            install_hooks: None,
        })
        .expect("init");

    let commits = repo.log(None).expect("log");
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].sha, sha);
    assert_eq!(commits[0].author_name, "Custom Agent Name");
    assert_eq!(commits[0].author_email, "agent-one@omo.local");
}

#[test]
fn test_runs_hook_installer_during_init_and_commit_boundaries() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let hook_calls = Arc::new(AtomicUsize::new(0));
    let hook_calls_clone = Arc::clone(&hook_calls);

    let hook_installer = Arc::new(move |_dir: &std::path::Path| {
        hook_calls_clone.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: temp_dir.path().to_path_buf(),
        agent_id: "agent-one".to_string(),
        exec: None,
        install_hooks: Some(hook_installer),
    })
    .expect("repo");

    repo.init(None).expect("init");
    repo.clean_check().expect("clean_check");

    fs::write(temp_dir.path().join("fact.md"), "fact\n").expect("write");
    let author = GitCommitAuthor {
        agent_id: "agent-one".to_string(),
        author_name: "Agent One".to_string(),
        author_email: None,
    };
    repo.commit_write(&["fact.md"], "record fact", &author)
        .expect("commit");

    assert!(hook_calls.load(Ordering::SeqCst) >= 3);
}

#[test]
fn test_explicit_author_flags_control_attribution() {
    let (_temp, repo) = create_repo();
    repo.init(None).expect("init");
    repo.config_set("user.name", "Wrong Local Name")
        .expect("set name");
    repo.config_set("user.email", "wrong@example.com")
        .expect("set email");

    fs::write(repo.dir.join("fact.md"), "fact\n").expect("write");
    repo.commit_write(
        &["fact.md"],
        "remember fact",
        &GitCommitAuthor {
            agent_id: "agent-one".to_string(),
            author_name: "Correct Agent".to_string(),
            author_email: None,
        },
    )
    .expect("commit");

    let commits = repo.log(None).expect("log");
    assert_eq!(commits[0].author_name, "Correct Agent");
    assert_eq!(commits[0].author_email, "agent-one@omo.local");
}

#[test]
fn test_refuses_to_commit_unrelated_changes() {
    let (_temp, repo) = create_repo();
    repo.init(None).expect("init");

    fs::write(repo.dir.join("related.md"), "related\n").expect("write");
    fs::write(repo.dir.join("unrelated.md"), "unrelated\n").expect("write");

    let author = GitCommitAuthor {
        agent_id: "agent-one".to_string(),
        author_name: "Agent".to_string(),
        author_email: None,
    };

    let err = repo
        .commit_write(&["related.md"], "try commit", &author)
        .unwrap_err();

    match err {
        GitError::DirtyRepo { porcelain, .. } => {
            assert!(porcelain.contains("unrelated.md"));
        }
        _ => panic!("expected DirtyRepo error"),
    }
}

#[test]
fn test_reports_dirty_markdown_encoding_issues() {
    let (_temp, repo) = create_repo();
    repo.init(None).expect("init");

    let utf16le = vec![0xff, 0xfe, b'a', 0x00, b'\n', 0x00];
    fs::write(repo.dir.join("bad.md"), &utf16le).expect("write");

    let err = repo.clean_check().unwrap_err();
    match err {
        GitError::DirtyRepo {
            encoding_diagnostics,
            ..
        } => {
            assert_eq!(
                encoding_diagnostics,
                vec!["bad.md has UTF-16 LE BOM".to_string()]
            );
        }
        _ => panic!("expected DirtyRepo error"),
    }
}

#[test]
fn test_rejects_clean_check_on_dirty_changes() {
    let (_temp, repo) = create_repo();
    repo.init(None).expect("init");
    repo.clean_check().expect("clean initially");

    fs::write(repo.dir.join("dirty.md"), "dirty\n").expect("write");
    assert!(repo.clean_check().is_err());
}

#[test]
fn test_reports_no_effective_changes_error() {
    let (_temp, repo) = create_repo();
    repo.init(None).expect("init");

    let author = GitCommitAuthor {
        agent_id: "agent-one".to_string(),
        author_name: "Agent".to_string(),
        author_email: None,
    };

    let empty_err = repo
        .commit_write(&[] as &[&str], "empty", &author)
        .unwrap_err();
    match empty_err {
        GitError::NoEffectiveChanges { .. } => {}
        _ => panic!("expected NoEffectiveChanges error"),
    }

    fs::write(repo.dir.join("clean.md"), "clean\n").expect("write");
    repo.commit_write(&["clean.md"], "clean commit", &author)
        .expect("first commit");

    let no_diff_err = repo
        .commit_write(&["clean.md"], "same commit", &author)
        .unwrap_err();
    match no_diff_err {
        GitError::NoEffectiveChanges { .. } => {}
        _ => panic!("expected NoEffectiveChanges error"),
    }
}

#[test]
fn test_shows_and_logs_history_with_trailers_and_paths() {
    let (_temp, repo) = create_repo();
    repo.init(None).expect("init");

    fs::write(repo.dir.join("item.md"), "content\n").expect("write");
    let author = GitCommitAuthor {
        agent_id: "agent-one".to_string(),
        author_name: "Agent".to_string(),
        author_email: None,
    };

    repo.commit_write(
        &["item.md"],
        "add item\n\nProvenance: test-harness",
        &author,
    )
    .expect("commit");

    let log = repo
        .log(Some(&GitLogOptions {
            range: None,
            paths: None,
            limit: Some(1),
            include_paths: true,
        }))
        .expect("log");

    assert_eq!(log.len(), 1);
    assert_eq!(log[0].subject, "add item");
    assert_eq!(
        log[0].trailers.get("Provenance"),
        Some(&"test-harness".to_string())
    );
    assert_eq!(log[0].paths, Some(vec!["item.md".to_string()]));

    let content = repo.show("HEAD", "item.md").expect("show");
    assert_eq!(content, "content\n");

    let ls = repo.ls_tree(None, None).expect("ls_tree");
    assert!(ls.contains(&"item.md".to_string()));
}

#[test]
fn test_supports_worktrees_and_merges() {
    let (temp_dir, repo) = create_repo();
    repo.init(None).expect("init");

    let worktree_dir = temp_dir.path().join("wt");
    repo.worktree_add(&worktree_dir, "feature", None)
        .expect("add");

    fs::write(worktree_dir.join("feature.md"), "feature\n").expect("write");
    let wt_repo = GitMemoryRepo::open(&worktree_dir, "agent-one").expect("open wt");
    let author = GitCommitAuthor {
        agent_id: "agent-one".to_string(),
        author_name: "Agent".to_string(),
        author_email: None,
    };
    wt_repo
        .commit_write(&["feature.md"], "feature commit", &author)
        .expect("commit in wt");

    repo.worktree_remove(&worktree_dir, true).expect("remove");

    let merged_sha = repo
        .merge(
            "feature",
            Some(&GitMergeOptions {
                no_ff: Some(true),
                message: Some("merge feature branch".to_string()),
            }),
        )
        .expect("merge");

    assert_eq!(merged_sha.len(), 40);
    assert_eq!(
        fs::read_to_string(repo.dir.join("feature.md")).expect("read"),
        "feature\n"
    );
}
