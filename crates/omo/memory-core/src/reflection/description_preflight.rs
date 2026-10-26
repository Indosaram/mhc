//! Length-only repair of oversized memory descriptions before the reflection completion gate runs.

use std::path::Path;

use serde_json::Value;

use crate::git::exec::GitExec;
use crate::git::{GitCommitAuthor, GitMemoryRepo, GitMemoryRepoOptions};
use crate::memfs::{
    MAX_DESCRIPTION_LENGTH, describe_frontmatter_violation, is_memory_content_path,
    parse_memory_file, render_string_scalar, split_frontmatter,
};
use crate::sync::redact::redact_secret_like_material;

use super::completion_repair::STABLE_LOG_OUTPUT;
use super::completion_validation::{frontmatter_block, list_reflection_changed_paths};
use super::worktree::ReflectionWorktree;

/// One description the preflight shortened, with the lengths the commit trailer records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrimmedDescription {
    pub path: String,
    pub original_length: usize,
    pub new_length: usize,
}

/// Diagnostic sink for a preflight that could not repair the branch (upstream
/// `DescriptionPreflightWarn`): the message plus structured fields, both already redacted.
pub type DescriptionPreflightWarn<'a> = dyn Fn(&str, &Value) + 'a;

/// Repair only length violations in a clean, finished reflection branch, before its pure gate runs.
pub fn preflight_reflection_descriptions(
    worktree: &ReflectionWorktree,
    recorded_base: &str,
    exec: &dyn GitExec,
    warn: Option<&DescriptionPreflightWarn<'_>>,
) -> Vec<TrimmedDescription> {
    let repo = match GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: worktree.dir.clone(),
        agent_id: worktree.parent.agent_id.clone(),
        exec: None,
        install_hooks: None,
    }) {
        Ok(repo) => repo,
        Err(error) => {
            warn_redacted(
                warn,
                "memory description preflight failed; completion validation will decide the outcome",
                &error.to_string(),
                None,
            );
            return Vec::new();
        }
    };
    let mut rewritten: Vec<String> = Vec::new();
    match trim_descriptions(worktree, recorded_base, exec, &repo, &mut rewritten) {
        Ok(trimmed) => trimmed,
        Err(error) => {
            let rollback_error = restore_paths(exec, &worktree.dir, &rewritten).err();
            warn_redacted(
                warn,
                "memory description preflight failed; completion validation will decide the outcome",
                &error,
                rollback_error.as_deref(),
            );
            Vec::new()
        }
    }
}

/// Report through the diagnostic sink, and never let that sink prevent the gate from settling the
/// run: upstream wraps the call in a `try`/`catch` for the same reason.
fn warn_redacted(
    warn: Option<&DescriptionPreflightWarn<'_>>,
    message: &str,
    error: &str,
    rollback_error: Option<&str>,
) {
    let Some(warn) = warn else {
        return;
    };
    let mut fields = serde_json::Map::new();
    fields.insert(
        "error".to_string(),
        Value::String(redact_secret_like_material(error)),
    );
    if let Some(rollback_error) = rollback_error {
        fields.insert(
            "rollbackError".to_string(),
            Value::String(redact_secret_like_material(rollback_error)),
        );
    }
    let fields = Value::Object(fields);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| warn(message, &fields)));
}

