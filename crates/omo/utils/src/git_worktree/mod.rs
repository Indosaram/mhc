//! Git porcelain/numstat parsing, diff stat collection and change summaries.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFileStatus {
    Modified,
    Added,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFileStat {
    pub path: String,
    pub added: u64,
    pub removed: u64,
    pub status: GitFileStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedGitStatusPorcelainLine {
    pub file_path: String,
    pub status: GitFileStatus,
}

pub fn parse_git_status_porcelain_line(line: &str) -> Option<ParsedGitStatusPorcelainLine> {
    if line.is_empty() {
        return None;
    }
    let token = line.get(..2).unwrap_or(line).trim();
    let raw_path = line.get(3..).unwrap_or_default();
    let file_path = match token {
        "R" | "C" => raw_path
            .rfind(" -> ")
            .map_or(raw_path, |index| &raw_path[index + 4..]),
        _ => raw_path,
    };
    if file_path.is_empty() {
        return None;
    }
    let status = match token {
        "A" | "??" => GitFileStatus::Added,
        "D" => GitFileStatus::Deleted,
        _ => GitFileStatus::Modified,
    };
    Some(ParsedGitStatusPorcelainLine {
        file_path: file_path.to_string(),
        status,
    })
}

pub fn parse_git_status_porcelain(output: &str) -> HashMap<String, GitFileStatus> {
    output
        .split('\n')
        .filter_map(parse_git_status_porcelain_line)
        .map(|parsed| (parsed.file_path, parsed.status))
        .collect()
}

fn parse_count(text: &str) -> u64 {
    if text == "-" {
        return 0;
    }
    let digits: String = text
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or(0)
}

pub fn parse_git_diff_numstat(
    output: &str,
    status_map: &HashMap<String, GitFileStatus>,
) -> Vec<GitFileStat> {
    output
        .split('\n')
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let (added, removed, path) = (parts.next()?, parts.next()?, parts.next()?);
            Some(GitFileStat {
                path: path.to_string(),
                added: parse_count(added),
                removed: parse_count(removed),
                status: status_map
                    .get(path)
                    .copied()
                    .unwrap_or(GitFileStatus::Modified),
            })
        })
        .collect()
}

fn git_output(directory: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string()
    })
}

/// Numstat for tracked changes plus line counts for untracked files; empty on any git failure.
/// Arguments are passed as an argv array, so the directory never reaches a shell.
pub fn collect_git_diff_stats(directory: impl AsRef<Path>) -> Vec<GitFileStat> {
    let directory = directory.as_ref();
    let (Some(diff), Some(status), Some(untracked)) = (
        git_output(directory, &["diff", "--numstat", "HEAD"]),
        git_output(directory, &["status", "--porcelain"]),
        git_output(directory, &["ls-files", "--others", "--exclude-standard"]),
    ) else {
        return Vec::new();
    };
    let untracked_numstat = untracked
        .split('\n')
        .filter(|path| !path.is_empty())
        .map(|path| {
            let lines = std::fs::read_to_string(directory.join(path)).map_or(0, |content| {
                content.split('\n').count() - usize::from(content.ends_with('\n'))
            });
            format!("{lines}\t0\t{path}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let combined = [diff, untracked_numstat]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let combined = combined.trim();
    if combined.is_empty() {
        return Vec::new();
    }
    parse_git_diff_numstat(combined, &parse_git_status_porcelain(&status))
}

pub fn format_file_changes(stats: &[GitFileStat], notepad_path: Option<&str>) -> String {
    if stats.is_empty() {
        return "[FILE CHANGES SUMMARY]\nNo file changes detected.\n".to_string();
    }
    let mut lines = vec!["[FILE CHANGES SUMMARY]".to_string()];
    let sections: [(GitFileStatus, &str); 3] = [
        (GitFileStatus::Modified, "Modified files:"),
        (GitFileStatus::Added, "Created files:"),
        (GitFileStatus::Deleted, "Deleted files:"),
    ];
    for (status, heading) in sections {
        let matching: Vec<&GitFileStat> =
            stats.iter().filter(|stat| stat.status == status).collect();
        if matching.is_empty() {
            continue;
        }
        lines.push(heading.to_string());
        for stat in matching {
            let detail = match status {
                GitFileStatus::Modified => format!("(+{}, -{})", stat.added, stat.removed),
                GitFileStatus::Added => format!("(+{})", stat.added),
                GitFileStatus::Deleted => format!("(-{})", stat.removed),
            };
            lines.push(format!("  {}  {detail}", stat.path));
        }
        lines.push(String::new());
    }
    if let Some(notepad) = notepad_path.filter(|path| !path.is_empty()) {
        let normalized = notepad.replace('\\', "/");
        if let Some(stat) = stats
            .iter()
            .find(|stat| stat.path.replace('\\', "/") == normalized)
        {
            lines.push("[NOTEPAD UPDATED]".to_string());
            lines.push(format!("  {}  (+{})", stat.path, stat.added));
            lines.push(String::new());
        }
    }
    lines.join("\n")
}
