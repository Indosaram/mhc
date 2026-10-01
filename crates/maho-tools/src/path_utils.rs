use std::path::{Component, Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub fn expand_path(path: &str) -> PathBuf {
    let normalized: String = path.chars().map(|c| match c {
        '\u{a0}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' => ' ',
        _ => c,
    }).collect();
    let normalized = normalized.strip_prefix('@').unwrap_or(&normalized);
    if normalized == "~" || normalized.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(normalized.strip_prefix("~/").unwrap_or(""));
        }
    }
    PathBuf::from(normalized)
}
pub fn resolve_to_cwd(path: &str, cwd: &Path) -> PathBuf {
    let path = expand_path(path);
    let joined = cwd.join(path);
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {},
            Component::ParentDir => { result.pop(); },
            component => result.push(component.as_os_str()),
        }
    }
    result
}
pub async fn path_exists(path: &Path) -> bool { tokio::fs::metadata(path).await.is_ok() }
pub async fn resolve_read_path_async(path: &str, cwd: &Path) -> PathBuf {
    let resolved = resolve_to_cwd(path, cwd);
    if path_exists(&resolved).await { return resolved; }
    let text = resolved.to_string_lossy();
    let mut ampm = text.to_string();
    for marker in ["AM", "PM", "am", "pm", "Am", "Pm", "aM", "pM"] {
        ampm = ampm.replace(&format!(" {marker}."), &format!("\u{202f}{marker}."));
    }
    let nfd = text.nfd().collect::<String>();
    for candidate in [ampm, nfd.clone(), text.replace('\'', "\u{2019}"), nfd.replace('\'', "\u{2019}")] {
        let candidate = PathBuf::from(candidate);
        if candidate != resolved && path_exists(&candidate).await { return candidate; }
    }
    resolved
}

/// Component traversal with lstat/readlink only, used after the realpath deadline.
pub fn realpath_without_open_strict(path: &Path) -> std::io::Result<PathBuf> {
    use std::collections::VecDeque;
    let mut pending: VecDeque<_> = path.components().map(|c| c.as_os_str().to_owned()).collect();
    let mut resolved = PathBuf::new();
    let mut hops = 0;
    while let Some(part) = pending.pop_front() {
        if part == "." { continue; }
        if part == ".." { resolved.pop(); continue; }
        let candidate = resolved.join(&part);
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.is_symlink() => {
                hops += 1;
                if hops > 40 { return Err(std::io::Error::from_raw_os_error(40)); }
                let target = std::fs::read_link(&candidate)?;
                if target.is_absolute() { resolved.clear(); }
                let parts: Vec<_> = target.components().map(|c| c.as_os_str().to_owned()).collect();
                for component in parts.into_iter().rev() { pending.push_front(component); }
            }
            Ok(_) => resolved.push(part),
            Err(error) if crate::bounded_realpath::is_missing_path_error(&error) => {
                resolved.push(part);
                for part in pending { if part == ".." { resolved.pop(); } else if part != "." { resolved.push(part); } }
                return Ok(resolved);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(resolved)
}
