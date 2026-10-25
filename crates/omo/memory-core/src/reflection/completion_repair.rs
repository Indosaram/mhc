//! Lossless partial-entry repair: dropped receipts and branch squashing for reflection completion.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::git::exec::{GitExec, GitExecOptions, GitExecResult};
use crate::sync::redact::redact_secret_like_material;

const GIT_TIMEOUT_MS: u64 = 30_000;

/// Mirrors upstream `git/stable-log-output.ts`: a user's `log.showSignature` puts gpg/ssh
/// verification lines ahead of every parsed `--format` read, and `log.follow` turns a single-path
/// log into `--follow`, which `--no-renames` rejects outright.
pub const STABLE_LOG_OUTPUT: [&str; 4] = ["-c", "log.showSignature=false", "-c", "log.follow=false"];

/// One memory entry the completion gate refused and removed from the run's commit. `path` is the
/// repository path itself (the agent chose it, so it can be secret-shaped): redact it before
/// printing. `reason` never carries the matched value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedEntry {
    pub path: String,
    pub reason: String,
}

/// The squashed commit records what it left out, so a later validation of the same tip still knows.
pub const DROPPED_TRAILER: &str = "Omo-Completion-Dropped";

/// One line a finalizer can put in the run's detail; every path and reason is redacted here.
pub fn describe_dropped_entries(dropped: &[DroppedEntry]) -> String {
    let items: Vec<String> = dropped
        .iter()
        .map(|entry| {
            format!(
                "{} ({})",
                redact_secret_like_material(&entry.path),
                redact_secret_like_material(&entry.reason)
            )
        })
        .collect();
    format!(
        "Dropped {} invalid memory entr{}: {}",
        dropped.len(),
        if dropped.len() == 1 { "y" } else { "ies" },
        items.join("; ")
    )
}

/// JSON keeps any path (a ` | `, a newline) intact on read-back; the values are redacted first.
fn trailer_line(entry: &DroppedEntry) -> String {
    let mut object = Map::new();
    object.insert(
        "path".to_string(),
        Value::String(redact_secret_like_material(&entry.path)),
    );
    object.insert(
        "reason".to_string(),
        Value::String(redact_secret_like_material(&entry.reason)),
    );
    format!("{DROPPED_TRAILER}: {}", Value::Object(object))
}

fn parse_trailer_value(value: &str) -> Option<DroppedEntry> {
    let parsed: Value = serde_json::from_str(value).ok()?;
    let object = parsed.as_object()?;
    let path = object.get("path")?.as_str()?;
    let reason = object.get("reason")?.as_str()?;
    Some(DroppedEntry {
        path: path.to_string(),
        reason: reason.to_string(),
    })
}

/// The dropped entries a previous squash recorded on `tip_sha`, when the branch is exactly that one
/// commit on the base; empty otherwise. Lets a second validation of an already-repaired tip report
/// what was dropped.
pub fn read_dropped_trailers(
    exec: &dyn GitExec,
    dir: &Path,
    base_sha: &str,
    tip_sha: &str,
) -> Result<Vec<DroppedEntry>, String> {
    let run = Run { exec, dir };
    let parent_spec = format!("{tip_sha}^");
    let parent = run.call(&["rev-parse", "--verify", &parent_spec])?;
    if parent.code != 0 || parent.stdout.trim() != base_sha {
        return Ok(Vec::new());
    }
    let mut argv: Vec<&str> = STABLE_LOG_OUTPUT.to_vec();
    argv.extend(["log", "-1", "--format=%B", tip_sha]);
    let body = run.must(&argv)?;
    let prefix = format!("{DROPPED_TRAILER}: ");
    let mut entries = Vec::new();
    for line in body.split('\n') {
        let Some(value) = line.strip_prefix(prefix.as_str()) else {
            continue;
        };
        if let Some(entry) = parse_trailer_value(value) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Every git call here that names a note passes the agent-chosen path as a pathspec.
/// `GIT_LITERAL_PATHSPECS` makes each one match only that exact path: without it a name such as
/// `k*.md`, `k[e]ep1.md` or `:!x.md` is a glob or magic pathspec, and dropping it would delete or
/// revert OTHER notes.
fn literal_pathspec_env() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        ("GIT_LITERAL_PATHSPECS".to_string(), "1".to_string()),
        // git refuses literal pathspecs combined with glob/icase ones, so a user's own setting must
        // not fail every drop.
        ("GIT_GLOB_PATHSPECS".to_string(), "0".to_string()),
        ("GIT_ICASE_PATHSPECS".to_string(), "0".to_string()),
        ("GIT_NOGLOB_PATHSPECS".to_string(), "0".to_string()),
    ])
}

struct Run<'a> {
    exec: &'a dyn GitExec,
    dir: &'a Path,
}

