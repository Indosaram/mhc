//! Reclamation of reflection leftovers a killed or crashed run left behind.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::fs;
use crate::git::GitMemoryRepo;
use crate::git::exec::GitExec;

use super::worktree::discard_reflection_worktree;

const REFLECTION_BRANCH_PREFIX: &str = "memory/reflection-";
const LEGACY_BRANCH_PREFIX: &str = "reflection/run-";

/// Covers the window between a prelaunch directory and its ledger, plus clock skew.
pub const REFLECTION_ORPHAN_GRACE_MS: f64 = 15.0 * 60_000.0;

/// A worktree git still registers under the reflection worktrees root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredReflectionWorktree {
    pub dir: PathBuf,
    pub branch: Option<String>,
    pub present: bool,
}

/// Everything a reflection run may have left behind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflectionLeftovers {
    pub registered_worktrees: Vec<RegisteredReflectionWorktree>,
    pub stray_dirs: Vec<PathBuf>,
    pub branches: Vec<String>,
}

/// Ownership inputs for the orphan decision.
#[derive(Debug, Clone, Default)]
pub struct ReflectionOrphanSelection {
    pub live_run_ids: BTreeSet<String>,
    pub now_ms: f64,
    pub grace_ms: Option<f64>,
}

/// Sweep inputs: the ownership decision plus the git runner.
pub struct ReflectionOrphanSweepOptions<'a> {
    pub live_run_ids: BTreeSet<String>,
    pub now_ms: f64,
    pub grace_ms: Option<f64>,
    pub exec: Option<&'a dyn GitExec>,
}

/// One attempt's outcome; a failing item never aborts the sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectionOrphanReceipt {
    pub kind: OrphanKind,
    pub target: String,
    pub removed: bool,
    pub detail: Option<String>,
}

/// Which leftover surface a receipt describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanKind {
    Worktree,
    Directory,
    Branch,
}

/// Lists the registered worktrees, stray run directories and reflection branches.
pub fn list_reflection_leftovers(
    repo: &GitMemoryRepo,
    worktrees_dir: &Path,
    exec: &dyn GitExec,
) -> Result<ReflectionLeftovers, String> {
    let root = resolve(worktrees_dir);
    let listed = git(exec, &repo.dir, &["worktree", "list", "--porcelain"])?;
    let registered_worktrees: Vec<RegisteredReflectionWorktree> = parse_registered_worktrees(&listed)
        .into_iter()
        .filter(|worktree| is_inside(&root, &worktree.dir))
        .collect();
    let registered: BTreeSet<PathBuf> = registered_worktrees
        .iter()
        .map(|worktree| worktree.dir.clone())
        .collect();

    let stray_dirs = fs::read_dir_directories(&root)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|name| is_epoch_prefixed(name))
        .map(|name| root.join(name))
        .filter(|dir| !registered.contains(dir))
        .collect();

    let refs = git(
        exec,
        &repo.dir,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            &format!("refs/heads/{REFLECTION_BRANCH_PREFIX}*"),
            &format!("refs/heads/{LEGACY_BRANCH_PREFIX}*"),
        ],
    )?;
    let branches = refs
        .lines()
        .map(str::trim)
        .filter(|line| is_reflection_branch(line))
        .map(str::to_string)
        .collect();

    Ok(ReflectionLeftovers {
        registered_worktrees,
        stray_dirs,
        branches,
    })
}

/// Pure ownership decision over already-collected leftovers.
pub fn select_reflection_orphans(
    leftovers: &ReflectionLeftovers,
    selection: &ReflectionOrphanSelection,
) -> ReflectionLeftovers {
    let grace_ms = selection.grace_ms.unwrap_or(REFLECTION_ORPHAN_GRACE_MS);
    let is_orphan = |name: &str, present: bool| -> bool {
        let Some(owner) = parse_reflection_owner(name) else {
            return false;
        };
        if is_live(&selection.live_run_ids, &owner.run_id) {
            return false;
        }
        if !present {
            return true;
        }
        owner.epoch.is_none_or(|epoch| epoch <= selection.now_ms - grace_ms)
    };
    ReflectionLeftovers {
        registered_worktrees: leftovers
            .registered_worktrees
            .iter()
            .filter(|worktree| {
                is_orphan(&basename(&worktree.dir), worktree.present)
            })
            .cloned()
            .collect(),
        stray_dirs: leftovers
            .stray_dirs
            .iter()
            .filter(|dir| is_orphan(&basename(dir), true))
            .cloned()
            .collect(),
        branches: leftovers
            .branches
            .iter()
            .filter(|branch| is_orphan(branch, true))
            .cloned()
            .collect(),
    }
}

