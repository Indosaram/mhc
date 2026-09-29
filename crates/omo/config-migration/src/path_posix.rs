//! Port of Node's `path.posix` operations used by discovery.

pub(crate) fn normalize_segments(
    tail: &str,
    allow_above_root: bool,
    is_sep: fn(char) -> bool,
) -> Vec<&str> {
    let mut segments: Vec<&str> = Vec::new();
    for segment in tail.split(is_sep) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.last().is_some_and(|last| *last != "..") {
                    segments.pop();
                } else if allow_above_root {
                    segments.push("..");
                }
            }
            other => segments.push(other),
        }
    }
    segments
}

fn is_sep(character: char) -> bool {
    character == '/'
}

pub fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

pub fn normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let absolute = is_absolute(path);
    let trailing = path.ends_with('/');
    let mut tail = normalize_segments(path, !absolute, is_sep).join("/");
    if tail.is_empty() && !absolute {
        tail = ".".to_string();
    }
    if !tail.is_empty() && trailing {
        tail.push('/');
    }
    if absolute { format!("/{tail}") } else { tail }
}

pub fn join(paths: &[&str]) -> String {
    let joined = paths
        .iter()
        .filter(|path| !path.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    if joined.is_empty() {
        return ".".to_string();
    }
    normalize(&joined)
}

pub fn dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let bytes = path.as_bytes();
    let has_root = bytes[0] == b'/';
    let mut end: Option<usize> = None;
    let mut matched_slash = true;
    for index in (1..bytes.len()).rev() {
        if bytes[index] == b'/' {
            if !matched_slash {
                end = Some(index);
                break;
            }
        } else {
            matched_slash = false;
        }
    }
    match end {
        None if has_root => "/".to_string(),
        None => ".".to_string(),
        Some(1) if has_root => "//".to_string(),
        Some(end) => path[..end].to_string(),
    }
}

pub fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

fn current_directory() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string())
}

pub fn resolve(paths: &[&str]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for path in paths.iter().rev().filter(|path| !path.is_empty()) {
        parts.push((*path).to_string());
        if is_absolute(path) {
            break;
        }
    }
    if !parts.last().is_some_and(|path| is_absolute(path)) {
        parts.push(current_directory());
    }
    parts.reverse();
    let joined = parts.join("/");
    let tail = normalize_segments(&joined, false, is_sep).join("/");
    format!("/{tail}")
}

pub fn relative(from: &str, to: &str) -> String {
    let from = resolve(&[from]);
    let to = resolve(&[to]);
    if from == to {
        return String::new();
    }
    let from_segments: Vec<&str> = from
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let to_segments: Vec<&str> = to
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let common = from_segments
        .iter()
        .zip(&to_segments)
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts: Vec<&str> = vec![".."; from_segments.len() - common];
    parts.extend(&to_segments[common..]);
    parts.join("/")
}
