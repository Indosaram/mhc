//! Reclaiming reflection leftovers a killed or crashed run left behind.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-orphan-sweep.md`. Three surfaces can be left over —
//! registered worktrees, stray run directories, and branches — and the decision for each is the same
//! question: does this belong to a run that is still alive, or to one that is young enough to still be
//! starting?
//!
//! The dangerous direction is reclaiming a live run's worktree, which deletes an in-progress reflection's
//! output. So a name that cannot be parsed as a reflection owner is **not** an orphan, and a run id that
//! is live protects every surface spelled with it.

use std::collections::BTreeSet;
use std::path::PathBuf;

pub const REFLECTION_BRANCH_PREFIX: &str = "memory/reflection-";

pub const LEGACY_BRANCH_PREFIX: &str = "reflection/run-";

/// Covers the window between a prelaunch directory appearing and its ledger recording the run, plus
/// clock skew between the process that named the directory and the one sweeping.
pub const REFLECTION_ORPHAN_GRACE_MS: f64 = 15.0 * 60_000.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredReflectionWorktree {
    pub dir: PathBuf,
    pub branch: Option<String>,
    /// Whether the directory is still on disk. A registration whose directory is already gone is
    /// reclaimable without waiting for the grace window: it cannot belong to a run that is writing.
    pub present: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflectionLeftovers {
    pub registered_worktrees: Vec<RegisteredReflectionWorktree>,
    pub stray_dirs: Vec<PathBuf>,
    pub branches: Vec<String>,
}

impl ReflectionLeftovers {
    pub fn is_empty(&self) -> bool {
        self.registered_worktrees.is_empty()
            && self.stray_dirs.is_empty()
            && self.branches.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReflectionOrphanSelection {
    pub live_run_ids: BTreeSet<String>,
    pub now_ms: f64,
    pub grace_ms: Option<f64>,
}

/// One reclaimed item. A failing item never aborts the sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectionOrphanReceipt {
    pub kind: OrphanKind,
    pub target: String,
    pub removed: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanKind {
    Worktree,
    Directory,
    Branch,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReflectionOwner {
    pub run_id: String,
    pub epoch: Option<f64>,
}

pub fn parse_reflection_owner(name: &str) -> Option<ReflectionOwner> {
    if name.starts_with(LEGACY_BRANCH_PREFIX) {
        let run_id = &name["reflection/".len()..];
        return (!run_id.is_empty()).then(|| ReflectionOwner {
            run_id: run_id.to_owned(),
            epoch: None,
        });
    }
    // The prefix is stripped first because it contains hyphens of its own: splitting the raw name on
    // '-' would take "memory" as the epoch. A worktree directory is named after the suffix, so the
    // prefix is genuinely optional there; the dedicated worktrees root is what makes that safe.
    let suffix = name.strip_prefix(REFLECTION_BRANCH_PREFIX).unwrap_or(name);
    let (epoch, run_id) = suffix.split_once('-')?;
    if epoch.is_empty() || run_id.is_empty() || !epoch.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(ReflectionOwner {
        run_id: run_id.to_owned(),
        epoch: epoch.parse::<f64>().ok(),
    })
}

pub fn is_live(live_run_ids: &BTreeSet<String>, run_id: &str) -> bool {
    live_run_ids.contains(run_id) || live_run_ids.contains(&format!("reflection-{run_id}"))
}

pub fn is_reflection_branch(branch: &str) -> bool {
    branch.starts_with(REFLECTION_BRANCH_PREFIX) || branch.starts_with(LEGACY_BRANCH_PREFIX)
}

pub fn is_epoch_prefixed(name: &str) -> bool {
    match name.split_once('-') {
        Some((epoch, rest)) => {
            !epoch.is_empty() && !rest.is_empty() && epoch.bytes().all(|byte| byte.is_ascii_digit())
        }
        None => false,
    }
}

pub fn basename(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Pure ownership decision over already-collected leftovers.
///
/// Selection rules, in order:
///
/// 1. A name that is not a reflection owner is never selected (someone else's worktree).
/// 2. A live run id protects every surface spelled with it.
/// 3. An already-absent worktree is selected immediately — nothing is writing there.
/// 4. Otherwise the name's epoch must be at or before `now - grace`. A name **without** an epoch (the
///    legacy spelling) passes this test, because it has no minting time to compare and the live-id check
///    has already had its chance.
pub fn select_reflection_orphans(
    leftovers: &ReflectionLeftovers,
    selection: &ReflectionOrphanSelection,
) -> ReflectionLeftovers {
    let grace_ms = selection.grace_ms.unwrap_or(REFLECTION_ORPHAN_GRACE_MS);
    let cutoff = selection.now_ms - grace_ms;
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
        owner.epoch.is_none_or(|epoch| epoch <= cutoff)
    };
    ReflectionLeftovers {
        registered_worktrees: leftovers
            .registered_worktrees
            .iter()
            .filter(|worktree| is_orphan(&basename(&worktree.dir), worktree.present))
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PorcelainWorktree {
    pub dir: PathBuf,
    pub branch: Option<String>,
}

pub fn parse_registered_worktrees(
    porcelain: &str,
    present: impl Fn(&std::path::Path) -> bool,
) -> Vec<RegisteredReflectionWorktree> {
    let mut worktrees = Vec::new();
    let mut dir: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    for line in porcelain.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            if let Some(path) = dir.take() {
                worktrees.push(RegisteredReflectionWorktree {
                    present: present(&path),
                    dir: path,
                    branch: branch.take(),
                });
            }
            branch = None;
            dir = Some(PathBuf::from(rest.trim()));
        } else if let Some(rest) = line.strip_prefix("branch refs/heads/") {
            branch = Some(rest.trim().to_owned());
        }
    }
    if let Some(path) = dir.take() {
        worktrees.push(RegisteredReflectionWorktree {
            present: present(&path),
            dir: path,
            branch,
        });
    }
    worktrees
}

pub fn is_inside(root: &std::path::Path, path: &std::path::Path) -> bool {
    match path.strip_prefix(root) {
        Ok(offset) => !offset.as_os_str().is_empty(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn leftovers(
        worktrees: &[(&str, bool)],
        dirs: &[&str],
        branches: &[&str],
    ) -> ReflectionLeftovers {
        ReflectionLeftovers {
            registered_worktrees: worktrees
                .iter()
                .map(|(dir, present)| RegisteredReflectionWorktree {
                    dir: PathBuf::from(dir),
                    branch: None,
                    present: *present,
                })
                .collect(),
            stray_dirs: dirs.iter().map(PathBuf::from).collect(),
            branches: branches.iter().map(|branch| (*branch).to_owned()).collect(),
        }
    }

    fn selection(now: f64, live: &[&str]) -> ReflectionOrphanSelection {
        ReflectionOrphanSelection {
            live_run_ids: live.iter().map(|id| (*id).to_owned()).collect(),
            now_ms: now,
            grace_ms: None,
        }
    }

    #[test]
    fn a_new_branch_name_parses_its_epoch_and_run() {
        let owner = parse_reflection_owner("memory/reflection-1000-run-abc").expect("parses");
        assert_eq!(owner.run_id, "run-abc");
        assert_eq!(owner.epoch, Some(1000.0));
    }

    #[test]
    fn a_legacy_branch_name_has_no_epoch() {
        let owner = parse_reflection_owner("reflection/run-7").expect("parses");
        assert_eq!(owner.run_id, "run-7");
        assert_eq!(owner.epoch, None);
    }

    #[test]
    fn a_name_without_a_numeric_epoch_is_not_an_owner() {
        assert!(parse_reflection_owner("memory/reflection-abc-run-1").is_none());
        assert!(parse_reflection_owner("memory/reflection--run-1").is_none());
        assert!(parse_reflection_owner("memory/reflection-1000-").is_none());
        assert!(parse_reflection_owner("feature/work").is_none());
    }

    #[test]
    fn a_live_run_protects_its_worktree() {
        let found = leftovers(&[("/refl/1000-run-a", true)], &[], &[]);
        let selected = select_reflection_orphans(&found, &selection(9_999_999.0, &["run-a"]));
        assert!(selected.is_empty());
    }

    #[test]
    fn a_live_legacy_spelling_also_protects() {
        let found = leftovers(&[], &[], &["reflection/run-7"]);
        let selected =
            select_reflection_orphans(&found, &selection(9_999_999.0, &["reflection-run-7"]));
        assert!(selected.is_empty());
    }

    #[test]
    fn a_dead_run_inside_the_grace_window_is_kept() {
        let found = leftovers(&[], &["/refl/1000-run-a"], &[]);
        let selected = select_reflection_orphans(&found, &selection(2000.0, &[]));
        assert!(selected.is_empty(), "a just-started run must not be swept");
    }

    #[test]
    fn a_dead_run_past_the_grace_window_is_selected() {
        let found = leftovers(&[], &["/refl/1000-run-a"], &[]);
        let selected = select_reflection_orphans(
            &found,
            &selection(1000.0 + REFLECTION_ORPHAN_GRACE_MS + 1.0, &[]),
        );
        assert_eq!(selected.stray_dirs.len(), 1);
    }

    #[test]
    fn a_missing_worktree_directory_is_selected_without_waiting() {
        let found = leftovers(&[("/refl/1000-run-a", false)], &[], &[]);
        let selected = select_reflection_orphans(&found, &selection(0.0, &[]));
        assert_eq!(selected.registered_worktrees.len(), 1);
    }

    #[test]
    fn a_registration_for_an_unparseable_directory_is_never_selected() {
        let found = leftovers(&[("/refl/someone-worktree", false)], &[], &[]);
        assert!(select_reflection_orphans(&found, &selection(9_999_999.0, &[])).is_empty());
    }

    #[test]
    fn a_bare_suffix_directory_parses_because_a_worktree_dir_is_named_after_the_suffix() {
        let found = leftovers(&[], &["/refl/1700000000000-run-a"], &[]);
        let selected = select_reflection_orphans(&found, &selection(1.0e15, &[]));
        assert_eq!(selected.stray_dirs.len(), 1);
    }

    #[test]
    fn an_unrelated_name_is_never_selected() {
        let found = leftovers(&[("/refl/someone-worktree", false)], &[], &[]);
        assert!(select_reflection_orphans(&found, &selection(9_999_999.0, &[])).is_empty());
    }

    #[test]
    fn a_stray_directory_that_is_not_epoch_prefixed_is_never_selected() {
        let found = leftovers(&[], &["/refl/tmp"], &[]);
        assert!(select_reflection_orphans(&found, &selection(9_999_999.0, &[])).is_empty());
    }

    #[test]
    fn an_explicit_grace_window_overrides_the_default() {
        let found = leftovers(&[], &["/refl/1000-run-a"], &[]);
        let mut input = selection(2000.0, &[]);
        input.grace_ms = Some(500.0);
        let selected = select_reflection_orphans(&found, &input);
        assert_eq!(selected.stray_dirs.len(), 1);
    }

    #[test]
    fn the_grace_window_boundary_is_inclusive() {
        let found = leftovers(&[], &["/refl/1000-run-a"], &[]);
        let mut input = selection(1000.0, &[]);
        input.grace_ms = Some(0.0);
        assert_eq!(
            select_reflection_orphans(&found, &input).stray_dirs.len(),
            1
        );
    }

    #[test]
    fn only_reflection_branches_are_listed() {
        assert!(is_reflection_branch("memory/reflection-1-run-a"));
        assert!(is_reflection_branch("reflection/run-7"));
        assert!(!is_reflection_branch("main"));
        assert!(!is_reflection_branch("memory/other"));
    }

    #[test]
    fn a_porcelain_listing_parses_every_record() {
        let porcelain = "worktree /refl/1-a\nHEAD abc\nbranch refs/heads/memory/reflection-1-run-a\n\nworktree /refl/2-b\nHEAD def\ndetached\n";
        let worktrees = parse_registered_worktrees(porcelain, |_| true);
        assert_eq!(worktrees.len(), 2);
        assert_eq!(
            worktrees[0].branch.as_deref(),
            Some("memory/reflection-1-run-a")
        );
        assert_eq!(worktrees[1].branch, None);
        assert!(worktrees[0].present);
    }

    #[test]
    fn the_last_porcelain_record_is_not_dropped() {
        let worktrees =
            parse_registered_worktrees("worktree /refl/1-a\nbranch refs/heads/x\n", |_| false);
        assert_eq!(worktrees.len(), 1);
        assert!(!worktrees[0].present);
    }

    #[test]
    fn a_directory_must_be_strictly_below_the_root() {
        assert!(is_inside(Path::new("/refl"), Path::new("/refl/1-a")));
        assert!(!is_inside(Path::new("/refl"), Path::new("/refl")));
        assert!(!is_inside(Path::new("/refl"), Path::new("/other/1-a")));
    }

    #[test]
    fn an_epoch_prefixed_name_needs_two_parts() {
        assert!(is_epoch_prefixed("1000-run-a"));
        assert!(!is_epoch_prefixed("1000"));
        assert!(!is_epoch_prefixed("run-a"));
        assert!(!is_epoch_prefixed("-a"));
    }
}
