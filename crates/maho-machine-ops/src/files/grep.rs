//! Port of `packages/machine-ops/src/files/grep.ts`.

use crate::errors::{MachineOpError, OpErrorCode};
use crate::types::{AbortSignal, GrepMatch, GrepResult};
use regex::Regex;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

const DEFAULT_LIMIT: usize = 100;
const MAX_LINE_CHARS: usize = 500;
const WALK_SKIP: &[&str] = &[".git", "node_modules"];

fn slice_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Regular-expression search of one file or a directory tree; symlinks are never followed.
pub async fn local_grep(
    root: &Path,
    pattern: &str,
    limit: Option<usize>,
    read_max_bytes: Option<usize>,
    signal: &AbortSignal,
) -> Result<GrepResult, MachineOpError> {
    let regex = Regex::new(pattern).map_err(|err| {
        MachineOpError::new(
            OpErrorCode::InvalidPattern,
            format!("invalid pattern: {err}"),
        )
    })?;
    let effective_limit = limit.unwrap_or(DEFAULT_LIMIT);
    if effective_limit == 0 {
        return Err(MachineOpError::new(
            OpErrorCode::InvalidArgument,
            "limit must be a positive integer",
        ));
    }

    let info = tokio::fs::metadata(root)
        .await
        .map_err(|err| MachineOpError::from_io(&err, Some("stat"), Some(root)))?;

    let mut files: Vec<PathBuf> = Vec::new();
    if info.is_file() {
        if let Some(max_bytes) = read_max_bytes
            && info.len() > max_bytes as u64
        {
            return Err(MachineOpError::new(
                OpErrorCode::LimitExceeded,
                format!(
                    "{} is {} bytes, above the {max_bytes}-byte read limit",
                    root.display(),
                    info.len()
                ),
            ));
        }
        files.push(root.to_path_buf());
    } else {
        let mut pending = VecDeque::new();
        pending.push_back(root.to_path_buf());
        while let Some(dir) = pending.pop_front() {
            let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
                continue;
            };
            while let Ok(Some(dirent)) = rd.next_entry().await {
                let name = dirent.file_name();
                if WALK_SKIP.contains(&name.to_string_lossy().as_ref()) {
                    continue;
                }
                let Ok(ft) = dirent.file_type().await else {
                    continue;
                };
                if ft.is_dir() {
                    pending.push_back(dirent.path());
                } else if ft.is_file() {
                    files.push(dirent.path());
                }
            }
        }
    }

    let mut matches: Vec<GrepMatch> = Vec::new();
    for file in files {
        if signal.is_aborted() {
            return Err(MachineOpError::new(
                OpErrorCode::Aborted,
                "Operation aborted",
            ));
        }
        if file != root
            && let Some(max_bytes) = read_max_bytes
        {
            let Ok(meta) = tokio::fs::metadata(&file).await else {
                continue;
            };
            if meta.len() > max_bytes as u64 {
                continue;
            }
        }
        let Ok(content) = tokio::fs::read(&file).await else {
            if file == root {
                let err = tokio::fs::read(&file)
                    .await
                    .err()
                    .map(|e| MachineOpError::from_io(&e, Some("open"), Some(&file)))
                    .unwrap_or_else(|| MachineOpError::new(OpErrorCode::IoError, "read failed"));
                return Err(err);
            }
            continue;
        };
        if content.contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&content);
        for (index, line) in text.split('\n').enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            if matches.len() >= effective_limit {
                return Ok(GrepResult {
                    matches,
                    truncated: true,
                });
            }
            matches.push(GrepMatch {
                path: file.clone(),
                line: index + 1,
                text: slice_chars(line, MAX_LINE_CHARS),
            });
        }
    }

    Ok(GrepResult {
        matches,
        truncated: false,
    })
}
