use super::*;
use std::{path::Path, process::Command};

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C").arg(dir).args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("unused-global-config"))
        .output().expect("git fixture command");
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).expect("git stdout")
}

#[test]
fn git_origin_binding_persists_space_across_remote_url_change() {
    // Given
    let dir = tempfile::tempdir().expect("isolated repo");
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["remote", "add", "origin", "git@github.com:a/b.git"]);
    let first = resolve_space_in_repo(dir.path(), None, None).expect("origin");
    assert_eq!(first.binding_key, "repo:github.com/a/b");
    assert_eq!(git(dir.path(), &["config", "--local", "--get", SYNC_SPACE_CONFIG_KEY]).trim(), first.space_id);
    git(dir.path(), &["remote", "set-url", "origin", "ssh://git@github.com/a/renamed.git"]);
    // When
    let second = resolve_space_in_repo(dir.path(), None, None).expect("renamed");
    // Then
    assert_eq!(second.space_id, first.space_id);
    assert_eq!(second.binding_key, "repo:github.com/a/renamed");
    assert!(second.alias_of_stored_space);
    assert_eq!(second.wrote, SpaceWrites::default());
}

#[test]
fn git_remote_less_repo_persists_local_binding_without_reminting() {
    // Given
    let dir = tempfile::tempdir().expect("isolated repo");
    git(dir.path(), &["init", "-q"]);
    let mints = std::cell::Cell::new(0);
    let random = || {
        mints.set(mints.get() + 1);
        "0".repeat(32)
    };
    let first = resolve_space_in_repo(dir.path(), None, Some(&random)).expect("local");
    // When
    let second = resolve_space_in_repo(dir.path(), None, Some(&random)).expect("repeat");
    // Then
    assert_eq!(first.binding_key, format!("local:{}", "0".repeat(32)));
    assert_eq!(git(dir.path(), &["config", "--local", "--get", SYNC_BINDING_CONFIG_KEY]).trim(), first.binding_key);
    assert_eq!(second.binding_key, first.binding_key);
    assert_eq!(second.space_id, first.space_id);
    assert_eq!(second.wrote, SpaceWrites::default());
    assert_eq!(mints.get(), 1);
}

#[test]
fn git_malformed_origin_falls_back_to_persisted_local_binding() {
    // Given
    let dir = tempfile::tempdir().expect("isolated repo");
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["remote", "add", "origin", "https://[bad"]);
    let random = || "a".repeat(32);
    let first = resolve_space_in_repo(dir.path(), None, Some(&random)).expect("fallback");
    // When: omit entropy on the second lookup so only persisted config can supply identity.
    let second = resolve_space_in_repo(dir.path(), None, None).expect("repeat");
    // Then
    assert_eq!(first.binding_key, format!("local:{}", "a".repeat(32)));
    assert_eq!(git(dir.path(), &["config", "--local", "--get", SYNC_BINDING_CONFIG_KEY]).trim(), first.binding_key);
    assert_eq!(second.binding_key, first.binding_key);
    assert_eq!(second.space_id, first.space_id);
    assert_eq!(second.wrote, SpaceWrites::default());
}
