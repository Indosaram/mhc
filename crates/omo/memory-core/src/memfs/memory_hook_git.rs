//! Git operations and raw diff inspection for memory commit hooks.

use std::path::{Path, PathBuf};

/// A single staged file entry parsed from raw NUL-delimited git diff output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedEntry {
    pub status: String,
    pub path: String,
    pub from: Option<String>,
    pub src_blob: String,
    pub dst_blob: String,
    pub dst_mode: String,
}

/// Execute a git command with `--literal-pathspecs` and disabled glob/icase modes.
pub fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("--literal-pathspecs")
        .args(args)
        .current_dir(cwd)
        .env("GIT_GLOB_PATHSPECS", "0")
        .env("GIT_ICASE_PATHSPECS", "0");

    let output = cmd.output().ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        None
    }
}

/// Parse `git diff --raw -z` output into structured staged entries.
pub fn parse_raw_z(output: &str) -> Vec<StagedEntry> {
    let tokens: Vec<&str> = output.split('\0').collect();
    let mut entries = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let meta = tokens[index];
        index += 1;
        if !meta.starts_with(':') {
            break;
        }
        let parts: Vec<&str> = meta[1..].split(' ').collect();
        let dst_mode = parts.get(1).copied().unwrap_or("").to_string();
        let src_blob = parts.get(2).copied().unwrap_or("").to_string();
        let dst_blob = parts.get(3).copied().unwrap_or("").to_string();
        let status = parts.get(4).copied().unwrap_or("").to_string();

        let first = if index < tokens.len() {
            let s = tokens[index];
            index += 1;
            s
        } else {
            ""
        };

        if status.starts_with('R') || status.starts_with('C') {
            let path = if index < tokens.len() {
                let s = tokens[index];
                index += 1;
                s
            } else {
                ""
            };
            entries.push(StagedEntry {
                status,
                path: path.to_string(),
                from: Some(first.to_string()),
                src_blob,
                dst_blob,
                dst_mode,
            });
        } else {
            entries.push(StagedEntry {
                status,
                path: first.to_string(),
                from: None,
                src_blob,
                dst_blob,
                dst_mode,
            });
        }
    }
    entries
}

/// Return structured representations of all staged changes in `cwd`.
pub fn staged_entries(cwd: &Path) -> Result<Vec<StagedEntry>, String> {
    let listed = git(
        cwd,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--cached",
            "-M",
            "-C",
            "-z",
            "--raw",
            "--no-abbrev",
        ],
    )
    .ok_or_else(|| "memory hook: could not list the staged paths".to_string())?;
    Ok(parse_raw_z(&listed))
}

/// Read object content by object ID.
pub fn read_blob(cwd: &Path, oid: Option<&str>) -> Option<String> {
    let oid = oid?;
    if oid.is_empty() || oid.chars().all(|c| c == '0') {
        return None;
    }
    git(cwd, &["cat-file", "blob", oid])
}

/// Read blob object ID for a specific path at a given tree revision.
pub fn tree_blob(cwd: &Path, rev: &str, path: &str) -> Option<String> {
    let listed = git(cwd, &["ls-tree", "-z", rev, "--", path])?;
    let tab_idx = listed.find('\t')?;
    let prefix = &listed[..tab_idx];
    let parts: Vec<&str> = prefix.split_whitespace().collect();
    if parts.len() == 3 && parts[1] == "blob" {
        Some(parts[2].to_string())
    } else {
        None
    }
}

/// Return git commit hashes of any active MERGE_HEAD.
pub fn merge_heads(cwd: &Path) -> Vec<String> {
    let path = match git(cwd, &["rev-parse", "--git-path", "MERGE_HEAD"]) {
        Some(p) => p.trim().to_string(),
        None => return Vec::new(),
    };
    if path.is_empty() {
        return Vec::new();
    }
    let abs_path = if Path::new(&path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };
    std::fs::read_to_string(&abs_path)
        .map(|content| {
            content
                .lines()
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect()
        })
        .unwrap_or_default()
}
