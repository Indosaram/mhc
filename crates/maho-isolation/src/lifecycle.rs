//! Prepare → run → capture → merge-back or keep → cleanup for one task. Ported from the MIT
//! oh-my-pi task isolation (`task/worktree.ts` `ensureIsolation`, `captureIsolationBaseline`,
//! `captureDeltaPatch`, `commitToBranch`; `task/isolation-runner.ts` `runIsolatedSubprocess`,
//! `mergeIsolatedChanges`, `retainIsolationWorkspace`), adapted: the baseline and the result are
//! snapshot trees written through a temporary index, so the delta is one `git diff` between two
//! trees; the plain (non-git) path is NEW (spec §4).
use crate::backend::{self, BackendKind};
use crate::owner;
use crate::settings::{IsolationSettings, MergeMode};
use crate::tree::{self, EntryMeta, Manifest};
use maho_git::{ApplyOptions, Git};
use std::path::{Path, PathBuf};

/// Name of the materialised directory inside an isolation's base directory.
pub const MERGED_DIR: &str = "m";
/// Branch prefix of `merge: branch` commits.
pub const BRANCH_PREFIX: &str = "mhc/task/";

/// What the work is compared against.
#[derive(Clone, Debug)]
pub enum Baseline {
    /// Snapshot tree of the isolation right after it was made, and its `HEAD`.
    Git { tree: String, head: String },
    /// Manifest of the copy right after it was made.
    Plain(Manifest),
}

/// A prepared isolation.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub id: String,
    /// Repository root (or directory) the isolation was made from.
    pub source: PathBuf,
    /// `t<digest>` directory holding the owner marker and `m`.
    pub base: PathBuf,
    pub merged: PathBuf,
    /// Where the child runs (same relative position as the task's cwd).
    pub cwd: PathBuf,
    pub kind: BackendKind,
    pub settings: IsolationSettings,
    pub baseline: Baseline,
    /// Directory for patch artifacts.
    pub patches: PathBuf,
    pub fallback: Option<String>,
}