impl Run<'_> {
    fn call(&self, argv: &[&str]) -> Result<GitExecResult, String> {
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_string()).collect();
        self.exec
            .run(
                &argv,
                &GitExecOptions {
                    cwd: self.dir.to_path_buf(),
                    timeout_ms: GIT_TIMEOUT_MS,
                    env: literal_pathspec_env(),
                    stdin: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn must(&self, argv: &[&str]) -> Result<String, String> {
        let result = self.call(argv)?;
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
}

/// destination -> source of every rename in `base_sha..tip_sha`, so dropping a moved file also
/// undoes the move.
fn rename_sources(
    run: &Run<'_>,
    base_sha: &str,
    tip_sha: &str,
) -> Result<BTreeMap<String, String>, String> {
    let range = format!("{base_sha}..{tip_sha}");
    let out = run.must(&[
        "diff",
        "--name-status",
        "-M",
        "-z",
        range.as_str(),
        "--",
    ])?;
    let parts: Vec<&str> = out.split('\0').collect();
    let mut sources = BTreeMap::new();
    let mut index = 0;
    while index < parts.len() {
        let status = parts[index];
        if status.is_empty() {
            break;
        }
        if status.starts_with('R') || status.starts_with('C') {
            let from = parts.get(index + 1).copied();
            let to = parts.get(index + 2).copied();
            if status.starts_with('R')
                && let (Some(from), Some(to)) = (from, to)
            {
                sources.insert(to.to_string(), from.to_string());
            }
            index += 3;
        } else {
            index += 2;
        }
    }
    Ok(sources)
}

/// Number of commits in `base_sha..tip_sha`.
pub fn commit_count(
    exec: &dyn GitExec,
    dir: &Path,
    base_sha: &str,
    tip_sha: &str,
) -> Result<usize, String> {
    let run = Run { exec, dir };
    let range = format!("{base_sha}..{tip_sha}");
    let out = run.must(&["rev-list", "--count", range.as_str()])?;
    Ok(out.trim().parse::<usize>().unwrap_or(0))
}

/// Rewrite the run's branch as ONE commit on the recorded base that keeps every change except the
/// given paths. Squashing is deliberate: the branch is merged `--no-ff` with its whole history, so a
/// blob a dropped entry introduced in an earlier commit would otherwise still ride along. A dropped
/// path returns to its base content (or disappears when the base lacked it); when it was the
/// destination of a rename, the rename's source is restored too so no file is lost. Returns the new
/// tip, which is the base itself when nothing is left to keep.
pub fn squash_without_paths(
    exec: &dyn GitExec,
    dir: &Path,
    base_sha: &str,
    tip_sha: &str,
    dropped: &BTreeSet<String>,
    record: &[DroppedEntry],
) -> Result<String, String> {
    let run = Run { exec, dir };
    let sources = rename_sources(&run, base_sha, tip_sha)?;
    let mut identity_argv: Vec<&str> = STABLE_LOG_OUTPUT.to_vec();
    identity_argv.extend(["log", "-1", "--format=%an%n%ae%n%s", tip_sha]);
    let identity_out = run.must(&identity_argv)?;
    let identity: Vec<&str> = identity_out.split('\n').collect();
    let mut restore: Vec<String> = dropped.iter().cloned().collect();
    for path in dropped {
        if let Some(source) = sources.get(path)
            && !restore.iter().any(|entry| entry == source)
        {
            restore.push(source.clone());
        }
    }
    run.must(&["reset", "--soft", base_sha])?;
    for path in &restore {
        let spec = format!("{base_sha}:{path}");
        let in_base = run.call(&["cat-file", "-e", spec.as_str()])?;
        if in_base.code == 0 {
            run.must(&["checkout", base_sha, "--", path])?;
        } else {
            run.must(&["rm", "-rf", "-q", "--ignore-unmatch", "--", path])?;
        }
    }
    let cached = run.call(&["diff", "--cached", "--quiet", base_sha, "--"])?;
    if cached.code != 0 {
        let name = identity.first().copied().unwrap_or("reflection");
        let email = identity.get(1).copied().unwrap_or("reflection@omo.local");
        let subject = identity.get(2).copied().unwrap_or("reflection");
        let mut lines = vec![
            format!(
                "{} (kept valid entries)",
                redact_secret_like_material(subject)
            ),
            String::new(),
        ];
        lines.extend(record.iter().map(trailer_line));
        let message = lines.join("\n");
        let name_arg = format!("user.name={name}");
        let email_arg = format!("user.email={email}");
        run.must(&[
            "-c",
            name_arg.as_str(),
            "-c",
            email_arg.as_str(),
            "commit",
            "-q",
            "--no-verify",
            "-m",
            message.as_str(),
        ])?;
    }
    Ok(run.must(&["rev-parse", "--verify", "HEAD"])?.trim().to_string())
}

#[cfg(test)]
#[path = "completion_repair_tests.rs"]
mod tests;
