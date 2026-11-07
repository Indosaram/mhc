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
fn unicode_status_paths_are_raw_utf8() {
    let (_temp, repo) = create_repo();
    repo.init(Some(InitializeGitRepoOptions::default()))
        .expect("init");
    let path = "people/\u{ae40}\u{cca0}\u{c218}/card.md";
    fs::create_dir_all(repo.dir.join(path).parent().expect("card directory")).expect("mkdir");
    fs::write(
        repo.dir.join(path),
        "---\ndescription: Person\n---\nidentity\n",
    )
    .expect("card");
    let status = repo.status(&[] as &[&str]).expect("status");
    assert!(status.contains(path));
    assert!(!status.contains("\\352"));
}

#[test]
fn unicode_memory_path_can_be_committed_without_false_dirty_rejection() {
    let (_temp, repo) = create_repo();
    repo.init(Some(InitializeGitRepoOptions::default()))
        .expect("init");
    let before = repo.head().expect("head");
    let path = "people/\u{ae40}\u{cca0}\u{c218}/card.md";
    fs::create_dir_all(repo.dir.join(path).parent().expect("card directory")).expect("mkdir");
    fs::write(
        repo.dir.join(path),
        "---\ndescription: Person\n---\nidentity\n",
    )
    .expect("card");
    repo.commit_write(
        &[path],
        "remember person",
        &GitCommitAuthor {
            agent_id: "agent-one".into(),
            author_name: "Author".into(),
            author_email: None,
        },
    )
    .expect("commit");
    assert_ne!(repo.head().expect("head"), before);
    assert!(
        repo.ls_tree(Some("HEAD"), None)
            .expect("tree")
            .contains(&path.to_string())
    );
    assert!(
        repo.status(&[] as &[&str])
            .expect("status")
            .trim()
            .is_empty()
    );
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
            grep: None,
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
        fs::read_to_string(repo.dir.join("feature.md"))
            .expect("read")
            .replace("\r\n", "\n"),
        "feature\n"
    );
}

#[test]
fn path_log_includes_full_diff_under_follow_config() {
    let (_temp, repo) = create_repo();
    repo.init(None).unwrap();
    let author = GitCommitAuthor {
        agent_id: "agent-one".into(),
        author_name: "Author".into(),
        author_email: None,
    };
    fs::write(repo.dir.join("one.md"), "one\n").unwrap();
    fs::write(repo.dir.join("two.md"), "two\n").unwrap();
    repo.commit_write(&["one.md", "two.md"], "both", &author)
        .unwrap();
    repo.config_set("log.follow", "true").unwrap();
    repo.config_set("log.showSignature", "true").unwrap();
    let commits = repo
        .log(Some(&GitLogOptions {
            include_paths: true,
            paths: Some(vec!["one.md".into()]),
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].paths.as_ref().unwrap(), &["one.md", "two.md"]);
}

#[test]
fn line_ending_repair_restores_only_index_equivalent_copies() {
    let (_temp, repo) = create_repo();
    repo.init(None).unwrap();
    let author = GitCommitAuthor {
        agent_id: "agent-one".into(),
        author_name: "Author".into(),
        author_email: None,
    };
    fs::write(repo.dir.join("one.md"), "one\ntwo\n").unwrap();
    fs::write(repo.dir.join("edited.md"), "original\n").unwrap();
    repo.commit_write(&["one.md", "edited.md"], "notes", &author)
        .unwrap();
    repo.config_set("core.autocrlf", "true").unwrap();
    fs::write(repo.dir.join("one.md"), "one\r\ntwo\r\n").unwrap();
    fs::write(repo.dir.join("edited.md"), "real edit\r\n").unwrap();
    repo.init(None).unwrap();
    assert_eq!(fs::read(repo.dir.join("one.md")).unwrap(), b"one\ntwo\n");
    assert_eq!(
        fs::read(repo.dir.join("edited.md")).unwrap(),
        b"real edit\r\n"
    );
    assert_eq!(
        repo.config_get("core.autocrlf").unwrap().as_deref(),
        Some("false")
    );
}

#[test]
fn commit_times_ignore_follow_and_signature_settings() {
    let (_temp, repo) = create_repo();
    repo.init(None).unwrap();
    fs::write(repo.dir.join("one.md"), "one\n").unwrap();
    repo.commit_write(
        &["one.md"],
        "note",
        &GitCommitAuthor {
            agent_id: "agent-one".into(),
            author_name: "Author".into(),
            author_email: None,
        },
    )
    .unwrap();
    let plain = repo.path_commit_times("HEAD").unwrap();
    assert!(plain["one.md"] >= 0);
    repo.config_set("log.follow", "true").unwrap();
    repo.config_set("log.showSignature", "true").unwrap();
    let reopened = GitMemoryRepo::open(&repo.dir, "agent-one").unwrap();
    assert_eq!(reopened.path_commit_times("HEAD").unwrap(), plain);
}

/// Records every argv and timeout it runs, then runs the real git.
pub(crate) struct RecordingExec {
    pub(crate) inner: Arc<dyn super::exec::GitExec>,
    pub(crate) calls: std::sync::Mutex<Vec<(Vec<String>, u64)>>,
}

impl RecordingExec {
    pub(crate) fn system() -> Arc<Self> {
        Arc::new(Self {
            inner: super::exec::system_git_exec(),
            calls: Default::default(),
        })
    }

    /// The recorded call whose argv contains `command`, with its timeout.
    pub(crate) fn call_with(&self, command: &str) -> (Vec<String>, u64) {
        let calls = self.calls.lock().unwrap();
        calls
            .iter()
            .find(|(argv, _)| argv.iter().any(|arg| arg == command))
            .cloned()
            .unwrap_or_else(|| panic!("no git {command} call in {calls:?}"))
    }
}

impl super::exec::GitExec for RecordingExec {
    fn run(
        &self,
        argv: &[String],
        options: &super::exec::GitExecOptions,
    ) -> std::io::Result<super::exec::GitExecResult> {
        self.calls
            .lock()
            .unwrap()
            .push((argv.to_vec(), options.timeout_ms));
        self.inner.run(argv, options)
    }
}

/// `repo.ts`: every commit and merge runs with the commit budget (the hooks run inside it) and
/// without auto maintenance; plain reads keep the shorter git budget.
#[test]
fn commits_and_merges_get_the_commit_budget_without_auto_maintenance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let exec = Arc::new(RecordingExec {
        inner: super::exec::system_git_exec(),
        calls: Default::default(),
    });
    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: temp.path().to_path_buf(),
        agent_id: "agent-one".to_string(),
        exec: Some(exec.clone()),
        install_hooks: None,
    })
    .expect("repo");
    repo.init(Some(InitializeGitRepoOptions {
        seed_files: vec![GitSeedFile {
            relative_path: "seed.md".into(),
            content: "seed\n".into(),
        }],
        ..Default::default()
    }))
    .expect("init");
    fs::write(temp.path().join("fact.md"), "fact\n").expect("write");
    let author = GitCommitAuthor {
        agent_id: "agent-one".into(),
        author_name: "Agent One".into(),
        author_email: None,
    };
    repo.commit_write(&["fact.md"], "record fact", &author)
        .expect("commit");

    let calls = exec.calls.lock().unwrap().clone();
    let commits: Vec<&(Vec<String>, u64)> = calls
        .iter()
        .filter(|(argv, _)| argv.iter().any(|arg| arg == "commit"))
        .collect();
    assert_eq!(commits.len(), 2, "the seed commit and the write: {calls:?}");
    for (argv, timeout) in commits {
        assert_eq!(
            &argv[..4],
            &super::repo::NO_AUTO_MAINTENANCE.map(str::to_string)[..],
            "{argv:?}"
        );
        assert_eq!(*timeout, super::repo::COMMIT_TIMEOUT_MS, "{argv:?}");
    }
    let status = calls
        .iter()
        .find(|(argv, _)| argv.iter().any(|arg| arg == "status"))
        .expect("a status read");
    assert_eq!(status.1, 30_000, "reads keep the git budget");
}

