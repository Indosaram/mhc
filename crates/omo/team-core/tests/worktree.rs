//! Translated from src/team-worktree/{manager,cleanup}.test.ts.

use std::fs;
use std::path::Path;
use std::process::Command;

use team_core::error::TeamCoreError;
use team_core::team_worktree::{
    GitOutput, WorktreeConfig, create_worktree, create_worktree_with, find_orphan_worktrees,
    remove_worktree, validate_worktree_spec,
};
use tempfile::TempDir;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn normalize_git_path_text(value: &str) -> String {
    value
        .replace('\\', "/")
        .replace("/private/var/", "/var/")
        .to_lowercase()
}

/// `initGitRepo`: the repo lives in `<tmp>/repo` so `../worktree-*` stays inside the temp dir.
fn init_git_repo() -> (TempDir, std::path::PathBuf) {
    let root = tempfile::Builder::new()
        .prefix("team-worktree-")
        .tempdir()
        .expect("tempdir");
    let repo = root.path().join("repo");
    fs::create_dir(&repo).expect("repo dir");
    git(&repo, &["init"]);
    fs::write(repo.join("README.md"), "hello\n").expect("readme");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    git(
        &repo,
        &["-c", "commit.gpgsign=false", "commit", "-m", "init"],
    );
    (root, repo)
}

fn unique_spec() -> String {
    format!("../worktree-{}", uuid::Uuid::new_v4())
}

#[test]
fn given_tmp_git_repo_when_create_worktree_then_registers_detached_worktree() {
    let (_root, repo) = init_git_repo();
    let spec = unique_spec();
    let expected = repo
        .parent()
        .expect("parent")
        .join(spec.trim_start_matches("../"));

    let result =
        create_worktree(&repo, "t1", "m1", &spec, &WorktreeConfig::default()).expect("create");

    assert_eq!(result, expected);
    assert!(fs::metadata(&expected).is_ok());
    let canonical = fs::canonicalize(&expected).expect("realpath");
    assert!(
        normalize_git_path_text(&git(&repo, &["worktree", "list"]))
            .contains(&normalize_git_path_text(&canonical.display().to_string()))
    );
    assert_eq!(
        git(&expected, &["rev-parse", "HEAD"]).trim(),
        git(&repo, &["rev-parse", "HEAD"]).trim()
    );
}

#[test]
fn validate_worktree_spec_rejects_bare_name() {
    let error = validate_worktree_spec("feature-x").expect_err("bare name");
    assert_eq!(
        error.to_string(),
        "worktreePath must be a filesystem path (relative './...', '../...' or absolute '/...')"
    );
}

#[test]
fn given_git_unavailable_when_create_worktree_then_throws_unavailable_error() {
    let (_root, repo) = init_git_repo();
    let runner = |args: &[String]| {
        if args[0] == "--version" {
            GitOutput {
                code: 1,
                stderr: "git missing".into(),
            }
        } else {
            GitOutput {
                code: 0,
                stderr: String::new(),
            }
        }
    };
    let error = create_worktree_with(
        &repo,
        "t1",
        "m1",
        &unique_spec(),
        &WorktreeConfig::default(),
        &runner,
    )
    .expect_err("git unavailable");
    assert!(matches!(error, TeamCoreError::GitUnavailable));
    assert_eq!(error.name(), "GitUnavailableError");
}

#[test]
fn given_created_worktree_when_remove_worktree_then_directory_disappears() {
    let (_root, repo) = init_git_repo();
    let path = create_worktree(
        &repo,
        "t1",
        "m1",
        &unique_spec(),
        &WorktreeConfig::default(),
    )
    .expect("create");
    remove_worktree(&path).expect("remove");
    assert!(fs::metadata(&path).is_err());
}

#[test]
fn given_runtime_mismatch_when_find_orphan_worktrees_then_returns_orphan_paths() {
    let base = tempfile::Builder::new()
        .prefix("team-worktree-orphans-")
        .tempdir()
        .expect("tempdir");
    fs::create_dir_all(base.path().join("worktrees/t1/m1")).expect("worktree");
    fs::create_dir_all(base.path().join("runtime/t1")).expect("runtime");
    fs::write(
        base.path().join("runtime/t1/state.json"),
        r#"{"status":"deleted"}"#,
    )
    .expect("state");
    assert_eq!(
        find_orphan_worktrees(base.path(), &WorktreeConfig::default()),
        vec![base.path().join("worktrees").join("t1").join("m1")]
    );
}
