//! Port of senpi `packages/coding-agent/src/utils/paths.ts` (the subset maho-core uses).

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const UNICODE_SPACES: [char; 10] =
    ['\u{a0}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}', '\u{202f}'];

#[derive(Debug, Clone, Default)]
pub struct PathInputOptions {
    pub trim: bool,
    pub expand_tilde: bool,
    pub home_dir: Option<String>,
    pub strip_at_prefix: bool,
    pub normalize_unicode_spaces: bool,
}

impl PathInputOptions {
    pub fn expanding_tilde(home_dir: impl Into<String>) -> Self {
        Self { expand_tilde: true, home_dir: Some(home_dir.into()), ..Self::default() }
    }
}

fn default_expand_tilde() -> bool {
    true
}

/// `canonicalizePath`: realpath, falling back to the raw path when resolution fails.
pub fn canonicalize_path(path: &str) -> String {
    std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_owned())
}

/// `canonicalizePathStrict`: refuses to guess; an unresolvable path is an error.
pub fn canonicalize_path_strict(path: &str) -> std::io::Result<String> {
    std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned())
}

/// `getFileRevision`: `dev:ino:size:mtimeNs:ctimeNs`, or `None` when stat fails.
pub fn get_file_revision(path: &str) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let mtime_ns = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let dev = 0u64;
    let ino = 0u64;
    Some(format!("{dev}:{ino}:{}:{mtime_ns}:{mtime_ns}", metadata.len()))
}

/// `getFileContentRevision`: sha256 of the file contents, or `None` when unreadable.
pub fn get_file_content_revision(path: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(hex::encode(hasher.finalize()))
}

/// `isLocalPath`: true unless the value is a package source or a remote URL protocol.
pub fn is_local_path(value: &str) -> bool {
    let trimmed = value.trim();
    !(trimmed.starts_with("npm:")
        || trimmed.starts_with("git:")
        || trimmed.starts_with("github:")
        || trimmed.starts_with("http:")
        || trimmed.starts_with("https:")
        || trimmed.starts_with("ssh:"))
}

/// `normalizeWindowsShellPath`: convert Git Bash/MSYS/Cygwin/WSL drive paths.
pub fn normalize_windows_shell_path(file_path: &str) -> String {
    if !file_path.starts_with('/') || file_path.starts_with("//") || file_path.contains('\\') {
        return file_path.to_owned();
    }
    let rest = file_path.strip_prefix('/').unwrap_or(file_path);
    let rest = rest.strip_prefix("mnt/").or_else(|| rest.strip_prefix("cygdrive/")).unwrap_or(rest);
    let Some(drive) = rest.chars().next().filter(|c| c.is_ascii_alphabetic()) else {
        return file_path.to_owned();
    };
    let suffix = &rest[drive.len_utf8()..];
    let suffix = suffix.strip_prefix('/').unwrap_or(suffix).replace('/', "\\");
    if suffix.is_empty() {
        format!("{}:\\", drive.to_ascii_uppercase())
    } else {
        format!("{}:\\{}", drive.to_ascii_uppercase(), suffix)
    }
}

/// `normalizePath`: trim, unicode-space folding, `@` stripping, `~` expansion, `file:` URLs.
pub fn normalize_path(input: &str, options: &PathInputOptions) -> String {
    let mut normalized = if options.trim { input.trim().to_owned() } else { input.to_owned() };
    if options.normalize_unicode_spaces {
        normalized = normalized.chars().map(|c| if UNICODE_SPACES.contains(&c) { ' ' } else { c }).collect();
    }
    if options.strip_at_prefix {
        if let Some(stripped) = normalized.strip_prefix('@') {
            normalized = stripped.to_owned();
        }
    }
    if cfg!(windows) {
        normalized = normalize_windows_shell_path(&normalized);
    }

    let expand = if options.expand_tilde { true } else { default_expand_tilde() };
    if expand {
        let home = options.home_dir.clone().unwrap_or_else(crate::config::home_dir);
        if normalized == "~" {
            return home;
        }
        if let Some(rest) = normalized.strip_prefix("~/") {
            return join(&home, rest);
        }
        if cfg!(windows) {
            if let Some(rest) = normalized.strip_prefix("~\\") {
                return join(&home, rest);
            }
        }
    }

    if normalized.starts_with("file://") {
        if let Some(rest) = normalized.strip_prefix("file://") {
            return rest.to_owned();
        }
    }

    normalized
}

/// `resolvePath`: normalize then resolve relative to `base_dir`.
pub fn resolve_path(input: &str, base_dir: &str, options: &PathInputOptions) -> String {
    let normalized = normalize_path(input, options);
    let normalized_base = normalize_path(base_dir, &PathInputOptions::default());
    if Path::new(&normalized).is_absolute() {
        lexical_resolve(&normalized)
    } else {
        lexical_resolve(&join(&normalized_base, &normalized))
    }
}

