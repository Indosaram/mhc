//! Porcelain status output parser and markdown encoding diagnostics.

use std::fs;
use std::path::Path;

/// Inspects dirty markdown files from porcelain status and returns readable encoding issues.
pub fn describe_dirty_markdown_encoding_issues(root: &Path, porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .filter_map(parse_porcelain_path)
        .filter(|path| path.ends_with(".md"))
        .filter_map(|path| describe_markdown_encoding_issue(root, &path))
        .collect()
}

/// Parses the path from a Git porcelain status line.
pub fn parse_porcelain_path(line: &str) -> Option<String> {
    if line.len() < 4 {
        return None;
    }
    let status = &line[0..2];
    if status == " D" || status == "D " || status == "DD" {
        return None;
    }

    let raw_path = &line[3..];
    let path = if let Some((_, right)) = raw_path.split_once(" -> ") {
        right
    } else {
        raw_path
    };
    let trimmed = path.trim_matches('"');
    Some(trimmed.to_string())
}

fn describe_markdown_encoding_issue(root: &Path, relative_path: &str) -> Option<String> {
    let full = root.join(relative_path);
    let bytes = fs::read(&full).ok()?;

    if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xfe {
        return Some(format!("{relative_path} has UTF-16 LE BOM"));
    }
    if bytes.len() >= 2 && bytes[0] == 0xfe && bytes[1] == 0xff {
        return Some(format!("{relative_path} has UTF-16 BE BOM"));
    }
    if bytes.contains(&0) {
        return Some(format!(
            "{relative_path} contains NUL bytes, possibly UTF-16"
        ));
    }
    None
}
