//! Validates and commits link repairs on screened reflection tips without trusting run trailers (#12642, #12645).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::git::exec::GitExec;
use crate::git::repo::{COMMIT_TIMEOUT_MS, NO_AUTO_MAINTENANCE};
use crate::memfs::frontmatter_scalar::split_frontmatter;
use crate::memfs::frontmatter_validation::describe_frontmatter_violation;
use crate::memfs::paths::is_memory_content_path;
use crate::sync::redact::scan_secret_like_material;
use crate::tools::claim_frontmatter::restamp_content;

use super::completion_repair::{
    LiteralPathspecRunner, STABLE_LOG_OUTPUT, literal_pathspec_runner, read_dropped_trailers,
};
use super::link_removals::{list_tree, must, removed_paths};
use super::link_rewrite::{LinkTargets, posix_basename, repoint_links};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecognizedLinkRepair {
    pub tip_sha: String,
    pub run_tip: String,
    pub writes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRepair {
    pub tip_sha: String,
    /// Changed paths only the link repair wrote (not touched by the run itself).
    pub link_repairs: Vec<String>,
}

/// Recognizes whether `tip_sha` is the gate's own link-repair commit on top of its parent by
/// recomputing the plan from `base_sha..run_tip` and checking exact path & content equality.
pub fn recognize_link_repair(
    exec: &dyn GitExec,
    dir: &Path,
    base_sha: &str,
    tip_sha: &str,
) -> Result<Option<RecognizedLinkRepair>, String> {
    let run = literal_pathspec_runner(exec, dir);
    let list = must(&run, &["rev-list", "--parents", "-n", "1", tip_sha])?;
    let parents: Vec<&str> = list.split_whitespace().skip(1).collect();
    if parents.len() != 1 {
        return Ok(None);
    }
    let run_tip = parents[0];
    if run_tip == base_sha {
        return Ok(None);
    }
    let ancestor = run.call(&["merge-base", "--is-ancestor", base_sha, run_tip])?;
    if ancestor.code != 0 {
        return Ok(None);
    }
    let changed_raw = must(
        &run,
        &[
            "diff",
            "--name-status",
            "--no-renames",
            "-z",
            run_tip,
            tip_sha,
            "--",
        ],
    )?;
    let changed: Vec<&str> = changed_raw.split('\0').filter(|s| !s.is_empty()).collect();
    if changed.is_empty() || !changed.len().is_multiple_of(2) {
        return Ok(None);
    }
    let mut paths = Vec::new();
    for pair in changed.chunks_exact(2) {
        if pair[0] != "M" {
            return Ok(None);
        }
        paths.push(pair[1].to_owned());
    }
    let skip: BTreeSet<String> = read_dropped_trailers(exec, dir, base_sha, run_tip)?
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    let writes = plan_link_repair(&run, base_sha, run_tip, &skip)?;
    if writes.len() != paths.len() {
        return Ok(None);
    }
    for path in paths {
        let Some(planned) = writes.get(&path) else {
            return Ok(None);
        };
        let actual = must(&run, &["cat-file", "blob", &format!("{tip_sha}:{path}")])?;
        if actual != *planned {
            return Ok(None);
        }
    }
    Ok(Some(RecognizedLinkRepair {
        tip_sha: tip_sha.to_owned(),
        run_tip: run_tip.to_owned(),
        writes,
    }))
}

/// Recomputes link repairs on the screened `run_tip`, writing one commit on top (or restoring
/// `prior` when the plan is identical). Non-UTF-8 notes are skipped untouched.
pub fn apply_link_repair(
    exec: &dyn GitExec,
    dir: &Path,
    base_sha: &str,
    run_tip: &str,
    dropped: &[String],
    prior: Option<&RecognizedLinkRepair>,
) -> Result<Option<LinkRepair>, String> {
    let run = literal_pathspec_runner(exec, dir);
    let skip: BTreeSet<String> = dropped.iter().cloned().collect();
    let writes = plan_link_repair(&run, base_sha, run_tip, &skip)?;
    if writes.is_empty() {
        return Ok(None);
    }
    let tip_sha = match prior {
        Some(prev) if prev.run_tip == run_tip && prev.writes == writes => {
            must(&run, &["reset", "-q", "--hard", &prev.tip_sha])?;
            prev.tip_sha.clone()
        }
        _ => {
            for (path, content) in &writes {
                let abs = dir.join(path);
                if let Some(parent) = abs.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(abs, content).map_err(|e| e.to_string())?;
            }
            let mut add_argv = vec!["add", "--"];
            add_argv.extend(writes.keys().map(String::as_str));
            must(&run, &add_argv)?;

            let mut log_argv: Vec<&str> = STABLE_LOG_OUTPUT.to_vec();
            log_argv.extend(["log", "-1", "--format=%an%n%ae", run_tip]);
            let author_out = must(&run, &log_argv)?;
            let mut lines = author_out.split('\n');
            let name = lines
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("reflection");
            let email = lines
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("reflection@omo.local");
            let name_cfg = format!("user.name={name}");
            let email_cfg = format!("user.email={email}");
            // SAFETY-DIVERGENCE(reflection/completion-links.ts): inherit repo commit budget and
            // disable auto maintenance like completion-repair.rs.
            let mut commit_argv: Vec<&str> = NO_AUTO_MAINTENANCE.to_vec();
            commit_argv.extend([
                "-c",
                name_cfg.as_str(),
                "-c",
                email_cfg.as_str(),
                "commit",
                "-q",
                "--no-verify",
                "-m",
                "chore(memory): repoint links to removed notes",
            ]);
            run.must_with_timeout(&commit_argv, COMMIT_TIMEOUT_MS)?;
            must(&run, &["rev-parse", "--verify", "HEAD"])?
                .trim()
                .to_owned()
        }
    };

    let range = format!("{base_sha}..{run_tip}");
    let by_run_raw = must(&run, &["diff", "--name-only", "-z", &range, "--"])?;
    let by_run: BTreeSet<&str> = by_run_raw.split('\0').filter(|s| !s.is_empty()).collect();
    let mut link_repairs: Vec<String> = writes
        .keys()
        .filter(|path| !by_run.contains(path.as_str()))
        .cloned()
        .collect();
    link_repairs.sort();

    Ok(Some(LinkRepair {
        tip_sha,
        link_repairs,
    }))
}

fn plan_link_repair(
    run: &LiteralPathspecRunner<'_>,
    base_sha: &str,
    run_tip: &str,
    skip: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, String> {
    let tree = list_tree(run, run_tip)?;
    let live: BTreeSet<String> = tree.iter().map(|entry| entry.path.clone()).collect();
    let removals = removed_paths(run, base_sha, run_tip, &live)?;
    let mut writes = BTreeMap::new();
    if removals.removed.is_empty() {
        return Ok(writes);
    }
    let notes: BTreeSet<String> = tree
        .into_iter()
        .filter(|entry| entry.regular && entry.path.ends_with(".md") && !skip.contains(&entry.path))
        .map(|entry| entry.path)
        .collect();

    let mut candidates: BTreeSet<String> = removals.moved_from.keys().cloned().collect();
    let removed_keys: Vec<String> = removals.removed.keys().cloned().collect();
    candidates.extend(notes_naming(run, run_tip, &removed_keys)?);

    let targets = LinkTargets {
        live,
        removed: removals.removed,
    };
    for path in candidates {
        if !notes.contains(&path) {
            continue;
        }
        let Some(current) = note_text(run, run_tip, &path)? else {
            continue;
        };
        let next = repoint_links(
            &path,
            &current,
            &targets,
            removals.moved_from.get(&path).map(String::as_str),
        );
        if next == current {
            continue;
        }
        let stamped = restamped(&current, &next);
        if screen_accepts(&path, &current, &stamped) {
            writes.insert(path, stamped);
        }
    }
    Ok(writes)
}

fn notes_naming(
    run: &LiteralPathspecRunner<'_>,
    revision: &str,
    removed: &[String],
) -> Result<Vec<String>, String> {
    let mut stems = BTreeSet::new();
    for path in removed {
        let base = posix_basename(path);
        let stem = base.strip_suffix(".md").unwrap_or(base);
        if !stem.is_empty() {
            stems.insert(stem.to_owned());
        }
    }
    if stems.is_empty() {
        return Ok(Vec::new());
    }
    let mut argv: Vec<&str> = vec!["grep", "-l", "-z", "-F"];
    for stem in &stems {
        argv.push("-e");
        argv.push(stem.as_str());
    }
    argv.push(revision);
    let result = run.call(&argv)?;
    if result.code == 1 {
        return Ok(Vec::new());
    }
    if result.code != 0 {
        let stderr = result.stderr.trim();
        return Err(if stderr.is_empty() {
            "git grep failed".to_owned()
        } else {
            stderr.to_owned()
        });
    }
    let prefix_len = revision.len() + 1;
    Ok(result
        .stdout
        .split('\0')
        .filter(|entry| entry.len() > prefix_len)
        .map(|entry| entry[prefix_len..].to_owned())
        .collect())
}

fn screen_accepts(path: &str, before: &str, after: &str) -> bool {
    if !scan_secret_like_material(after).is_empty() {
        return false;
    }
    if !is_memory_content_path(path) || describe_frontmatter_violation(after).is_none() {
        return true;
    }
    frontmatter_header(after) == frontmatter_header(before)
}

fn frontmatter_header(content: &str) -> Option<String> {
    split_frontmatter(content).map(|(header, _)| header)
}

fn restamped(current: &str, next: &str) -> String {
    if split_frontmatter(current).is_none() {
        return next.to_owned();
    }
    restamp_content(Some(current), next, None).unwrap_or_else(|_| next.to_owned())
}

/// Reads exact blob bytes; returns `Ok(None)` for non-UTF-8 files so invalid UTF-8 notes are left byte-identical.
fn note_text(
    run: &LiteralPathspecRunner<'_>,
    revision: &str,
    path: &str,
) -> Result<Option<String>, String> {
    let spec = format!("{revision}:{path}");
    let result = run.call_bytes(&["cat-file", "blob", &spec])?;
    if result.code != 0 {
        let stderr = result.stderr.trim();
        return Err(if stderr.is_empty() {
            format!("git cat-file blob {spec} failed")
        } else {
            stderr.to_owned()
        });
    }
    Ok(String::from_utf8(result.stdout).ok())
}