/// Reclaims every selected orphan, reporting one receipt per attempt.
pub fn sweep_reflection_orphans(
    repo: &GitMemoryRepo,
    worktrees_dir: &Path,
    options: &ReflectionOrphanSweepOptions<'_>,
) -> Result<Vec<ReflectionOrphanReceipt>, String> {
    let owned_exec;
    let exec: &dyn GitExec = match options.exec {
        Some(exec) => exec,
        None => {
            owned_exec = repo.exec();
            owned_exec.as_ref()
        }
    };
    let root = resolve(worktrees_dir);
    let selection = ReflectionOrphanSelection {
        live_run_ids: options.live_run_ids.clone(),
        now_ms: options.now_ms,
        grace_ms: options.grace_ms,
    };
    let orphans = select_reflection_orphans(&list_reflection_leftovers(repo, &root, exec)?, &selection);
    let mut receipts = Vec::new();
    let mut discarded: BTreeSet<String> = BTreeSet::new();

    for worktree in &orphans.registered_worktrees {
        let branch = worktree
            .branch
            .clone()
            .unwrap_or_else(|| format!("{REFLECTION_BRANCH_PREFIX}{}", basename(&worktree.dir)));
        discarded.insert(branch.clone());
        let receipt = attempt(OrphanKind::Worktree, &worktree.dir, || {
            let cleanup = discard_reflection_worktree(repo, &worktree.dir, &branch, exec);
            Ok(cleanup.worktree_removed && cleanup.branch_removed)
        });
        receipts.push(receipt);
    }

    for dir in &orphans.stray_dirs {
        let receipt = attempt(OrphanKind::Directory, dir, || {
            if !is_inside(&root, &resolve(dir)) {
                return Err(format!(
                    "Refusing to remove {} outside {}",
                    dir.display(),
                    root.display()
                ));
            }
            fs::remove_dir_all(dir).map_err(|error| error.to_string())?;
            Ok(!fs::exists(dir))
        });
        receipts.push(receipt);
    }

    if !orphans.stray_dirs.is_empty() {
        let _ = git(exec, &repo.dir, &["worktree", "prune"]);
    }

    for branch in &orphans.branches {
        if discarded.contains(branch) {
            continue;
        }
        let receipt = attempt(OrphanKind::Branch, Path::new(branch), || {
            git(exec, &repo.dir, &["branch", "-D", branch])?;
            let still_there = exec
                .run_in(
                    &repo.dir,
                    &["show-ref", "--verify", &format!("refs/heads/{branch}")],
                )
                .map(|result| result.code == 0)
                .unwrap_or(false);
            Ok(!still_there)
        });
        receipts.push(receipt);
    }

    Ok(receipts)
}

struct ReflectionOwner {
    run_id: String,
    epoch: Option<f64>,
}

/// `<epoch>-<runId>` directories and `memory/reflection-<epoch>-<runId>` branches carry their own age.
fn parse_reflection_owner(name: &str) -> Option<ReflectionOwner> {
    if name.starts_with(LEGACY_BRANCH_PREFIX) {
        return Some(ReflectionOwner {
            run_id: name["reflection/".len()..].to_string(),
            epoch: None,
        });
    }
    let suffix = name
        .strip_prefix(REFLECTION_BRANCH_PREFIX)
        .unwrap_or(name);
    let (epoch, run_id) = suffix.split_once('-')?;
    if epoch.is_empty() || run_id.is_empty() || !epoch.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(ReflectionOwner {
        run_id: run_id.to_string(),
        epoch: epoch.parse::<f64>().ok(),
    })
}

/// Legacy branches were recorded as both `run-<n>` and `reflection-run-<n>`; either claim protects.
fn is_live(live_run_ids: &BTreeSet<String>, run_id: &str) -> bool {
    live_run_ids.contains(run_id) || live_run_ids.contains(&format!("reflection-{run_id}"))
}

fn is_reflection_branch(branch: &str) -> bool {
    branch.starts_with(REFLECTION_BRANCH_PREFIX) || branch.starts_with(LEGACY_BRANCH_PREFIX)
}

fn parse_registered_worktrees(porcelain: &str) -> Vec<RegisteredReflectionWorktree> {
    let mut worktrees = Vec::new();
    let mut dir: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    let mut flush = |dir: &mut Option<PathBuf>, branch: &mut Option<String>| {
        if let Some(path) = dir.take() {
            worktrees.push(RegisteredReflectionWorktree {
                present: fs::exists(&path),
                dir: path,
                branch: branch.take(),
            });
        }
        *branch = None;
    };
    for line in porcelain.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            flush(&mut dir, &mut branch);
            dir = Some(resolve(Path::new(rest.trim())));
        } else if let Some(rest) = line.strip_prefix("branch refs/heads/") {
            branch = Some(rest.trim().to_string());
        }
    }
    flush(&mut dir, &mut branch);
    worktrees
}

fn attempt(
    kind: OrphanKind,
    target: &Path,
    operation: impl FnOnce() -> Result<bool, String>,
) -> ReflectionOrphanReceipt {
    match operation() {
        Ok(removed) => ReflectionOrphanReceipt {
            kind,
            target: target.to_string_lossy().into_owned(),
            removed,
            detail: None,
        },
        Err(detail) => ReflectionOrphanReceipt {
            kind,
            target: target.to_string_lossy().into_owned(),
            removed: false,
            detail: Some(detail),
        },
    }
}

fn git(exec: &dyn GitExec, cwd: &Path, argv: &[&str]) -> Result<String, String> {
    let result = exec
        .run_in(cwd, argv)
        .map_err(|error| error.to_string())?;
    if result.code != 0 {
        let stderr = result.stderr.trim();
        return Err(if stderr.is_empty() {
            format!("git {} failed", argv.join(" "))
        } else {
            stderr.to_string()
        });
    }
    Ok(result.stdout)
}

fn resolve(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn basename(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn is_epoch_prefixed(name: &str) -> bool {
    match name.split_once('-') {
        Some((epoch, rest)) => !epoch.is_empty() && !rest.is_empty() && epoch.bytes().all(|byte| byte.is_ascii_digit()),
        None => false,
    }
}

fn is_inside(root: &Path, path: &Path) -> bool {
    match path.strip_prefix(root) {
        Ok(offset) => !offset.as_os_str().is_empty(),
        Err(_) => false,
    }
}

#[cfg(test)]
#[path = "orphan_sweep_tests.rs"]
mod tests;