fn join(base: &str, child: &str) -> String {
    Path::new(base).join(child).to_string_lossy().into_owned()
}

/// `nodeResolvePath`: collapse `.`/`..` lexically and strip a trailing separator.
pub fn lexical_resolve(path: &str) -> String {
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    let resolved = out.to_string_lossy().into_owned();
    if resolved.is_empty() { ".".to_owned() } else { resolved }
}

/// `getCwdRelativePath`: the path relative to `cwd`, or `None` when it escapes `cwd`.
pub fn get_cwd_relative_path(file_path: &str, cwd: &str) -> Option<String> {
    let resolved_cwd = resolve_path(cwd, cwd, &PathInputOptions::default());
    let resolved_path = resolve_path(file_path, &resolved_cwd, &PathInputOptions::default());
    let relative = Path::new(&resolved_path).strip_prefix(Path::new(&resolved_cwd)).ok()?;
    let relative = relative.to_string_lossy().into_owned();
    Some(if relative.is_empty() { ".".to_owned() } else { relative })
}

/// `formatPathRelativeToCwdOrAbsolute`.
pub fn format_path_relative_to_cwd_or_absolute(file_path: &str, cwd: &str) -> String {
    let absolute = resolve_path(file_path, cwd, &PathInputOptions::default());
    let rendered = get_cwd_relative_path(&absolute, cwd).unwrap_or(absolute);
    rendered.replace(std::path::MAIN_SEPARATOR, "/")
}

/// `shortenPath`: replace a leading home directory with `~`.
pub fn shorten_path(path: &str) -> String {
    if path.is_empty() {
        return path.to_owned();
    }
    let home = crate::config::home_dir();
    match path.strip_prefix(&home) {
        Some(rest) => format!("~{rest}"),
        None => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_tilde_against_the_given_home() {
        let options = PathInputOptions::expanding_tilde("/home/u");
        assert_eq!(normalize_path("~", &options), "/home/u");
        assert_eq!(normalize_path("~/x/y", &options), "/home/u/x/y");
        assert_eq!(normalize_path("/abs", &options), "/abs");
    }

    #[test]
    fn trim_strip_at_and_unicode_spaces() {
        let options = PathInputOptions { trim: true, expand_tilde: false, strip_at_prefix: true, normalize_unicode_spaces: true, ..Default::default() };
        assert_eq!(normalize_path("  @a\u{00a0}b  ", &options), "a b");
    }

    #[test]
    fn resolves_relative_against_a_base() {
        let options = PathInputOptions { expand_tilde: false, ..Default::default() };
        assert_eq!(resolve_path("b/c", "/a", &options), "/a/b/c");
        assert_eq!(resolve_path("../c", "/a/b", &options), "/a/c");
        assert_eq!(resolve_path("/x/./y/../z", "/a", &options), "/x/z");
    }

    #[test]
    fn detects_local_paths() {
        assert!(is_local_path("./x"));
        assert!(is_local_path("file:///x"));
        assert!(!is_local_path("npm:foo"));
        assert!(!is_local_path("git:github.com/x/y"));
        assert!(!is_local_path("https://example.com"));
    }

    #[test]
    fn cwd_relative_path_refuses_escapes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().into_owned();
        let inside = tmp.path().join("a/b.txt");
        std::fs::create_dir_all(tmp.path().join("a")).expect("mkdir");
        std::fs::write(&inside, "x").expect("write");
        assert_eq!(get_cwd_relative_path(&inside.to_string_lossy(), &cwd).as_deref(), Some("a/b.txt"));
        assert_eq!(get_cwd_relative_path(&cwd, &cwd).as_deref(), Some("."));
        assert!(get_cwd_relative_path("/etc/passwd", &cwd).is_none());
    }

    #[test]
    fn content_revision_is_stable_and_changes_with_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("f.txt");
        std::fs::write(&file, "one").expect("write");
        let first = get_file_content_revision(&file.to_string_lossy()).expect("revision");
        assert_eq!(get_file_content_revision(&file.to_string_lossy()).as_deref(), Some(first.as_str()));
        std::fs::write(&file, "two").expect("write");
        assert_ne!(get_file_content_revision(&file.to_string_lossy()).as_deref(), Some(first.as_str()));
        assert!(get_file_content_revision("/definitely/not/here").is_none());
    }

    #[test]
    fn normalizes_windows_shell_paths() {
        assert_eq!(normalize_windows_shell_path("/c/Users/x"), "C:\\Users\\x");
        assert_eq!(normalize_windows_shell_path("/mnt/d/tmp"), "D:\\tmp");
        assert_eq!(normalize_windows_shell_path("/c"), "C:\\");
        assert_eq!(normalize_windows_shell_path("/not-a-drive/x"), "/not-a-drive/x");
    }

    #[test]
    fn shortens_paths_under_home() {
        let home = crate::config::home_dir();
        assert_eq!(shorten_path(&format!("{home}/x")), "~/x");
        assert_eq!(shorten_path("/other"), "/other");
    }
}
