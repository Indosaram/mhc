use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

pub fn expand_home(input: &str, home: &Path) -> PathBuf {
    if input == "~" || input == "$HOME" {
        return home.to_owned();
    }
    for prefix in ["~/", "~\\", "$HOME/", "$HOME\\"] {
        if let Some(rest) = input.strip_prefix(prefix) {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}
pub fn to_parent_directory_pattern(input: &str, directory: bool) -> String {
    to_parent_directory_pattern_from(input, directory, input)
}
/// `toParentDirectoryPattern(target, scope, originalPath)`: a raw path written with a trailing
/// separator names a directory, so its "Allow always" scope is the directory (senpi#4311).
pub fn to_parent_directory_pattern_from(input: &str, directory: bool, original: &str) -> String {
    if input == "~" || input == "$HOME" {
        return format!("{input}/*");
    }
    let trailing_separator = input.ends_with('/') || input.ends_with('\\');
    if directory || original.ends_with('/') || original.ends_with('\\') {
        return if trailing_separator {
            format!("{input}*")
        } else {
            format!("{input}/*")
        };
    }
    if trailing_separator {
        return format!("{input}*");
    }
    // `inputPath.replace(/[\\/][^\\/]+$/, "/*")`; a parent that is the filesystem root keeps the input.
    match input.rfind(['/', '\\']) {
        Some(index) if index + 1 < input.len() => {
            let parent = format!("{}/*", &input[..index]);
            let root = if input.starts_with('/') || input.starts_with('\\') {
                "/"
            } else {
                ""
            };
            if parent.replace('\\', "/") == format!("{root}*") {
                input.into()
            } else {
                parent
            }
        }
        _ => input.into(),
    }
}
/// `path.resolve(cwd, path)`: join and normalize `.`/`..` without touching the filesystem.
pub fn lexical_resolve(cwd: &Path, path: &Path) -> PathBuf {
    normalize(&cwd.join(path))
}
fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                result.pop();
            }
            component => result.push(component.as_os_str()),
        }
    }
    result
}
pub fn split_link_target(link: &Path) -> (Option<PathBuf>, Vec<String>) {
    let mut parts: VecDeque<String> = link
        .to_string_lossy()
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    let root = if link.is_absolute() || link.to_string_lossy().starts_with(['/', '\\']) {
        if link.has_root() && !link.to_string_lossy().starts_with(['/', '\\']) {
            let prefix = parts.pop_front().unwrap_or_default();
            Some(PathBuf::from(format!("{prefix}{}", std::path::MAIN_SEPARATOR)))
        } else {
            Some(PathBuf::from(std::path::MAIN_SEPARATOR.to_string()))
        }
    } else {
        None
    };
    (root, parts.into())
}
pub fn resolve_without_open(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut pending: VecDeque<String> = absolute
        .to_string_lossy()
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    let mut resolved =
        if absolute.has_root() && !absolute.to_string_lossy().starts_with(['/', '\\']) {
            let prefix = pending.pop_front().unwrap_or_default();
            PathBuf::from(format!("{prefix}{}", std::path::MAIN_SEPARATOR))
        } else if absolute.has_root() || absolute.to_string_lossy().starts_with(['/', '\\']) {
            PathBuf::from(std::path::MAIN_SEPARATOR.to_string())
        } else {
            PathBuf::new()
        };
    let mut hops = 0;
    while let Some(part) = pending.pop_front() {
        if part == "." {
            continue;
        }
        if part == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&part);
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.file_type().is_symlink() => {
                hops += 1;
                if hops > 40 {
                    return normalize(
                        &pending.iter().fold(candidate, |path, part| path.join(part)),
                    );
                }
                match std::fs::read_link(&candidate) {
                    Ok(link) => {
                        let (root, parts) = split_link_target(&link);
                        if let Some(root) = root {
                            resolved = root;
                        }
                        for part in parts.into_iter().rev() {
                            pending.push_front(part);
                        }
                    }
                    Err(_) => {
                        return normalize(
                            &pending.iter().fold(candidate, |path, part| path.join(part)),
                        );
                    }
                }
            }
            Ok(_) => resolved = candidate,
            Err(_) => {
                return normalize(&pending.iter().fold(candidate, |path, part| path.join(part)));
            }
        }
    }
    resolved
}
/// The resolved target if it is external, shared by classification and the permission request.
pub fn resolve_external_path(input: &str, cwd: &Path, home: &Path) -> Option<String> {
    let cwd = resolve_without_open(cwd);
    let expanded = expand_home(input, home);
    let target = resolve_without_open(&if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    });
    if target.starts_with(&cwd) {
        return None;
    }
    let target_text = target.to_string_lossy().into_owned();
    // Case-preserving spellings can name the same directory on a case-insensitive volume. stat
    // compares filesystem identity without opening a directory. Walk through missing descendants to
    // their existing ancestor; never lowercase names, since two differently cased directories can be
    // distinct on a case-sensitive volume (senpi#4351).
    let Ok(cwd_identity) = crate::external_pattern::stat_identity(&cwd.to_string_lossy()) else {
        return Some(target_text);
    };
    let mut ancestor = target.as_path();
    loop {
        if crate::external_pattern::stat_identity(&ancestor.to_string_lossy())
            .is_ok_and(|identity| identity == cwd_identity)
        {
            return None;
        }
        match ancestor.parent() {
            Some(parent) => ancestor = parent,
            None => return Some(target_text),
        }
    }
}
pub fn is_external_path(input: &str, cwd: &Path, home: &Path) -> bool {
    resolve_external_path(input, cwd, home).is_some()
}
/// True when `input` resolves to one of mhc's credential files (`auth.json`, `auth.json.lock`) in `agent_dir`,
/// independent of whether the path is inside the working directory.
pub fn is_credential_path(input: &str, cwd: &Path, home: &Path, agent_dir: &Path) -> bool {
    let cwd = resolve_without_open(cwd);
    let expanded = expand_home(input, home);
    let target = resolve_without_open(&if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    });
    let agent_dir = resolve_without_open(agent_dir);
    ["auth.json", "auth.json.lock"]
        .iter()
        .any(|name| target == agent_dir.join(name))
}
pub fn extract_external_paths(command: &str, cwd: &Path, home: &Path) -> Vec<String> {
    extract_command_paths(command)
        .into_iter()
        .filter(|token| is_external_path(token, cwd, home))
        .collect()
}
/// Path-like shell tokens (external or not), plus bare credential file names.
pub fn extract_command_paths(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in command.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            current.push(ch);
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            current.push(ch);
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            current.push(ch);
            continue;
        }
        if ch == ' ' || ch == '\t' {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
        .into_iter()
        .enumerate()
        .filter_map(|(index, token)| {
            let token = if token.len() >= 2
                && ((token.starts_with('"') && token.ends_with('"'))
                    || (token.starts_with('\'') && token.ends_with('\'')))
            {
                token[1..token.len() - 1].to_owned()
            } else {
                token
            };
            let pathlike = token.starts_with('/')
                || token.starts_with('\\')
                || token.starts_with('~')
                || token.starts_with("$HOME")
                || token.starts_with("./")
                || token.starts_with(".\\")
                || token.starts_with("../")
                || token.starts_with("..\\")
                || token.contains('/')
                || token.contains('\\');
            let credential_name = index > 0 && (token == "auth.json" || token == "auth.json.lock");
            if index == 0 && !pathlike {
                return None;
            }
            let assignment = token.split_once('=').is_some_and(|(name, _)| {
                let mut chars = name.chars();
                chars
                    .next()
                    .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
                    && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            });
            if token.starts_with('-')
                || assignment
                || ["|", "||", "&&", ";", "&", "$(", "${", "`"]
                    .iter()
                    .any(|op| token.contains(op))
                || !(pathlike || credential_name)
            {
                return None;
            }
            Some(token)
        })
        .collect()
}
#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::*;

    #[cfg(windows)]
    #[test]
    fn resolve_without_open_handles_windows_drive_symlink_target() {
        let (root, parts) = split_link_target(Path::new(r"C:\target\file.txt"));
        assert_eq!(root, Some(PathBuf::from(format!("C:{}", std::path::MAIN_SEPARATOR))));
        assert_eq!(parts, vec!["target".to_string(), "file.txt".to_string()]);
    }
}