fn trim_descriptions(
    worktree: &ReflectionWorktree,
    recorded_base: &str,
    exec: &dyn GitExec,
    repo: &GitMemoryRepo,
    rewritten: &mut Vec<String>,
) -> Result<Vec<TrimmedDescription>, String> {
    // Leave dirty or tampered runs to the gate; never commit their unrelated changes.
    let git_file =
        std::fs::read_to_string(&worktree.git_file_path).map_err(|err| err.to_string())?;
    if git_file != worktree.git_file_snapshot {
        return Ok(Vec::new());
    }
    let config = match std::fs::read_to_string(&worktree.common_config_path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.to_string()),
    };
    if config != worktree.common_config_snapshot {
        return Ok(Vec::new());
    }
    let git = |args: &[&str]| -> Result<String, String> {
        let result = exec
            .run_in(&worktree.dir, args)
            .map_err(|error| error.to_string())?;
        if result.code != 0 {
            let stderr = result.stderr.trim();
            return Err(if stderr.is_empty() {
                format!("git {} failed", args.join(" "))
            } else {
                stderr.to_string()
            });
        }
        Ok(result.stdout.trim_end().to_string())
    };
    if !git(&["status", "--porcelain"])?.trim().is_empty() {
        return Ok(Vec::new());
    }
    if git(&["branch", "--show-current"])? != worktree.branch {
        return Ok(Vec::new());
    }
    let tip_sha = git(&["rev-parse", "--verify", "HEAD"])?;
    if tip_sha == recorded_base {
        return Ok(Vec::new());
    }
    let based = exec
        .run_in(
            &worktree.dir,
            &["merge-base", "--is-ancestor", recorded_base, &tip_sha],
        )
        .map_err(|error| error.to_string())?;
    if based.code != 0 {
        return Ok(Vec::new());
    }

    let exceeds_prefix = format!("'description' exceeds {MAX_DESCRIPTION_LENGTH} characters (");
    let mut trimmed: Vec<TrimmedDescription> = Vec::new();
    for path in list_reflection_changed_paths(exec, &worktree.dir, recorded_base, &tip_sha)? {
        if !is_memory_content_path(&path) {
            continue;
        }
        let tip_spec = format!("{tip_sha}:{path}");
        let blob = exec
            .run_in(&worktree.dir, &["cat-file", "blob", tip_spec.as_str()])
            .map_err(|error| error.to_string())?;
        if blob.code != 0 {
            // Deleted paths have no description to repair.
            continue;
        }
        let content = blob.stdout;
        let Some(violation) = describe_frontmatter_violation(&content) else {
            continue;
        };
        // Grammar is validated before description length, so equality excludes every other violation.
        if !violation.starts_with(&exceeds_prefix) {
            continue;
        }
        let base_spec = format!("{recorded_base}:{path}");
        let base = exec
            .run_in(&worktree.dir, &["cat-file", "blob", base_spec.as_str()])
            .map_err(|error| error.to_string())?;
        if base.code == 0 && frontmatter_block(&base.stdout) == frontmatter_block(&content) {
            continue;
        }
        let parsed = parse_memory_file(&content).map_err(|error| error.to_string())?;
        let original = parsed.frontmatter.description;
        if violation != format!("{exceeds_prefix}{})", description_length(&original)) {
            continue;
        }
        let absolute = worktree.dir.join(&path);
        match std::fs::symlink_metadata(&absolute) {
            Ok(metadata) if metadata.is_file() => {}
            _ => continue,
        }
        let description = trim_description(&original);
        let header = split_frontmatter(&content)
            .map(|(header, _)| header)
            .unwrap_or_default();
        let header_start = content.find('\n').map(|index| index + 1).unwrap_or(0);
        let rendered = format!(
            "{}{}{}",
            &content[..header_start],
            replace_description_line(&header, &render_string_scalar(&description)),
            &content[header_start + header.len()..]
        );
        rewritten.push(path.clone());
        std::fs::write(&absolute, &rendered).map_err(|error| error.to_string())?;
        let new_description = parse_memory_file(&rendered)
            .map_err(|error| error.to_string())?
            .frontmatter
            .description;
        trimmed.push(TrimmedDescription {
            path,
            original_length: description_length(&original),
            new_length: description_length(&new_description),
        });
    }
    if trimmed.is_empty() {
        return Ok(trimmed);
    }

    let mut identity_argv: Vec<&str> = STABLE_LOG_OUTPUT.to_vec();
    identity_argv.extend(["show", "-s", "--format=%an%x00%ae", tip_sha.as_str()]);
    let identity = git(&identity_argv)?;
    let mut parts = identity.split('\0');
    let author_name = parts.next().unwrap_or_default().to_string();
    let author_email = parts.next().unwrap_or_default().to_string();
    let mut lines = vec![
        "fix(reflection): trim oversized memory descriptions".to_string(),
        String::new(),
    ];
    lines.extend(trimmed.iter().map(|entry| {
        format!(
            "Omo-Description-Trimmed: {} ({} -> {})",
            entry.path, entry.original_length, entry.new_length
        )
    }));
    let reason = lines.join("\n");
    let paths: Vec<&str> = trimmed.iter().map(|entry| entry.path.as_str()).collect();
    repo.commit_write(
        paths.as_slice(),
        &reason,
        &GitCommitAuthor {
            agent_id: worktree.parent.agent_id.clone(),
            author_name,
            author_email: Some(author_email),
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(trimmed)
}

/// The measure the validator reports in `'description' exceeds 1024 characters (<n>)`, so the
/// preflight and the gate agree on what an oversized description is.
fn description_length(value: &str) -> usize {
    value.chars().count()
}

/// Cut to `MAX_DESCRIPTION_LENGTH - 1` UTF-16 units without splitting a code point, prefer the last
/// word boundary inside that prefix, and end with an ellipsis.
fn trim_description(original: &str) -> String {
    let mut prefix = String::new();
    let mut units = 0usize;
    for code_point in original.chars() {
        if units + code_point.len_utf16() > MAX_DESCRIPTION_LENGTH - 1 {
            break;
        }
        prefix.push(code_point);
        units += code_point.len_utf16();
    }
    let boundary = if original[prefix.len()..].starts_with(' ') {
        Some(prefix.len())
    } else {
        prefix.rfind(' ')
    };
    match boundary {
        Some(index) if index > 0 => format!("{}…", original[..index].trim_end()),
        _ => format!("{}…", prefix.trim_end()),
    }
}

/// Replace the value of the first `description:` line of `header` with `rendered`, leaving every
/// other byte of the header (CRLF endings, key order, other keys) untouched.
fn replace_description_line(header: &str, rendered: &str) -> String {
    let mut output = String::with_capacity(header.len());
    let mut replaced = false;
    let mut rest = header;
    while !rest.is_empty() {
        let (line, next) = match rest.find('\n') {
            Some(index) => (&rest[..=index], &rest[index + 1..]),
            None => (rest, ""),
        };
        let (core, terminator) = match line.strip_suffix('\n') {
            Some(core) => match core.strip_suffix('\r') {
                Some(core) => (core, "\r\n"),
                None => (core, "\n"),
            },
            None => (line, ""),
        };
        match description_spacing_end(core) {
            Some(spacing_end) if !replaced => {
                output.push_str(&core[..spacing_end]);
                output.push_str(rendered);
                output.push_str(terminator);
                replaced = true;
            }
            _ => output.push_str(line),
        }
        rest = next;
    }
    output
}

/// Byte offset just past the `description:` key and its mandatory spacing, when `line` is that key.
fn description_spacing_end(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("description:")?;
    let spacing = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    (spacing > 0).then(|| "description:".len() + spacing)
}

/// Undo only the files this preflight rewrote, so a refused commit leaves the branch exactly as the
/// agent left it (upstream `GitMemoryRepo.restorePaths`).
fn restore_paths(exec: &dyn GitExec, dir: &Path, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let listing = exec
        .run_in(dir, &["ls-tree", "-r", "--name-only", "HEAD"])
        .map_err(|error| error.to_string())?;
    if listing.code != 0 {
        let stderr = listing.stderr.trim();
        return Err(if stderr.is_empty() {
            "git ls-tree failed".to_string()
        } else {
            stderr.to_string()
        });
    }
    let at_head: Vec<&str> = listing
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .collect();
    let tracked: Vec<&str> = paths
        .iter()
        .map(String::as_str)
        .filter(|path| at_head.contains(path))
        .collect();
    let untracked: Vec<&str> = paths
        .iter()
        .map(String::as_str)
        .filter(|path| !at_head.contains(path))
        .collect();
    if !tracked.is_empty() {
        let mut argv = vec!["restore", "--source=HEAD", "--staged", "--worktree", "--"];
        argv.extend(tracked);
        let restored = exec.run_in(dir, &argv).map_err(|error| error.to_string())?;
        if restored.code != 0 {
            let stderr = restored.stderr.trim();
            return Err(if stderr.is_empty() {
                "git restore failed".to_string()
            } else {
                stderr.to_string()
            });
        }
    }
    for path in untracked {
        let removed = exec
            .run_in(
                dir,
                &["rm", "-q", "--cached", "--ignore-unmatch", "--", path],
            )
            .map_err(|error| error.to_string())?;
        if removed.code != 0 {
            let stderr = removed.stderr.trim();
            return Err(if stderr.is_empty() {
                "git rm failed".to_string()
            } else {
                stderr.to_string()
            });
        }
        let _ = std::fs::remove_file(dir.join(path));
    }
    Ok(())
}

#[cfg(test)]
#[path = "description_preflight_tests.rs"]
mod tests;
