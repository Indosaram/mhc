use std::path::Path;

use crate::backend::Result;
use crate::git::command::{run_git, str_args, GitOptions};
use crate::util::mkdtemp;

pub fn write_synthetic_tree(
    object_repo_dir: &Path,
    head_commit: &str,
    patches: &[String],
) -> Result<String> {
    let dir = mkdtemp("isolation-index-")?;
    let index_file = dir.join("index");
    let mut options = GitOptions::new(object_repo_dir.to_path_buf());
    options
        .env
        .push(("GIT_INDEX_FILE".to_string(), index_file.to_string_lossy().into_owned()));
    let result = (|| {
        let head = if head_commit.is_empty() {
            "--empty"
        } else {
            head_commit
        };
        run_git(&str_args(&["read-tree", head]), &options)?;
        for patch in patches {
            if patch.trim().is_empty() {
                continue;
            }
            let mut apply_options = options.clone();
            apply_options.input = Some(patch.clone().into_bytes());
            run_git(
                &str_args(&[
                    "apply",
                    "--cached",
                    "--binary",
                    "--whitespace=nowarn",
                    "-",
                ]),
                &apply_options,
            )?;
        }
        let output = run_git(&str_args(&["write-tree"]), &options)?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn find_closing_quote(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return Some(index),
            _ => index += 1,
        }
    }
    None
}

fn take_trailing_token(text: &str) -> Option<String> {
    if text.starts_with('"') {
        let end = find_closing_quote(text)?;
        if end + 1 == text.len() {
            return Some(text.to_string());
        }
        return None;
    }
    if text.starts_with("b/") {
        return Some(text.to_string());
    }
    None
}

fn split_diff_paths(rest: &str) -> Option<(String, String)> {
    if rest.starts_with('"')
        && let Some(end) = find_closing_quote(rest)
        && rest.as_bytes().get(end + 1) == Some(&b' ')
        && let Some(second) = take_trailing_token(&rest[end + 2..])
    {
        return Some((rest[..end + 1].to_string(), second));
    }
    for (index, _) in rest.match_indices(' ') {
        let first = &rest[..index];
        if !first.starts_with("a/") {
            continue;
        }
        if let Some(second) = take_trailing_token(&rest[index + 1..]) {
            return Some((first.to_string(), second));
        }
    }
    None
}

/// Git quotes UTF-8 bytes with C escapes (including octal), not JSON escapes.
pub fn unquote_git_diff_path(raw_path: &str) -> String {
    let mut value = raw_path.to_string();
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        let body = &value[1..value.len() - 1];
        let bytes = body.as_bytes();
        let mut out: Vec<u8> = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'\\' {
                let digits: String = body[index + 1..]
                    .chars()
                    .take(3)
                    .take_while(|c| c.is_ascii_digit() && *c < '8')
                    .collect();
                if !digits.is_empty() {
                    if let Ok(parsed) = u32::from_str_radix(&digits, 8) {
                        out.push(parsed as u8);
                    }
                    index += digits.len() + 1;
                    continue;
                }
                let escaped = bytes.get(index + 1).copied().unwrap_or(0);
                let mapped = match escaped {
                    b'a' => 7,
                    b'b' => 8,
                    b't' => 9,
                    b'n' => 10,
                    b'v' => 11,
                    b'f' => 12,
                    b'r' => 13,
                    b'"' => 34,
                    b'\\' => 92,
                    other => other,
                };
                out.push(mapped);
                index += 2;
                continue;
            }
            let ch = body[index..].chars().next().unwrap_or('\u{0}');
            let mut buffer = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            index += ch.len_utf8();
        }
        value = String::from_utf8_lossy(&out).into_owned();
    }
    if let Some(stripped) = value.strip_prefix("a/") {
        return stripped.to_string();
    }
    if let Some(stripped) = value.strip_prefix("b/") {
        return stripped.to_string();
    }
    value
}

pub fn parse_diff_git_line_paths(line: &str) -> Vec<String> {
    let Some(rest) = line.strip_prefix("diff --git ") else {
        return Vec::new();
    };
    let Some((first, second)) = split_diff_paths(rest) else {
        return Vec::new();
    };
    let mut paths: Vec<String> = Vec::new();
    for candidate in [first, second] {
        let path = unquote_git_diff_path(&candidate);
        if !path.is_empty() && path != "/dev/null" && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}
