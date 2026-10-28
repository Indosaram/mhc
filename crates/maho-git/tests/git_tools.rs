//! The git tools against real temporary repositories.
use maho_git::{Git, tools};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "T"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\nfn b() {}\n").expect("write");
    std::fs::write(dir.path().join("README.md"), "# x\n").expect("write");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "feat(core): add a"]);
    git(
        dir.path(),
        &["commit", "-q", "--allow-empty", "-m", "fix: Handle b"],
    );
    dir
}

fn text(result: &maho_ext_api::ToolResult) -> String {
    serde_json::to_value(&result.content[0])
        .ok()
        .and_then(|v| v.get("text").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default()
}

#[test]
fn overview_diff_hunk_and_commits() {
    let dir = repo();
    std::fs::write(dir.path().join("a.rs"), "fn a() { 1 }\nfn b() {}\n").expect("write");
    std::fs::write(dir.path().join("Cargo.lock"), "lock\n").expect("write");
    std::fs::write(dir.path().join("new.txt"), "n\n").expect("write");
    git(dir.path(), &["add", "a.rs", "Cargo.lock"]);

    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).expect("mkdir");
    let overview = tools::overview(&sub, &json!({})).expect("overview");
    let details = overview.details.clone().expect("details");
    assert_eq!(details["files"], json!(["a.rs"]));
    assert_eq!(details["excludedFiles"], json!(["Cargo.lock"]));
    assert_eq!(
        details["numstat"],
        json!([{"path": "a.rs", "additions": 1, "deletions": 1}])
    );
    assert!(
        details["stat"]
            .as_str()
            .unwrap_or_default()
            .contains("2 files changed")
    );
    let unstaged = tools::overview(
        dir.path(),
        &json!({"staged": false, "include_untracked": true}),
    )
    .expect("overview");
    assert_eq!(
        unstaged.details.expect("details")["untrackedFiles"],
        json!(["new.txt"])
    );

    let diff =
        tools::file_diff(dir.path(), &json!({"files": ["a.rs", "README.md"]})).expect("diff");
    let body = text(&diff);
    assert!(
        body.starts_with("=== a.rs ===\ndiff --git a/a.rs b/a.rs"),
        "{body}"
    );
    assert!(body.contains("+fn a() { 1 }"));
    assert!(!body.contains("README.md ==="));
    assert_eq!(
        text(&tools::file_diff(dir.path(), &json!({"files": ["README.md"]})).expect("diff")),
        "(no diff)"
    );
    assert!(tools::file_diff(dir.path(), &json!({"files": []})).is_err());

    let hunk = tools::hunk(dir.path(), &json!({"file": "a.rs", "hunks": [1]})).expect("hunk");
    assert!(text(&hunk).starts_with("@@ -1,2 +1,2 @@"));
    assert_eq!(
        text(&tools::hunk(dir.path(), &json!({"file": "a.rs", "hunks": [3]})).expect("hunk")),
        "(no matching hunks)"
    );

    let commits = tools::recent_commits(dir.path(), &json!({"count": 5})).expect("commits");
    let details = commits.details.expect("details");
    assert_eq!(
        details["commits"],
        json!(["fix: Handle b", "feat(core): add a"])
    );
    assert_eq!(details["stats"]["scopeUsagePercent"], json!(50));
    assert_eq!(details["stats"]["lowercaseSummaryPercent"], json!(50));
    assert_eq!(details["stats"]["topScopes"], json!({"core": 1}));
    assert_eq!(
        details["stats"]["commonVerbs"],
        json!({"add": 1, "handle": 1})
    );
}

#[test]
fn outside_a_repository() {
    let dir = tempfile::tempdir().expect("tempdir");
    let error = tools::overview(dir.path(), &json!({})).expect_err("not a repo");
    assert!(error.to_string().starts_with("not a git repository"));
    assert!(!Git::new(dir.path()).is_repository());
}

#[test]
fn snapshot_and_patched_trees() {
    let dir = repo();
    let repo = Git::new(dir.path());
    let scratch = tempfile::tempdir().expect("scratch");
    let base = repo.snapshot_tree(scratch.path()).expect("snapshot");
    std::fs::write(dir.path().join("a.rs"), "changed\n").expect("write");
    std::fs::write(dir.path().join("untracked.txt"), "u\n").expect("write");
    let after = repo.snapshot_tree(scratch.path()).expect("snapshot");
    assert_ne!(base, after);
    // The real index is untouched.
    assert!(repo.changed_files(true).expect("cached").is_empty());
    let patch = repo.diff_trees(&base, &after).expect("diff");
    let patched = repo
        .patched_tree(&base, &patch, scratch.path())
        .expect("patched");
    assert_eq!(patched, after);
}
