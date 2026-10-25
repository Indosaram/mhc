//! Staged-file and commit-message inspection for claim receipt and frontmatter validation.

use std::path::Path;

use crate::memfs::frontmatter::parse_memory_file;
use crate::memfs::frontmatter_entries::{
    content_frontmatter_entries, duplicate_frontmatter_keys, look_alike_frontmatter_keys,
};
use crate::memfs::memory_hook_git::{
    StagedEntry, merge_heads, read_blob, staged_entries, tree_blob,
};
use crate::tools::claim_frontmatter::{PROVENANCE_FRONTMATTER_KEY, stamp_receipt};

pub use crate::memfs::memory_hook_git::parse_raw_z;

const RECEIPT_REFUSAL: &str =
    "the provenance header is a claim receipt only the memory tool writes; remove that line (or save the note with the memory tool)";

fn is_regular_file_mode(mode: &str) -> bool {
    mode == "100644" || mode == "100755"
}

fn git_quoted_path(path: &str) -> bool {
    path.chars().any(|c| {
        c == '"' || c == '\\' || (c as u32) <= 0x1f || (c as u32) == 0x7f
    })
}

fn has_memory_writer_trailer(message: &str) -> bool {
    message
        .lines()
        .any(|line| line.trim() == "Omo-Writer: memory-tool")
}

fn look_alike_failures(path: &str, staged: &str) -> Vec<String> {
    look_alike_frontmatter_keys(staged, PROVENANCE_FRONTMATTER_KEY)
        .into_iter()
        .map(|key| {
            format!(
                "  {path}: frontmatter key '{key}' reads as '{PROVENANCE_FRONTMATTER_KEY}'; only the memory tool writes that key, spelled exactly"
            )
        })
        .collect()
}

/// The value the reader would take for the header: the last `provenance` entry of the frontmatter.
pub fn provenance_of(content: Option<&str>) -> Option<String> {
    let content = content?;
    let entries = content_frontmatter_entries(content)?;
    entries
        .into_iter().rfind(|entry| entry.key == PROVENANCE_FRONTMATTER_KEY)
        .map(|entry| entry.value)
}

/// Run pre-commit checks against staged entries.
pub fn pre_commit_failures(cwd: &Path, entries: Option<&[StagedEntry]>) -> Vec<String> {
    let owned_entries;
    let entries = match entries {
        Some(e) => e,
        None => match staged_entries(cwd) {
            Ok(e) => {
                owned_entries = e;
                &owned_entries
            }
            Err(err) => return vec![format!("  {err}")],
        },
    };

    let mut failures = Vec::new();
    for entry in entries {
        if git_quoted_path(&entry.path) {
            failures.push(format!(
                "  {:?}: memory file names cannot hold quotes, backslashes or control characters; rename it",
                entry.path
            ));
            continue;
        }
        if entry.status.starts_with('D') {
            continue;
        }
        if !is_regular_file_mode(&entry.dst_mode) {
            failures.push(format!(
                "  {}: a memory file must be a regular file (not a symbolic link, submodule or unknown type, git status {}); commit the file itself",
                entry.path, entry.status
            ));
            continue;
        }
        let staged = match read_blob(cwd, Some(&entry.dst_blob)) {
            Some(s) => s,
            None => {
                failures.push(format!("  {}: the staged file could not be read", entry.path));
                continue;
            }
        };
        failures.extend(look_alike_failures(&entry.path, &staged));
        if !entry.path.ends_with(".md") && provenance_of(Some(&staged)).is_none() {
            continue;
        }
        for key in duplicate_frontmatter_keys(&staged) {
            failures.push(format!(
                "  {}: duplicate frontmatter key '{key}'; keep one",
                entry.path
            ));
        }
    }
    failures
}