/// FNV-1a over the source root and task id: a stable short directory name.
pub fn segment(source: &Path, id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in source
        .to_string_lossy()
        .bytes()
        .chain([0])
        .chain(id.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("t{:09x}", hash & 0xf_ffff_ffff)
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Makes the isolation for task `id` started from `cwd` under the isolation root `wt`.
///
/// # Errors
/// The source cannot be isolated or any step failed (nothing is left behind).
pub fn prepare(
    wt: &Path,
    id: &str,
    cwd: &Path,
    settings: &IsolationSettings,
) -> Result<Prepared, String> {
    let cwd = canonical(cwd);
    let probe = Git::new(&cwd);
    let is_git = probe.is_repository();
    let source = if is_git {
        canonical(&probe.repo_root().map_err(|e| e.to_string())?)
    } else {
        cwd.clone()
    };
    let wt = canonical(&{
        std::fs::create_dir_all(wt).map_err(|e| format!("create {}: {e}", wt.display()))?;
        wt.to_path_buf()
    });
    if wt.starts_with(&source) {
        return Err(format!(
            "the isolation directory {} is inside {}; set MAHO_WORKTREE_DIR to a path outside the workspace",
            wt.display(),
            source.display()
        ));
    }
    let (kind, fallback) = backend::resolve(&settings.backend, &source, is_git)?;
    let base = wt.join(segment(&source, id));
    let merged = base.join(MERGED_DIR);
    // A leftover of an earlier run of the same id goes first.
    if base.exists() {
        if let Some(previous) = owner::read_owner(&base) {
            backend::stop(
                if previous.backend == "worktree" {
                    BackendKind::Worktree
                } else {
                    BackendKind::Copy
                },
                &previous.source,
                &merged,
            );
        }
        tree::remove_existing(&base).map_err(|e| format!("clear {}: {e}", base.display()))?;
    }
    std::fs::create_dir_all(&base).map_err(|e| format!("create {}: {e}", base.display()))?;
    let result = (|| {
        owner::write_owner(&base, id, &source, kind.as_str())
            .map_err(|e| format!("write owner marker: {e}"))?;
        backend::start(kind, &source, &merged)?;
        let baseline = if is_git {
            let git = Git::new(&merged);
            let tree = git
                .snapshot_tree(&base.join("scratch"))
                .map_err(|e| e.to_string())?;
            Baseline::Git {
                tree,
                head: git.head().unwrap_or_default(),
            }
        } else {
            Baseline::Plain(
                tree::manifest(&merged).map_err(|e| format!("scan {}: {e}", merged.display()))?,
            )
        };
        Ok(baseline)
    })();
    let baseline = match result {
        Ok(baseline) => baseline,
        Err(error) => {
            backend::stop(kind, &source, &merged);
            let _ = tree::remove_existing(&base);
            return Err(error);
        }
    };
    let rel = cwd
        .strip_prefix(&source)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    Ok(Prepared {
        id: id.to_owned(),
        cwd: merged.join(rel),
        source,
        base,
        merged,
        kind,
        settings: settings.clone(),
        baseline,
        patches: wt.join("patches"),
        fallback,
    })
}

fn count_files(patch: &str) -> usize {
    patch
        .lines()
        .filter(|line| line.starts_with("diff --git "))
        .count()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Moves the isolation out of its deterministic slot so a later run cannot wipe it; returns where
/// it now is.
fn retain(prepared: &Prepared) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let name = format!(
        "{}.retained-{stamp:x}-{:x}",
        prepared
            .base
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    );
    let target = prepared.base.with_file_name(name);
    if std::fs::create_dir_all(&target).is_err() {
        return prepared.merged.clone();
    }
    let new_merged = target.join(MERGED_DIR);
    let moved = match prepared.kind {
        BackendKind::Worktree => {
            let from = prepared.merged.to_string_lossy().into_owned();
            let to = new_merged.to_string_lossy().into_owned();
            Git::new(&prepared.source)
                .run(&["worktree", "move", &from, &to])
                .is_ok()
        }
        BackendKind::Copy => std::fs::rename(&prepared.merged, &new_merged).is_ok(),
    };
    if !moved {
        let _ = std::fs::remove_dir(&target);
        return prepared.merged.clone();
    }
    let _ = std::fs::rename(
        prepared.base.join(owner::OWNER_FILE),
        target.join(owner::OWNER_FILE),
    );
    let _ = tree::remove_existing(&prepared.base);
    new_merged
}

fn cleanup(prepared: &Prepared) {
    backend::stop(prepared.kind, &prepared.source, &prepared.merged);
    let _ = tree::remove_existing(&prepared.base);
}

fn patch_path(prepared: &Prepared) -> PathBuf {
    let segment = prepared
        .base
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let safe: String = prepared
        .id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    prepared.patches.join(format!("{segment}-{safe}.patch"))
}

fn write_patch(prepared: &Prepared, patch: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(&prepared.patches)
        .map_err(|e| format!("create {}: {e}", prepared.patches.display()))?;
    let path = patch_path(prepared);
    std::fs::write(&path, patch).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Captures, integrates (or keeps) and cleans up after the child ended; returns the result note.
pub fn finish(prepared: &Prepared, succeeded: bool) -> String {
    let mut note = String::from("Isolation");
    note.push_str(&format!(" ({})", prepared.kind.as_str()));
    if let Some(reason) = &prepared.fallback {
        note.push_str(&format!(", fell back: {reason}"));
    }
    note.push_str(": ");
    if !succeeded {
        cleanup(prepared);
        note.push_str("the task did not succeed; its isolated changes were discarded.");
        return note;
    }
    let outcome = match &prepared.baseline {
        Baseline::Git { tree, head } => finish_git(prepared, tree, head),
        Baseline::Plain(manifest) => finish_plain(prepared, manifest),
    };
    match outcome {
        Ok((text, keep)) => {
            if keep {
                let at = retain(prepared);
                note.push_str(&format!(
                    "{text} The isolated workspace is kept at {}.",
                    at.display()
                ));
            } else {
                cleanup(prepared);
                note.push_str(&text);
            }
        }
        Err(error) => {
            let at = retain(prepared);
            note.push_str(&format!(
                "capturing the changes failed ({error}); the isolated workspace is kept at {} so nothing is lost.",
                at.display()
            ));
        }
    }
    note
}

/// `(note, keep the isolation)`.
type Outcome = Result<(String, bool), String>;

fn finish_git(prepared: &Prepared, base_tree: &str, head: &str) -> Outcome {
    let isolated = Git::new(&prepared.merged);
    let current = isolated
        .snapshot_tree(&prepared.base.join("scratch"))
        .map_err(|e| e.to_string())?;
    if current == base_tree {
        return Ok(("no changes.".into(), false));
    }
    let patch = isolated
        .diff_trees(base_tree, &current)
        .map_err(|e| e.to_string())?;
    if patch.is_empty() {
        return Ok(("no changes.".into(), false));
    }
    let files = count_files(&String::from_utf8_lossy(&patch));
    let path = write_patch(prepared, &patch)?;
    let parent = Git::new(&prepared.source);
    if prepared.settings.merge == MergeMode::Branch && !head.is_empty() {
        return finish_branch(prepared, &patch, head, files, &path);
    }
    if !prepared.settings.apply {
        return Ok((
            format!(
                "{} changed; not applied (task.isolation.apply is off). Patch: {}",
                plural(files, "file", "files"),
                path.display()
            ),
            false,
        ));
    }
    let check = |reverse| {
        parent
            .apply(
                &patch,
                ApplyOptions {
                    check: true,
                    reverse,
                    ..ApplyOptions::default()
                },
            )
            .is_ok()
    };
    let forward = check(false);
    if !forward && check(true) {
        return Ok((
            "the changes are already present in the workspace; nothing to apply.".into(),
            false,
        ));
    }
    if forward && parent.apply(&patch, ApplyOptions::default()).is_ok() {
        return Ok((
            format!(
                "applied {} to {} (patch kept at {}).",
                plural(files, "changed file", "changed files"),
                prepared.source.display(),
                path.display()
            ),
            false,
        ));
    }
    Ok((
        format!(
            "the changes ({}) do not apply cleanly to {}; they were not applied. Patch: {}",
            plural(files, "file", "files"),
            prepared.source.display(),
            path.display()
        ),
        false,
    ))
}

fn finish_branch(
    prepared: &Prepared,
    patch: &[u8],
    head: &str,
    files: usize,
    path: &Path,
) -> Outcome {
    let parent = Git::new(&prepared.source);
    let scratch = prepared.base.join("scratch");
    let tree = parent
        .patched_tree(head, patch, &scratch)
        .map_err(|e| format!("build the branch tree: {e}"))?;
    let message = format!("Changes from isolated task {}", prepared.id);
    let commit = parent
        .commit_tree(&tree, head, &message)
        .map_err(|e| e.to_string())?;
    let branch = format!("{BRANCH_PREFIX}{}", prepared.id);
    parent
        .run(&["branch", "-f", &branch, &commit])
        .map_err(|e| format!("create branch {branch}: {e}"))?;
    if !prepared.settings.apply {
        return Ok((
            format!(
                "{} committed on branch `{branch}`; not merged (task.isolation.apply is off). Patch: {}",
                plural(files, "changed file", "changed files"),
                path.display()
            ),
            false,
        ));
    }
    match parent.run_committing(&["cherry-pick", "--no-edit", &commit]) {
        Ok(_) => {
            let _ = parent.run(&["branch", "-D", &branch]);
            Ok((
                format!(
                    "committed {} and cherry-picked it onto {} ({}).",
                    plural(files, "changed file", "changed files"),
                    prepared.source.display(),
                    parent
                        .line(&["rev-parse", "--short", "HEAD"])
                        .unwrap_or_default()
                ),
                false,
            ))
        }
        Err(error) => {
            let _ = parent.run(&["cherry-pick", "--abort"]);
            Ok((
                format!(
                    "branch `{branch}` could not be merged ({error}); it is kept for manual resolution. Patch: {}",
                    path.display()
                ),
                false,
            ))
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Change {
    Added(String),
    Modified(String),
    Removed(String),
}

fn same_content(a: &Path, b: &Path) -> bool {
    match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
        (Ok(ma), Ok(mb)) if ma.file_type().is_symlink() && mb.file_type().is_symlink() => {
            std::fs::read_link(a).ok() == std::fs::read_link(b).ok()
        }
        (Ok(ma), Ok(mb)) if ma.is_file() && mb.is_file() => {
            ma.len() == mb.len() && std::fs::read(a).ok() == std::fs::read(b).ok()
        }
        _ => false,
    }
}

fn plain_changes(prepared: &Prepared, manifest: &Manifest) -> Result<Vec<Change>, String> {
    let now = tree::manifest(&prepared.merged)
        .map_err(|e| format!("scan {}: {e}", prepared.merged.display()))?;
    let mut changes = Vec::new();
    for (rel, meta) in &now {
        match manifest.get(rel) {
            None => changes.push(Change::Added(rel.clone())),
            Some(before) if before != meta => {
                if !same_content(&prepared.source.join(rel), &prepared.merged.join(rel)) {
                    changes.push(Change::Modified(rel.clone()));
                }
            }
            Some(_) => {}
        }
    }
    for rel in manifest.keys() {
        if !now.contains_key(rel) {
            changes.push(Change::Removed(rel.clone()));
        }
    }
    Ok(changes)
}

fn text_of(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn plain_patch(prepared: &Prepared, changes: &[Change]) -> String {
    let mut out = String::new();
    for change in changes {
        let (rel, old, new) = match change {
            Change::Added(rel) => (rel, None, Some(prepared.merged.join(rel))),
            Change::Modified(rel) => (
                rel,
                Some(prepared.source.join(rel)),
                Some(prepared.merged.join(rel)),
            ),
            Change::Removed(rel) => (rel, Some(prepared.source.join(rel)), None),
        };
        let old_text = old.as_deref().map_or(Some(String::new()), text_of);
        let new_text = new.as_deref().map_or(Some(String::new()), text_of);
        let (Some(old_text), Some(new_text)) = (old_text, new_text) else {
            out.push_str(&format!("Binary files a/{rel} and b/{rel} differ\n"));
            continue;
        };
        let a = if old.is_some() {
            format!("a/{rel}")
        } else {
            "/dev/null".into()
        };
        let b = if new.is_some() {
            format!("b/{rel}")
        } else {
            "/dev/null".into()
        };
        let diff = similar::TextDiff::from_lines(&old_text, &new_text);
        out.push_str(&format!("diff --git a/{rel} b/{rel}\n"));
        out.push_str(
            &diff
                .unified_diff()
                .context_radius(3)
                .header(&a, &b)
                .to_string(),
        );
    }
    out
}

fn finish_plain(prepared: &Prepared, manifest: &Manifest) -> Outcome {
    let changes = plain_changes(prepared, manifest)?;
    if changes.is_empty() {
        return Ok(("no changes.".into(), false));
    }
    let path = write_patch(prepared, plain_patch(prepared, &changes).as_bytes())?;
    let files = changes.len();
    if !prepared.settings.apply {
        return Ok((
            format!(
                "{} changed; not applied (task.isolation.apply is off). Patch: {}",
                plural(files, "file", "files"),
                path.display()
            ),
            true,
        ));
    }
    // All or nothing: every target must still be as it was when the copy was made.
    let conflicts: Vec<&str> = changes
        .iter()
        .filter_map(|change| {
            let (rel, expect_absent) = match change {
                Change::Added(rel) => (rel, true),
                Change::Modified(rel) | Change::Removed(rel) => (rel, false),
            };
            let current = EntryMeta::of(&prepared.source.join(rel));
            let unchanged = if expect_absent {
                current.is_none()
            } else {
                current.as_ref() == manifest.get(rel)
            };
            (!unchanged).then_some(rel.as_str())
        })
        .collect();
    if !conflicts.is_empty() {
        return Ok((
            format!(
                "the workspace changed meanwhile ({}); nothing was applied. Patch: {}.",
                conflicts.join(", "),
                path.display()
            ),
            true,
        ));
    }
    for change in &changes {
        match change {
            Change::Added(rel) | Change::Modified(rel) => {
                let target = prepared.source.join(rel);
                tree::remove_existing(&target).map_err(|e| format!("replace {rel}: {e}"))?;
                tree::copy_path(&prepared.merged.join(rel), &target)
                    .map_err(|e| format!("write {rel}: {e}"))?;
            }
            Change::Removed(rel) => {
                tree::remove_existing(&prepared.source.join(rel))
                    .map_err(|e| format!("remove {rel}: {e}"))?;
            }
        }
    }
    Ok((
        format!(
            "applied {} to {} (patch kept at {}).",
            plural(files, "changed file", "changed files"),
            prepared.source.display(),
            path.display()
        ),
        false,
    ))
}
