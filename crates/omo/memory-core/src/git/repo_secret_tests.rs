//! Port of upstream `packages/memory-core/src/git/repo-secret.test.ts` (omo v5.1.52): the
//! commit gate's secret screening on `commit_write` / `commit_prepared`, and index+worktree
//! rollback through `restore_paths`.

use pretty_assertions::assert_eq;
use std::fs;

use super::errors::GitError;
use super::repo::GitMemoryRepo;
use super::repo_types::{GitCommitAuthor, GitSeedFile, InitializeGitRepoOptions};
use super::secret::{MemorySecretError, MemorySecretLocation};
use crate::sync::redact::SecretPatternClass;
use crate::tools::agreements_write_guard::restore_paths;

fn author() -> GitCommitAuthor {
    GitCommitAuthor {
        agent_id: "agent-one".into(),
        author_name: "Memory Agent".into(),
        author_email: None,
    }
}

fn create_repo() -> (tempfile::TempDir, GitMemoryRepo) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(temp_dir.path(), "agent-one").expect("repo");
    (temp_dir, repo)
}

fn init(repo: &GitMemoryRepo) -> String {
    repo.init(Some(InitializeGitRepoOptions::default()))
        .expect("init")
}

fn init_seeded(repo: &GitMemoryRepo, path: &str, content: &str) -> String {
    repo.init(InitializeGitRepoOptions {
        author_name: None,
        seed_files: vec![GitSeedFile {
            relative_path: path.to_string(),
            content: content.to_string(),
        }],
        install_hooks: None,
    })
    .expect("init")
}

fn write(repo: &GitMemoryRepo, path: &str, content: &str) {
    let full = repo.dir.join(path);
    fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
    fs::write(full, content).expect("write");
}

fn git(repo: &GitMemoryRepo, args: &[&str]) {
    let result = repo.exec().run_in(&repo.dir, args).expect("git");
    assert_eq!(result.code, 0, "git {args:?} failed: {}", result.stderr);
}

fn head(repo: &GitMemoryRepo) -> String {
    repo.head().expect("head").expect("HEAD exists")
}

fn status(repo: &GitMemoryRepo) -> String {
    repo.status(&[] as &[&str]).expect("status")
}

fn tree(repo: &GitMemoryRepo) -> Vec<String> {
    repo.ls_tree(Some("HEAD"), None).expect("ls-tree")
}

fn expect_secret(result: Result<impl std::fmt::Debug, GitError>) -> (MemorySecretError, String) {
    match result {
        Err(error @ GitError::Secret(_)) => {
            let message = error.to_string();
            let GitError::Secret(secret) = error else {
                unreachable!()
            };
            (secret, message)
        }
        other => panic!("expected GitError::Secret, got {other:?}"),
    }
}

#[test]
fn given_a_file_carrying_an_aws_key_when_commit_write_runs_then_it_refuses_with_path_and_class_head_stays_and_the_file_survives()
 {
    // given
    let (_temp, repo) = create_repo();
    let initial = init(&repo);
    write(&repo, "reference/x.md", "the key is AKIAABCDEFGHIJKLMNOP\n");

    // when
    let (secret, message) =
        expect_secret(repo.commit_write(&["reference/x.md"], "remember key", &author()));

    // then
    assert_eq!(secret.path, "reference/x.md");
    assert_eq!(secret.pattern_class, SecretPatternClass::AwsAccessKey);
    assert_eq!(secret.location, MemorySecretLocation::Content);
    assert!(!message.contains("AKIAABCDEFGHIJKLMNOP"), "{message}");
    assert_eq!(head(&repo), initial);
    assert_eq!(
        fs::read_to_string(repo.dir.join("reference/x.md")).expect("read"),
        "the key is AKIAABCDEFGHIJKLMNOP\n"
    );
}

#[test]
fn given_a_secret_bearing_staged_blob_overwritten_in_the_worktree_when_commit_prepared_runs_then_it_refuses_because_the_index_is_scanned()
 {
    // given
    let (_temp, repo) = create_repo();
    let initial = init(&repo);
    write(&repo, "reference/x.md", "token=abc123456\n");
    git(&repo, &["add", "-A", "--", "reference/x.md"]);
    write(&repo, "reference/x.md", "clean body\n");

    // when
    let (secret, message) =
        expect_secret(repo.commit_prepared(&["reference/x.md"], "remember x", &author()));

    // then
    assert_eq!(
        secret.pattern_class,
        SecretPatternClass::CredentialAssignment
    );
    assert_eq!(secret.location, MemorySecretLocation::Content);
    assert_eq!(secret.path, "reference/x.md");
    assert!(!message.contains("abc123456"), "{message}");
    assert_eq!(head(&repo), initial);
}

