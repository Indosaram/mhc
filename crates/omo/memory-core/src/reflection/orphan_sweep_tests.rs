use super::*;
use crate::git::{GitMemoryRepo, InitializeGitRepoOptions};
use crate::git::exec::{GitExecOptions, GitExecResult};
use tempfile::tempdir;

fn owner(name: &str) -> Option<(String, Option<f64>)> {
    parse_reflection_owner(name).map(|owner| (owner.run_id, owner.epoch))
}

#[test]
fn owners_parse_from_directories_branches_and_legacy_branches() {
    assert_eq!(
        owner("1700000000000-run-7"),
        Some(("run-7".to_string(), Some(1_700_000_000_000.0)))
    );
    assert_eq!(
        owner("memory/reflection-1700000000000-run-7"),
        Some(("run-7".to_string(), Some(1_700_000_000_000.0)))
    );
    assert_eq!(
        owner("reflection/run-7"),
        Some(("run-7".to_string(), None))
    );
    assert_eq!(owner("writer.lock"), None);
    assert_eq!(owner("1700000000000"), None);
}

#[test]
fn a_live_run_and_an_in_grace_worktree_are_never_selected() {
    let leftovers = ReflectionLeftovers {
        registered_worktrees: vec![
            RegisteredReflectionWorktree {
                dir: PathBuf::from("/root/1700000000000-live"),
                branch: Some("memory/reflection-1700000000000-live".to_string()),
                present: true,
            },
            RegisteredReflectionWorktree {
                dir: PathBuf::from("/root/1700000000000-fresh"),
                branch: None,
                present: true,
            },
            RegisteredReflectionWorktree {
                dir: PathBuf::from("/root/1600000000000-stale"),
                branch: None,
                present: true,
            },
        ],
        stray_dirs: Vec::new(),
        branches: Vec::new(),
    };
    let mut live = BTreeSet::new();
    live.insert("live".to_string());
    let selection = ReflectionOrphanSelection {
        live_run_ids: live,
        now_ms: 1_700_000_000_000.0,
        grace_ms: None,
    };

    let selected = select_reflection_orphans(&leftovers, &selection);

    assert_eq!(selected.registered_worktrees.len(), 1);
    assert_eq!(
        selected.registered_worktrees[0].dir,
        PathBuf::from("/root/1600000000000-stale")
    );
}

#[test]
fn a_registered_worktree_whose_directory_is_gone_is_always_selected() {
    let leftovers = ReflectionLeftovers {
        registered_worktrees: vec![RegisteredReflectionWorktree {
            dir: PathBuf::from("/root/1700000000000-vanished"),
            branch: None,
            present: false,
        }],
        stray_dirs: Vec::new(),
        branches: Vec::new(),
    };
    let selection = ReflectionOrphanSelection {
        live_run_ids: BTreeSet::new(),
        now_ms: 1_700_000_000_000.0,
        grace_ms: None,
    };

    let selected = select_reflection_orphans(&leftovers, &selection);
    assert_eq!(selected.registered_worktrees.len(), 1);
}

#[test]
fn a_legacy_branch_gets_no_grace_but_stays_protected_while_live() {
    let leftovers = ReflectionLeftovers {
        registered_worktrees: Vec::new(),
        stray_dirs: Vec::new(),
        branches: vec!["reflection/run-9".to_string()],
    };
    let now = 1_700_000_000_000.0;

    let selected = select_reflection_orphans(
        &leftovers,
        &ReflectionOrphanSelection {
            live_run_ids: BTreeSet::new(),
            now_ms: now,
            grace_ms: None,
        },
    );
    assert_eq!(selected.branches, vec!["reflection/run-9".to_string()]);

    let mut live = BTreeSet::new();
    live.insert("run-9".to_string());
    let protected = select_reflection_orphans(
        &leftovers,
        &ReflectionOrphanSelection {
            live_run_ids: live,
            now_ms: now,
            grace_ms: None,
        },
    );
    assert!(protected.branches.is_empty());

    let mut legacy_live = BTreeSet::new();
    legacy_live.insert("reflection-run-9".to_string());
    let also_protected = select_reflection_orphans(
        &leftovers,
        &ReflectionOrphanSelection {
            live_run_ids: legacy_live,
            now_ms: now,
            grace_ms: None,
        },
    );
    assert!(also_protected.branches.is_empty());
}

#[test]
fn registered_worktrees_parse_from_porcelain_output() {
    let porcelain = "worktree /root/1700000000000-run-1\nHEAD abc\nbranch refs/heads/memory/reflection-1700000000000-run-1\n\nworktree /elsewhere/other\nHEAD def\nbranch refs/heads/main\n";
    let parsed = parse_registered_worktrees(porcelain);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].branch.as_deref(), Some("memory/reflection-1700000000000-run-1"));
    assert_eq!(parsed[1].branch.as_deref(), Some("main"));
}

#[test]
fn sweep_reclaims_a_stale_worktree_directory_and_branch() {
    let dir = tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(dir.path().join("repo"), "orphan-agent").expect("repo");
    repo.init(InitializeGitRepoOptions::default()).expect("init");
    let worktrees = dir.path().join("worktrees");
    std::fs::create_dir_all(&worktrees).expect("mkdir");

    let exec = repo.exec();
    let worktree = super::super::worktree::create_reflection_worktree(
        &repo,
        "run-1",
        &worktrees,
        exec.as_ref(),
        None,
    )
    .expect("create worktree");
    assert!(worktree.dir.exists());

    let receipts = sweep_reflection_orphans(
        &repo,
        &worktrees,
        &ReflectionOrphanSweepOptions {
            live_run_ids: BTreeSet::new(),
            now_ms: crate::support::time::now_millis() as f64 + REFLECTION_ORPHAN_GRACE_MS + 1.0,
            grace_ms: None,
            exec: Some(exec.as_ref()),
        },
    )
    .expect("sweep");

    assert!(!receipts.is_empty());
    assert!(receipts.iter().all(|receipt| receipt.removed));
    assert!(!worktree.dir.exists());
    let branch = exec
        .run_in(&repo.dir, &["show-ref", "--verify", &format!("refs/heads/{}", worktree.branch)])
        .expect("show-ref");
    assert_ne!(branch.code, 0);
}