/// Installs `pre-commit` and `commit-msg` hooks that record their own runs in `.git/hook-runs`.
fn recording_hooks() -> super::repo_types::GitHookInstaller {
    Arc::new(|dir: &std::path::Path| {
        let hooks = dir.join(".git").join("hooks");
        fs::create_dir_all(&hooks).map_err(GitError::Io)?;
        for stage in ["pre-commit", "commit-msg"] {
            let path = hooks.join(stage);
            fs::write(
                &path,
                format!("#!/bin/sh\necho {stage} >> .git/hook-runs\n"),
            )
            .map_err(GitError::Io)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                    .map_err(GitError::Io)?;
            }
        }
        Ok(())
    })
}

fn hook_runs(dir: &std::path::Path) -> String {
    fs::read_to_string(dir.join(".git").join("hook-runs")).unwrap_or_default()
}

/// The initial commit (seeded or empty) runs no commit check, so opening a fresh memory repository
/// starts no checker process; the first write after it runs every check.
#[test]
fn the_initial_commit_runs_no_commit_checks_and_later_commits_run_them_all() {
    for seed_files in [
        vec![GitSeedFile {
            relative_path: "system/persona.md".into(),
            content: "---\ndescription: Persona\n---\nbody\n".into(),
        }],
        Vec::new(),
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
            dir: temp.path().to_path_buf(),
            agent_id: "agent-one".to_string(),
            exec: None,
            install_hooks: Some(recording_hooks()),
        })
        .expect("repo");
        let seeded = !seed_files.is_empty();
        repo.init(Some(InitializeGitRepoOptions {
            seed_files,
            ..Default::default()
        }))
        .expect("init");
        assert!(repo.head().expect("head").is_some(), "seeded={seeded}");
        assert_eq!(hook_runs(temp.path()), "", "seeded={seeded}");

        fs::write(temp.path().join("fact.md"), "fact\n").expect("write");
        let author = GitCommitAuthor {
            agent_id: "agent-one".into(),
            author_name: "Agent One".into(),
            author_email: None,
        };
        repo.commit_write(&["fact.md"], "record fact", &author)
            .expect("commit");
        assert_eq!(
            hook_runs(temp.path()),
            "pre-commit\ncommit-msg\n",
            "seeded={seeded}"
        );
    }
}