#[test]
fn given_a_clean_staged_blob_with_a_secret_only_in_the_worktree_when_commit_prepared_runs_then_the_clean_index_commits()
 {
    // given
    let (_temp, repo) = create_repo();
    init(&repo);
    write(&repo, "reference/x.md", "clean body\n");
    git(&repo, &["add", "-A", "--", "reference/x.md"]);
    write(&repo, "reference/x.md", "token=abc123456\n");

    // when
    let result = repo
        .commit_prepared(&["reference/x.md"], "remember x", &author())
        .expect("commit");

    // then
    assert_eq!(head(&repo), result.sha);
    assert_eq!(
        repo.show("HEAD", "reference/x.md").expect("show"),
        "clean body\n"
    );
}

#[test]
fn given_a_staged_deletion_when_commit_prepared_runs_then_the_deletion_commits_without_reading_a_missing_blob()
 {
    // given
    let (_temp, repo) = create_repo();
    init_seeded(&repo, "reference/x.md", "x\n");
    git(&repo, &["rm", "-q", "reference/x.md"]);

    // when
    let result = repo
        .commit_prepared(&["reference/x.md"], "remove x", &author())
        .expect("commit");

    // then
    assert_eq!(head(&repo), result.sha);
    assert!(!tree(&repo).contains(&"reference/x.md".to_string()));
}

#[test]
fn given_a_staged_rename_when_commit_prepared_runs_then_the_rename_commits() {
    // given
    let (_temp, repo) = create_repo();
    init_seeded(&repo, "reference/old.md", "old body\n");
    git(&repo, &["mv", "reference/old.md", "reference/new.md"]);

    // when
    let result = repo
        .commit_prepared(
            &["reference/old.md", "reference/new.md"],
            "rename old",
            &author(),
        )
        .expect("commit");

    // then
    assert_eq!(head(&repo), result.sha);
    let paths = tree(&repo);
    assert!(paths.contains(&"reference/new.md".to_string()));
    assert!(!paths.contains(&"reference/old.md".to_string()));
}

#[test]
fn given_a_secret_like_file_name_with_a_clean_body_when_commit_write_runs_then_it_refuses_as_a_path_hit_and_the_names_digits_never_appear()
 {
    // given
    let (_temp, repo) = create_repo();
    let initial = init(&repo);
    write(&repo, "reference/token=abc123456.md", "clean body\n");

    // when
    let (secret, message) = expect_secret(repo.commit_write(
        &["reference/token=abc123456.md"],
        "remember name",
        &author(),
    ));

    // then
    assert_eq!(secret.location, MemorySecretLocation::Path);
    assert_eq!(
        secret.pattern_class,
        SecretPatternClass::CredentialAssignment
    );
    assert!(!message.contains("abc123456"), "{message}");
    assert_eq!(head(&repo), initial);
}

#[test]
fn given_a_clean_file_when_commit_write_runs_then_it_commits_as_before() {
    // given
    let (_temp, repo) = create_repo();
    init(&repo);
    write(&repo, "reference/ok.md", "perfectly ordinary note\n");

    // when
    let result = repo
        .commit_write(&["reference/ok.md"], "remember ok", &author())
        .expect("commit");

    // then
    assert_eq!(head(&repo), result.sha);
    assert_eq!(
        repo.show("HEAD", "reference/ok.md").expect("show"),
        "perfectly ordinary note\n"
    );
}

#[test]
fn given_a_refused_staging_of_a_new_secret_file_when_restore_paths_runs_then_index_and_worktree_both_return_to_the_pre_operation_state()
 {
    // given
    let (_temp, repo) = create_repo();
    let initial = init(&repo);
    write(&repo, "reference/new.md", "token=abc123456\n");
    expect_secret(repo.commit_write(&["reference/new.md"], "remember new", &author()));

    // when
    restore_paths(&repo, &["reference/new.md".to_string()]).expect("restore");

    // then
    assert_eq!(head(&repo), initial);
    assert_eq!(status(&repo).trim(), "");
    assert!(!repo.dir.join("reference/new.md").exists());
}

#[test]
fn given_a_refused_staging_over_a_tracked_clean_file_when_restore_paths_runs_then_the_tracked_content_is_back_in_index_and_worktree()
 {
    // given
    let (_temp, repo) = create_repo();
    init_seeded(&repo, "reference/x.md", "clean body\n");
    write(&repo, "reference/x.md", "token=abc123456\n");
    expect_secret(repo.commit_write(&["reference/x.md"], "remember x", &author()));

    // when
    restore_paths(&repo, &["reference/x.md".to_string()]).expect("restore");

    // then
    assert_eq!(status(&repo).trim(), "");
    assert_eq!(
        fs::read_to_string(repo.dir.join("reference/x.md"))
            .expect("read")
            .replace("\r\n", "\n"),
        "clean body\n"
    );
    assert_eq!(
        repo.show("HEAD", "reference/x.md").expect("show"),
        "clean body\n"
    );
}