#[test]
fn sweep_leaves_a_live_run_untouched() {
    let dir = tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(dir.path().join("repo"), "orphan-agent").expect("repo");
    repo.init(InitializeGitRepoOptions::default()).expect("init");
    let worktrees = dir.path().join("worktrees");
    std::fs::create_dir_all(&worktrees).expect("mkdir");

    let exec = repo.exec();
    let worktree = super::super::worktree::create_reflection_worktree(
        &repo,
        "run-live",
        &worktrees,
        exec.as_ref(),
        None,
    )
    .expect("create worktree");
    let mut live = BTreeSet::new();
    live.insert("run-live".to_string());

    let receipts = sweep_reflection_orphans(
        &repo,
        &worktrees,
        &ReflectionOrphanSweepOptions {
            live_run_ids: live,
            now_ms: crate::support::time::now_millis() as f64 + REFLECTION_ORPHAN_GRACE_MS + 1.0,
            grace_ms: None,
            exec: Some(exec.as_ref()),
        },
    )
    .expect("sweep");

    assert!(receipts.is_empty());
    assert!(worktree.dir.exists());
    let _ = super::super::worktree::discard_reflection_worktree(
        &repo,
        &worktree.dir,
        &worktree.branch,
        exec.as_ref(),
    );
}

#[test]
fn a_stray_directory_is_reclaimed_and_reported() {
    let dir = tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(dir.path().join("repo"), "orphan-agent").expect("repo");
    repo.init(InitializeGitRepoOptions::default()).expect("init");
    let worktrees = dir.path().join("worktrees");
    let stray = worktrees.join("1600000000000-stray");
    std::fs::create_dir_all(&stray).expect("mkdir stray");

    let exec = repo.exec();
    let receipts = sweep_reflection_orphans(
        &repo,
        &worktrees,
        &ReflectionOrphanSweepOptions {
            live_run_ids: BTreeSet::new(),
            now_ms: 1_700_000_000_000.0,
            grace_ms: None,
            exec: Some(exec.as_ref()),
        },
    )
    .expect("sweep");

    assert!(
        receipts
            .iter()
            .any(|receipt| receipt.kind == OrphanKind::Directory && receipt.removed)
    );
    assert!(!stray.exists());
}

#[test]
fn a_non_epoch_directory_is_never_touched() {
    assert!(!is_epoch_prefixed("worktrees"));
    assert!(!is_epoch_prefixed("run-1"));
    assert!(is_epoch_prefixed("1700000000000-run-1"));
}

struct RefusingBranchDelete<'a> {
    inner: &'a dyn GitExec,
    branch: String,
}

impl GitExec for RefusingBranchDelete<'_> {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        if argv.first().map(String::as_str) == Some("branch")
            && argv.iter().any(|arg| arg == "-D")
            && argv.iter().any(|arg| arg == &self.branch)
        {
            return Ok(GitExecResult {
                code: 1,
                stdout: String::new(),
                stderr: "refusing to delete".to_owned(),
            });
        }
        self.inner.run(argv, options)
    }
}

#[test]
fn a_failing_item_reports_removed_false_with_detail_and_the_rest_still_sweep() {
    let dir = tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(dir.path().join("repo"), "orphan-agent").expect("repo");
    repo.init(InitializeGitRepoOptions::default()).expect("init");
    let worktrees = dir.path().join("worktrees");
    std::fs::create_dir_all(&worktrees).expect("mkdir");
    let exec = repo.exec();
    let head = repo.head().expect("head").expect("seeded head");
    let refused_branch = "memory/reflection-1600000000000-run-refused".to_owned();
    assert_eq!(exec.run_in(&repo.dir, &["branch", &refused_branch, &head]).expect("branch").code, 0);
    let reclaimable = super::super::worktree::create_reflection_worktree(
        &repo, "run-gone", &worktrees, exec.as_ref(), None,
    ).expect("create worktree");
    let scripted = RefusingBranchDelete { inner: exec.as_ref(), branch: refused_branch.clone() };
    let receipts = sweep_reflection_orphans(&repo, &worktrees, &ReflectionOrphanSweepOptions {
        live_run_ids: BTreeSet::new(),
        now_ms: 4_000_000_000_000.0,
        grace_ms: None,
        exec: Some(&scripted),
    }).expect("sweep");
    let refused = receipts.iter().find(|receipt|
        receipt.kind == OrphanKind::Branch && receipt.target == refused_branch,
    ).expect("refused branch receipt");
    assert!(!refused.removed);
    assert_eq!(refused.detail.as_deref(), Some("refusing to delete"));
    let reclaimed = receipts.iter().find(|receipt|
        receipt.kind == OrphanKind::Worktree && receipt.target == reclaimable.dir.to_string_lossy(),
    ).expect("reclaimed worktree receipt");
    assert!(reclaimed.removed);
    assert!(!reclaimable.dir.exists());
    assert!(!receipts.iter().any(|receipt|
        receipt.kind == OrphanKind::Branch && receipt.target == reclaimable.branch,
    ));
    assert_eq!(exec.run_in(&repo.dir, &["branch", "-D", &refused_branch]).expect("cleanup branch").code, 0);
}
