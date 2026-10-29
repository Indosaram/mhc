//! Expands `@dir/file.ext` references in skill content to absolute paths under the skill base.

use std::sync::LazyLock;

use fancy_regex::{Captures, Regex};

use crate::contains_path::lexical_normalize;

static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?<![a-zA-Z0-9="\(])@([a-zA-Z0-9_-]+/[a-zA-Z0-9_.\-/]*)"#)
        .unwrap_or_else(|error| panic!("{error}"))
});
static FILE_EXTENSION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\.[a-zA-Z0-9]+$").unwrap_or_else(|error| panic!("{error}"))
});

fn looks_like_file_path(path: &str) -> bool {
    path.ends_with('/') || FILE_EXTENSION.is_match(path.rsplit('/').next().unwrap_or_default())
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

fn join_contained(base: &str, relative: &str) -> Option<String> {
    let separator_normalized = base.replace('\\', "/");
    let root = lexical_normalize(std::path::Path::new(&separator_normalized));
    let resolved = lexical_normalize(&root.join(relative));
    resolved
        .starts_with(&root)
        .then(|| resolved.to_string_lossy().replace('\\', "/"))
}

fn join_windows(base: &str, relative: &str) -> Option<String> {
    let mut segments: Vec<String> = base
        .replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let root_len = segments.len();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.len() <= root_len {
                    return None;
                }
                segments.pop();
            }
            other => segments.push(other.to_string()),
        }
    }
    Some(segments.join("/"))
}

pub fn resolve_skill_path_references(content: &str, base_path: &str) -> String {
    let base = base_path.strip_suffix(['/', '\\']).unwrap_or(base_path);
    REFERENCE
        .replace_all(content, |captures: &Captures<'_>| {
            let whole = captures.get(0).map_or("", |m| m.as_str()).to_string();
            let relative = captures.get(1).map_or("", |m| m.as_str());
            if !looks_like_file_path(relative) {
                return whole;
            }
            let resolved = if is_windows_absolute(base) {
                join_windows(base, relative)
            } else if base.starts_with('/') {
                join_contained(base, relative)
            } else {
                let absolute = std::env::current_dir().unwrap_or_default().join(base);
                join_contained(&absolute.to_string_lossy(), relative)
            };
            match resolved {
                Some(path) if relative.ends_with('/') && !path.ends_with('/') => format!("{path}/"),
                Some(path) => path,
                None => whole,
            }
        })
        .into_owned()
}
