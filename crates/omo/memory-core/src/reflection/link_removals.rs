//! Detects removed, renamed, and archived notes between `base_sha` and `tip_sha` (#12642, #12645).

use std::collections::{BTreeMap, BTreeSet};

use super::completion_repair::LiteralPathspecRunner;
use super::link_rewrite::{ARCHIVE_PATH, Successor, posix_basename};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: String,
    pub regular: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Removals {
    pub removed: BTreeMap<String, Successor>,
    /// Rename destination -> source so a moved note's own relative links resolve from the old folder.
    pub moved_from: BTreeMap<String, String>,
}

pub fn must(run: &LiteralPathspecRunner<'_>, argv: &[&str]) -> Result<String, String> {
    let result = run.call(argv)?;
    if result.code != 0 {
        let stderr = result.stderr.trim();
        return Err(if stderr.is_empty() {
            format!("git {} failed", argv.join(" "))
        } else {
            stderr.to_owned()
        });
    }
    Ok(result.stdout)
}

pub fn list_tree(
    run: &LiteralPathspecRunner<'_>,
    revision: &str,
) -> Result<Vec<TreeEntry>, String> {
    let out = must(run, &["ls-tree", "-r", "-z", "--full-tree", revision])?;
    let mut entries = Vec::new();
    for line in out.split('\0').filter(|s| !s.is_empty()) {
        let Some((meta, path)) = line.split_once('\t') else {
            continue;
        };
        let mode = meta.split(' ').next().unwrap_or("");
        entries.push(TreeEntry {
            path: path.to_owned(),
            regular: mode == "100644" || mode == "100755",
        });
    }
    Ok(entries)
}

/// Every path `base_sha..tip_sha` removed, with where its links go: rename destination, archive, or nowhere.
pub fn removed_paths(
    run: &LiteralPathspecRunner<'_>,
    base_sha: &str,
    tip_sha: &str,
    live: &BTreeSet<String>,
) -> Result<Removals, String> {
    let range = format!("{base_sha}..{tip_sha}");
    let raw = must(run, &["diff", "--name-status", "-M", "-z", &range, "--"])?;
    let parts: Vec<&str> = raw.split('\0').collect();
    let mut deleted = Vec::new();
    let mut removed = BTreeMap::new();
    let mut moved_from = BTreeMap::new();

    let mut index = 0usize;
    while index < parts.len() {
        let status = parts[index];
        if status.is_empty() {
            break;
        }
        if status.starts_with('R') || status.starts_with('C') {
            if status.starts_with('R')
                && let (Some(&from), Some(&to)) = (parts.get(index + 1), parts.get(index + 2))
                && !live.contains(from)
            {
                removed.insert(
                    from.to_owned(),
                    Successor::Moved {
                        path: to.to_owned(),
                    },
                );
                moved_from.insert(to.to_owned(), from.to_owned());
            }
            index += 3;
            continue;
        }
        if status == "D"
            && let Some(&path) = parts.get(index + 1)
            && !live.contains(path)
        {
            deleted.push(path.to_owned());
        }
        index += 2;
    }

    let archived = if live.contains(ARCHIVE_PATH) {
        archive_additions(run, base_sha, tip_sha)?
    } else {
        String::new()
    };

    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    for path in live.iter().chain(deleted.iter()) {
        *names.entry(posix_basename(path).to_owned()).or_insert(0) += 1;
    }

    for path in deleted {
        let unique = names.get(posix_basename(&path)).copied() == Some(1);
        let successor = if recorded_in(&archived, &path, unique) {
            Successor::Archived
        } else {
            Successor::Removed
        };
        removed.insert(path, successor);
    }

    Ok(Removals {
        removed,
        moved_from,
    })
}

fn archive_additions(
    run: &LiteralPathspecRunner<'_>,
    base_sha: &str,
    tip_sha: &str,
) -> Result<String, String> {
    let out = must(
        run,
        &[
            "diff",
            "-U0",
            "--no-color",
            base_sha,
            tip_sha,
            "--",
            ARCHIVE_PATH,
        ],
    )?;
    let lines: Vec<&str> = out
        .split('\n')
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .collect();
    Ok(lines.join("\n"))
}

fn is_word_or_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/')
}

fn is_end_boundary(rest: &str) -> bool {
    let Some(next) = rest.chars().next() else {
        return true;
    };
    if next.is_ascii_alphanumeric() || matches!(next, '_' | '/' | '-') {
        return false;
    }
    if next == '.' {
        return !rest[1..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    }
    true
}

fn contains_token(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut offset = 0usize;
    while let Some(pos) = haystack[offset..].find(needle) {
        let abs = offset + pos;
        let start_ok = haystack[..abs]
            .chars()
            .next_back()
            .is_none_or(|prev| !is_word_or_path_char(prev));
        let end = abs + needle.len();
        if start_ok && is_end_boundary(&haystack[end..]) {
            return true;
        }
        offset = abs + 1;
    }
    false
}

/// Whether lines added to `ARCHIVE.md` name `path` (repo path with/without `.md`, or unique bare filename `.md`).
pub fn recorded_in(archived: &str, path: &str, unique_name: bool) -> bool {
    let stem = path.strip_suffix(".md").unwrap_or(path);
    if contains_token(archived, path) || contains_token(archived, stem) {
        return true;
    }
    unique_name && path.ends_with(".md") && contains_token(archived, posix_basename(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_link_matching_respects_boundaries_and_uniqueness() {
        assert!(recorded_in(
            "+ Archived notes/foo.md",
            "notes/foo.md",
            false
        ));
        assert!(recorded_in(
            "+ Archived notes/foo here",
            "notes/foo.md",
            false
        ));
        assert!(!recorded_in(
            "+ Archived notes/foo.mdx",
            "notes/foo.md",
            true
        ));
        assert!(recorded_in("+ Archived foo.md", "notes/foo.md", true));
        assert!(!recorded_in("+ Archived foo.md", "notes/foo.md", false));
    }
}