/// Run commit-msg checks against staged entries and the commit message.
pub fn commit_message_failures(
    cwd: &Path,
    message: &str,
    entries: Option<&[StagedEntry]>,
) -> Vec<String> {
    let owned_entries;
    let entries = match entries {
        Some(e) => e,
        None => match staged_entries(cwd) {
            Ok(e) => {
                owned_entries = e;
                &owned_entries
            }
            Err(err) => return vec![format!("  {err}")],
        },
    };

    if has_memory_writer_trailer(message) {
        let mut failures = Vec::new();
        for entry in entries {
            if entry.status.starts_with('D') || !is_regular_file_mode(&entry.dst_mode) {
                continue;
            }
            match read_blob(cwd, Some(&entry.dst_blob)) {
                Some(staged) => {
                    failures.extend(look_alike_failures(&entry.path, &staged));
                }
                None => {
                    failures.push(format!("  {}: the staged file could not be read", entry.path));
                }
            }
        }
        return failures;
    }

    let heads = merge_heads(cwd);
    let mut failures = Vec::new();
    for entry in entries {
        if entry.status.starts_with('D') {
            continue;
        }
        if !is_regular_file_mode(&entry.dst_mode) {
            failures.push(format!(
                "  {}: a memory file must be a regular file (not a symbolic link, submodule or unknown type, git status {})",
                entry.path, entry.status
            ));
            continue;
        }
        let staged = match read_blob(cwd, Some(&entry.dst_blob)) {
            Some(s) => s,
            None => {
                failures.push(format!("  {}: the staged file could not be read", entry.path));
                continue;
            }
        };
        failures.extend(look_alike_failures(&entry.path, &staged));
        let header = provenance_of(Some(&staged));
        if let Some(h) = header
            && !hand_commit_keeps(cwd, entry, &staged, &h, &heads) {
                failures.push(format!("  {}: {RECEIPT_REFUSAL}", entry.path));
            }
    }
    failures
}

fn hand_commit_keeps(
    cwd: &Path,
    entry: &StagedEntry,
    staged: &str,
    header: &str,
    heads: &[String],
) -> bool {
    let ours = if entry.status.starts_with('M') || entry.status.starts_with('R') {
        read_blob(cwd, Some(&entry.src_blob))
    } else {
        None
    };

    let theirs: Vec<Option<String>> = heads
        .iter()
        .map(|head| {
            let blob_oid = tree_blob(cwd, head, &entry.path);
            read_blob(cwd, blob_oid.as_deref())
        })
        .collect();

    let mut parents = Vec::new();
    if let Some(o) = ours {
        parents.push(o);
    }
    for t in theirs.into_iter().flatten() {
        parents.push(t);
    }

    parents.iter().any(|parent| {
        parent == staged || lowered(parent, staged).as_deref() == Some(header)
    })
}

fn lowered(parent: &str, staged: &str) -> Option<String> {
    let parent_note = parse_memory_file(parent).ok()?;
    let next = parse_memory_file(staged).ok()?;
    let stamped = stamp_receipt(Some(&parent_note), &next.frontmatter, &next.body, None);
    stamped
        .extra
        .and_then(|extra| extra.get(PROVENANCE_FRONTMATTER_KEY).cloned())
}

/// Entry point for running memory hooks in the repository root.
pub fn run_memory_hook(args: &[String], cwd: &Path) -> i32 {
    let stage = args.first().map(String::as_str).unwrap_or("");
    let message_file = args.get(1).map(String::as_str);

    let failures = if stage == "pre-commit" {
        pre_commit_failures(cwd, None)
    } else if stage == "commit-msg" && message_file.is_some() {
        let msg_file = message_file.unwrap();
        match std::fs::read_to_string(msg_file) {
            Ok(content) => commit_message_failures(cwd, &content, None),
            Err(e) => vec![format!("  could not read commit message file {msg_file}: {e}")],
        }
    } else {
        vec![format!("  memory hook: unknown stage {stage:?}")]
    };

    if failures.is_empty() {
        return 0;
    }

    let header = if stage == "commit-msg" {
        "Claim receipt validation failed:"
    } else {
        "Frontmatter validation failed:"
    };

    println!("{header}\n{}", failures.join("\n"));
    1
}
