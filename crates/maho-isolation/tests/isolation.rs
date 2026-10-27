//! The isolation lifecycle against real temporary git repositories and plain directories.
use maho_isolation::{BackendChoice, IsolationSettings, MergeMode, TaskIsolation};
use maho_task::isolation::WorkspaceIsolation;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "T"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").expect("write");
    std::fs::write(dir.path().join(".gitignore"), "target/\n").expect("write");
    std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
    std::fs::write(dir.path().join("sub/b.txt"), "b\n").expect("write");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

fn backend(root: &Path, settings: IsolationSettings) -> TaskIsolation {
    TaskIsolation::with_settings(root, Arc::new(move |_| settings.clone()))
}

fn worktrees(dir: &Path) -> usize {
    git(dir, &["worktree", "list", "--porcelain"])
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn worktree_mirrors_live_state_and_patch_applies_back() {
    let repo = repo();
    let wt = tempfile::tempdir().expect("wt");
    // Parent WIP: staged, unstaged and untracked (and an ignored file that is not carried).
    std::fs::write(repo.path().join("a.txt"), "one\ntwo\nstaged\n").expect("write");
    git(repo.path(), &["add", "a.txt"]);
    std::fs::write(repo.path().join("sub/b.txt"), "b\nunstaged\n").expect("write");
    std::fs::write(repo.path().join("new.txt"), "untracked\n").expect("write");
    std::fs::create_dir(repo.path().join("target")).expect("mkdir");
    std::fs::write(repo.path().join("target/out"), "ignored\n").expect("write");

    let iso = backend(wt.path(), IsolationSettings::default());
    let ws = iso
        .prepare("Fix1", &repo.path().join("sub"))
        .await
        .expect("prepare");
    assert!(ws.cwd.ends_with("m/sub"), "{}", ws.cwd.display());
    let merged = ws.cwd.parent().expect("m").to_path_buf();
    assert_eq!(
        std::fs::read_to_string(merged.join("a.txt")).expect("read"),
        "one\ntwo\nstaged\n"
    );
    assert_eq!(
        std::fs::read_to_string(merged.join("sub/b.txt")).expect("read"),
        "b\nunstaged\n"
    );
    assert_eq!(
        std::fs::read_to_string(merged.join("new.txt")).expect("read"),
        "untracked\n"
    );
    assert!(!merged.join("target/out").exists());
    assert_eq!(git(&merged, &["diff", "--cached", "--name-only"]), "a.txt");
    assert_eq!(worktrees(repo.path()), 2);

    // The child edits, adds, deletes and even commits inside the isolation.
    std::fs::write(ws.cwd.join("b.txt"), "b\nunstaged\nchild\n").expect("write");
    std::fs::write(merged.join("added.txt"), "added\n").expect("write");
    std::fs::remove_file(merged.join("new.txt")).expect("rm");
    git(&merged, &["add", "-A"]);
    git(&merged, &["commit", "-q", "-m", "child commit"]);
    std::fs::write(merged.join("later.txt"), "after commit\n").expect("write");

    // The parent moves on meanwhile; that drift is not the task's work.
    std::fs::write(repo.path().join("unrelated.txt"), "parent\n").expect("write");

    let note = iso.finish(&ws, true).await.expect("finish").expect("note");
    assert!(
        note.starts_with("Isolation (worktree): applied 4 changed files"),
        "{note}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("sub/b.txt")).expect("read"),
        "b\nunstaged\nchild\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("added.txt")).expect("read"),
        "added\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("later.txt")).expect("read"),
        "after commit\n"
    );
    assert!(!repo.path().join("new.txt").exists());
    assert!(repo.path().join("unrelated.txt").exists());
    // Parent HEAD and index unchanged; the isolation is gone and unregistered.
    assert_eq!(git(repo.path(), &["log", "--format=%s"]), "init");
    assert_eq!(
        git(repo.path(), &["diff", "--cached", "--name-only"]),
        "a.txt"
    );
    assert!(!merged.exists());
    assert_eq!(worktrees(repo.path()), 1);
    let patches: Vec<_> = std::fs::read_dir(wt.path().join("patches"))
        .expect("patches")
        .collect();
    assert_eq!(patches.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn failure_discards_and_no_changes_is_reported() {
    let repo = repo();
    let wt = tempfile::tempdir().expect("wt");
    let iso = backend(wt.path(), IsolationSettings::default());
    let ws = iso.prepare("Fail", repo.path()).await.expect("prepare");
    std::fs::write(ws.cwd.join("a.txt"), "garbage\n").expect("write");
    let note = iso.finish(&ws, false).await.expect("finish").expect("note");
    assert!(
        note.contains("did not succeed; its isolated changes were discarded"),
        "{note}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).expect("read"),
        "one\ntwo\n"
    );
    assert!(!ws.cwd.exists());

    let ws = iso.prepare("Quiet", repo.path()).await.expect("prepare");
    let note = iso.finish(&ws, true).await.expect("finish").expect("note");
    assert!(note.ends_with("no changes."), "{note}");
    assert_eq!(worktrees(repo.path()), 1);
    assert!(iso.finish(&ws, true).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_conflict_and_already_present() {
    let repo = repo();
    let wt = tempfile::tempdir().expect("wt");
    let keep = backend(
        wt.path(),
        IsolationSettings {
            apply: false,
            ..IsolationSettings::default()
        },
    );
    let ws = keep.prepare("Keep", repo.path()).await.expect("prepare");
    std::fs::write(ws.cwd.join("a.txt"), "one\nTWO\n").expect("write");
    let note = keep.finish(&ws, true).await.expect("finish").expect("note");
    assert!(note.contains("1 file changed; not applied"), "{note}");
    let patch_path = note.rsplit("Patch: ").next().expect("path").trim();
    let patch = std::fs::read_to_string(patch_path).expect("patch");
    assert!(patch.contains("-two\n+TWO"));
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).expect("read"),
        "one\ntwo\n"
    );

    // Same change already made in the parent: nothing to apply.
    let apply = backend(wt.path(), IsolationSettings::default());
    let ws = apply.prepare("Same", repo.path()).await.expect("prepare");
    std::fs::write(ws.cwd.join("a.txt"), "one\nTWO\n").expect("write");
    std::fs::write(repo.path().join("a.txt"), "one\nTWO\n").expect("write");
    let note = apply
        .finish(&ws, true)
        .await
        .expect("finish")
        .expect("note");
    assert!(note.contains("already present"), "{note}");

    // A conflicting parent edit: not applied, patch kept.
    let ws = apply.prepare("Clash", repo.path()).await.expect("prepare");
    std::fs::write(ws.cwd.join("a.txt"), "one\nchild\n").expect("write");
    std::fs::write(repo.path().join("a.txt"), "one\nparent\n").expect("write");
    let note = apply
        .finish(&ws, true)
        .await
        .expect("finish")
        .expect("note");
    assert!(note.contains("do not apply cleanly"), "{note}");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).expect("read"),
        "one\nparent\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn branch_mode_commits_and_cherry_picks() {
    let repo = repo();
    let wt = tempfile::tempdir().expect("wt");
    let iso = backend(
        wt.path(),
        IsolationSettings {
            merge: MergeMode::Branch,
            ..IsolationSettings::default()
        },
    );
    let ws = iso.prepare("Feature", repo.path()).await.expect("prepare");
    std::fs::write(ws.cwd.join("feature.txt"), "f\n").expect("write");
    let note = iso.finish(&ws, true).await.expect("finish").expect("note");
    assert!(note.contains("cherry-picked"), "{note}");
    assert_eq!(
        git(repo.path(), &["log", "-1", "--format=%s"]),
        "Changes from isolated task Feature"
    );
    assert_eq!(
        git(repo.path(), &["show", "--name-only", "--format=", "HEAD"]),
        "feature.txt"
    );
    assert_eq!(git(repo.path(), &["branch", "--list", "mhc/task/*"]), "");

    // apply off: the branch stays, the parent does not move.
    let keep = backend(
        wt.path(),
        IsolationSettings {
            merge: MergeMode::Branch,
            apply: false,
            ..IsolationSettings::default()
        },
    );
    let ws = keep.prepare("Later", repo.path()).await.expect("prepare");
    std::fs::write(ws.cwd.join("later.txt"), "l\n").expect("write");
    let note = keep.finish(&ws, true).await.expect("finish").expect("note");
    assert!(note.contains("branch `mhc/task/Later`"), "{note}");
    assert_eq!(
        git(
            repo.path(),
            &["show", "--name-only", "--format=", "mhc/task/Later"]
        ),
        "later.txt"
    );
    assert!(!repo.path().join("later.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_directory_copy_and_write_back() {
    let src = tempfile::tempdir().expect("src");
    let wt = tempfile::tempdir().expect("wt");
    std::fs::write(src.path().join("keep.txt"), "k\n").expect("write");
    std::fs::write(src.path().join("edit.txt"), "old\n").expect("write");
    std::fs::write(src.path().join("gone.txt"), "g\n").expect("write");
    let iso = backend(wt.path(), IsolationSettings::default());
    let ws = iso.prepare("Plain", src.path()).await.expect("prepare");
    std::fs::write(ws.cwd.join("edit.txt"), "new\n").expect("write");
    std::fs::write(ws.cwd.join("made.txt"), "m\n").expect("write");
    std::fs::remove_file(ws.cwd.join("gone.txt")).expect("rm");
    let note = iso.finish(&ws, true).await.expect("finish").expect("note");
    assert!(
        note.starts_with("Isolation (copy): applied 3 changed files"),
        "{note}"
    );
    assert_eq!(
        std::fs::read_to_string(src.path().join("edit.txt")).expect("read"),
        "new\n"
    );
    assert_eq!(
        std::fs::read_to_string(src.path().join("made.txt")).expect("read"),
        "m\n"
    );
    assert!(!src.path().join("gone.txt").exists());
    assert!(!ws.cwd.exists());

    // A parent edit made meanwhile blocks the write-back and keeps the copy.
    let ws = iso.prepare("Plain2", src.path()).await.expect("prepare");
    std::fs::write(ws.cwd.join("keep.txt"), "child\n").expect("write");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(src.path().join("keep.txt"), "parent edit\n").expect("write");
    let note = iso.finish(&ws, true).await.expect("finish").expect("note");
    assert!(note.contains("changed meanwhile (keep.txt)"), "{note}");
    assert!(note.contains(".retained-"), "{note}");
    assert_eq!(
        std::fs::read_to_string(src.path().join("keep.txt")).expect("read"),
        "parent edit\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_backend_on_git_and_fallback_note_and_refusals() {
    let repo = repo();
    let wt = tempfile::tempdir().expect("wt");
    let copy = backend(
        wt.path(),
        IsolationSettings {
            backend: BackendChoice::Copy,
            ..IsolationSettings::default()
        },
    );
    let ws = copy.prepare("Copy", repo.path()).await.expect("prepare");
    assert!(ws.cwd.join(".git").is_dir());
    std::fs::write(ws.cwd.join("a.txt"), "one\ntwo\nthree\n").expect("write");
    let note = copy.finish(&ws, true).await.expect("finish").expect("note");
    assert!(
        note.starts_with("Isolation (copy): applied 1 changed file"),
        "{note}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).expect("read"),
        "one\ntwo\nthree\n"
    );
    assert_eq!(worktrees(repo.path()), 1);

    let cow = backend(
        wt.path(),
        IsolationSettings {
            backend: BackendChoice::Unsupported("btrfs".into()),
            ..IsolationSettings::default()
        },
    );
    let ws = cow.prepare("Cow", repo.path()).await.expect("prepare");
    let note = cow.finish(&ws, true).await.expect("finish").expect("note");
    assert!(
        note.starts_with("Isolation (worktree), fell back: backend `btrfs` is not available"),
        "{note}"
    );

    // The isolation root may not live inside the workspace.
    let inside = backend(&repo.path().join(".wt"), IsolationSettings::default());
    let error = inside.prepare("In", repo.path()).await.expect_err("inside");
    assert!(error.contains("inside"), "{error}");
}
