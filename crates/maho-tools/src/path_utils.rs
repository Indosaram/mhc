use std::path::{Component, Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub fn expand_path(path: &str) -> PathBuf {
    expand_path_with(path, || {
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    })
}

pub fn expand_path_with(path: &str, home: impl FnOnce() -> Option<std::ffi::OsString>) -> PathBuf {
    let normalized: String = path
        .chars()
        .map(|c| match c {
            '\u{a0}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' => ' ',
            _ => c,
        })
        .collect();
    let normalized = normalized.strip_prefix('@').unwrap_or(&normalized);
    let normalized = expand_percent_variables(normalized);
    // Parity with senpi's `normalizePath`: a `/mnt/c/...`, `/cygdrive/c/...` or `/c/...` path pasted
    // from WSL/Git Bash names a Windows drive on Windows, and a `file://` URL is a local path.
    let normalized = if cfg!(windows) {
        std::borrow::Cow::Owned(normalize_windows_shell_path(&normalized))
    } else {
        normalized
    };
    if normalized.starts_with("file://")
        && let Some(local) = file_url_to_path(&normalized)
    {
        return local;
    }
    let is_tilde = normalized == "~"
        || normalized.starts_with("~/")
        || (cfg!(windows) && normalized.starts_with("~\\"));
    if is_tilde && let Some(home) = home() {
        let rest = normalized
            .strip_prefix("~/")
            .or_else(|| normalized.strip_prefix("~\\"))
            .unwrap_or("");
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(normalized.as_ref())
}

/// `normalizeWindowsShellPath`: `/mnt/c/x`, `/cygdrive/d/x` and `/c/x` name `C:\x` / `D:\x`.
fn normalize_windows_shell_path(path: &str) -> String {
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
        return path.to_owned();
    }
    let rest = path.strip_prefix('/').unwrap_or(path);
    let rest = rest
        .strip_prefix("mnt/")
        .or_else(|| rest.strip_prefix("cygdrive/"))
        .unwrap_or(rest);
    let Some(drive) = rest.chars().next().filter(|c| c.is_ascii_alphabetic()) else {
        return path.to_owned();
    };
    let suffix = &rest[drive.len_utf8()..];
    if !suffix.is_empty() && !suffix.starts_with('/') {
        return path.to_owned();
    }
    let suffix = suffix
        .strip_prefix('/')
        .unwrap_or(suffix)
        .replace('/', "\\");
    if suffix.is_empty() {
        format!("{}:\\", drive.to_ascii_uppercase())
    } else {
        format!("{}:\\{}", drive.to_ascii_uppercase(), suffix)
    }
}

/// `fileURLToPath` for a local file URL: `file:///C:/x`, `file:///tmp/a%20b`, `file://localhost/x`.
/// A remote host or a malformed escape is not a local path, so the caller keeps the original text.
fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(at) => {
            let host = &rest[..at];
            if !host.eq_ignore_ascii_case("localhost") {
                return None;
            }
            &rest[at..]
        }
        None => return None,
    };
    let bytes = path.as_bytes();
    let well_formed = bytes.iter().enumerate().all(|(index, byte)| {
        *byte != b'%'
            || (bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit))
    });
    if !well_formed {
        return None;
    }
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    // A decoded NUL cannot be part of a filesystem path; the OS would silently truncate there.
    if decoded.contains('\0') {
        return None;
    }
    #[cfg(windows)]
    {
        let decoded = decoded.as_ref();
        let without_slash = decoded.strip_prefix('/').unwrap_or(decoded);
        let is_drive = without_slash
            .chars()
            .next()
            .is_some_and(|drive| drive.is_ascii_alphabetic())
            && without_slash.chars().nth(1) == Some(':');
        if is_drive {
            return Some(PathBuf::from(normalize_windows_shell_path(without_slash)));
        }
        // A rooted URL without a drive is a rooted path on the current drive: `\tmp\x`.
        Some(PathBuf::from(decoded.replace('/', "\\")))
    }
    #[cfg(not(windows))]
    {
        Some(PathBuf::from(decoded.as_ref()))
    }
}

/// Expand `%NAME%` the way `cmd.exe` does when a path is pasted from Windows Explorer, `set`, or a
/// registry value. Only Windows inherits this spelling, and an unknown name is left untouched so a
/// literal `%` in a file name still resolves.
fn expand_percent_variables(path: &str) -> std::borrow::Cow<'_, str> {
    if !cfg!(windows) {
        return std::borrow::Cow::Borrowed(path);
    }
    expand_percent_variables_with(path, |name| {
        std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
    })
}

fn expand_percent_variables_with(
    path: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> std::borrow::Cow<'_, str> {
    if !path.contains('%') {
        return std::borrow::Cow::Borrowed(path);
    }
    let mut expanded = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(start) = rest.find('%') {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match lookup(name).filter(|value| !value.is_empty()) {
                    Some(value) => {
                        expanded.push_str(&value);
                        rest = &after[end + 1..];
                    }
                    None => {
                        // Unknown or empty variable: keep the literal `%NAME%` verbatim.
                        expanded.push_str(&rest[start..start + end + 2]);
                        rest = &after[end + 1..];
                    }
                }
            }
            _ => {
                expanded.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    expanded.push_str(rest);
    std::borrow::Cow::Owned(expanded)
}
pub fn resolve_to_cwd(path: &str, cwd: &Path) -> PathBuf {
    let path = expand_path(path);
    let joined = cwd.join(path);
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            component => result.push(component.as_os_str()),
        }
    }
    result
}
pub async fn path_exists(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}
pub async fn resolve_read_path_async(path: &str, cwd: &Path) -> PathBuf {
    let mut path = path.to_owned();
    let resolved = loop {
        let resolved = resolve_to_cwd(&path, cwd);
        if path_exists(&resolved).await {
            return resolved;
        }
        match unquoted_path(&path) {
            Some(unquoted) => path = unquoted,
            None => break resolved,
        }
    };
    let text = resolved.to_string_lossy();
    let mut ampm = text.to_string();
    for marker in ["AM", "PM", "am", "pm", "Am", "Pm", "aM", "pM"] {
        ampm = ampm.replace(&format!(" {marker}."), &format!("\u{202f}{marker}."));
    }
    let nfd = text.nfd().collect::<String>();
    for candidate in [
        ampm,
        nfd.clone(),
        text.replace('\'', "\u{2019}"),
        nfd.replace('\'', "\u{2019}"),
    ] {
        let candidate = PathBuf::from(candidate);
        if candidate != resolved && path_exists(&candidate).await {
            return candidate;
        }
    }
    resolved
}
fn unquoted_path(path: &str) -> Option<String> {
    let path = path.trim();
    let path = path.strip_prefix('@').unwrap_or(path);
    let quote = path.chars().next()?;
    if !matches!(quote, '\'' | '"') || !path.ends_with(quote) || path.len() <= quote.len_utf8() {
        return None;
    }
    Some(path[quote.len_utf8()..path.len() - quote.len_utf8()].to_owned())
}

/// Component traversal with lstat/readlink only, used after the realpath deadline.
pub fn realpath_without_open_strict(path: &Path) -> std::io::Result<PathBuf> {
    use std::collections::VecDeque;
    let mut pending: VecDeque<_> = path
        .components()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    let mut resolved = PathBuf::new();
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
            Ok(meta) if meta.is_symlink() => {
                hops += 1;
                if hops > 40 {
                    return Err(crate::bounded_realpath::symlink_loop_error());
                }
                let target = std::fs::read_link(&candidate)?;
                if target.is_absolute() {
                    resolved.clear();
                }
                let parts: Vec<_> = target
                    .components()
                    .map(|c| c.as_os_str().to_owned())
                    .collect();
                for component in parts.into_iter().rev() {
                    pending.push_front(component);
                }
            }
            Ok(_) => resolved.push(part),
            Err(error) if crate::bounded_realpath::is_missing_path_error(&error) => {
                resolved.push(part);
                for part in pending {
                    if part == ".." {
                        resolved.pop();
                    } else if part != "." {
                        resolved.push(part);
                    }
                }
                return Ok(resolved);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::{file_url_to_path, normalize_windows_shell_path, resolve_read_path_async};

    #[tokio::test]
    async fn literal_single_quoted_path_wins_when_both_files_exist() -> std::io::Result<()> {
        // Given
        let directory = tempfile::tempdir()?;
        let literal = directory.path().join("'name'");
        std::fs::write(&literal, "literal")?;
        std::fs::write(directory.path().join("name"), "unquoted")?;
        // When
        let resolved = resolve_read_path_async("'name'", directory.path()).await;
        // Then
        assert_eq!(resolved, literal);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn literal_double_quoted_path_wins_when_both_files_exist() -> std::io::Result<()> {
        // Given: double quotes are valid filename characters on Unix.
        let directory = tempfile::tempdir()?;
        let literal = directory.path().join("\"name\"");
        std::fs::write(&literal, "literal")?;
        std::fs::write(directory.path().join("name"), "unquoted")?;
        // When
        let resolved = resolve_read_path_async("\"name\"", directory.path()).await;
        // Then
        assert_eq!(resolved, literal);
        Ok(())
    }

    #[tokio::test]
    async fn unquotes_repeatedly_when_literal_paths_are_missing() -> std::io::Result<()> {
        // Given
        let directory = tempfile::tempdir()?;
        let expected = directory.path().join("file name");
        std::fs::write(&expected, "contents")?;
        for input in [
            "\"file name\"",
            "'file name'",
            " @\"file name\" ",
            "\"'file name'\"",
        ] {
            // When
            let resolved = resolve_read_path_async(input, directory.path()).await;
            // Then
            assert_eq!(resolved, expected, "input: {input}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn unquotes_before_trying_filename_variants() -> std::io::Result<()> {
        // Given
        let directory = tempfile::tempdir()?;
        let expected = directory.path().join("Capture d\u{2019}ecran.png");
        std::fs::write(&expected, "unquoted variant")?;
        std::fs::write(
            directory.path().join("'Capture d\u{2019}ecran.png'"),
            "quoted variant",
        )?;
        // When
        let resolved = resolve_read_path_async("'Capture d'ecran.png'", directory.path()).await;
        // Then
        assert_eq!(resolved, expected);
        Ok(())
    }

    #[tokio::test]
    async fn returns_unquoted_path_when_no_candidate_exists() -> std::io::Result<()> {
        // Given
        let directory = tempfile::tempdir()?;
        // When
        let resolved = resolve_read_path_async("\"'missing'\"", directory.path()).await;
        // Then
        assert_eq!(resolved, directory.path().join("missing"));
        Ok(())
    }

    #[test]
    fn windows_shell_and_file_urls_resolve_to_native_paths() {
        // Git Bash / Cygwin / WSL drive spellings pasted into a tool argument.
        assert_eq!(normalize_windows_shell_path("/c/Users/x"), "C:\\Users\\x");
        assert_eq!(normalize_windows_shell_path("/mnt/d/tmp"), "D:\\tmp");
        assert_eq!(normalize_windows_shell_path("/cygdrive/e/work"), "E:\\work");
        assert_eq!(normalize_windows_shell_path("/c"), "C:\\");
        // Anything that is not a drive spelling is left alone.
        assert_eq!(normalize_windows_shell_path("/usr/lib"), "/usr/lib");
        assert_eq!(
            normalize_windows_shell_path("//server/share"),
            "//server/share"
        );
        assert_eq!(normalize_windows_shell_path(r"C:\x"), r"C:\x");
    }

    #[cfg(not(windows))]
    #[test]
    fn file_urls_resolve_to_local_paths() {
        assert_eq!(
            file_url_to_path("file:///tmp/a%20b").as_deref(),
            Some(std::path::Path::new("/tmp/a b"))
        );
        assert_eq!(
            file_url_to_path("file://localhost/tmp/x").as_deref(),
            Some(std::path::Path::new("/tmp/x"))
        );
        // A remote host, a malformed escape and an encoded NUL are not local paths.
        assert_eq!(file_url_to_path("file://remote/path"), None);
        assert_eq!(file_url_to_path("file:///tmp/%zz"), None);
        assert_eq!(file_url_to_path("file:///tmp/a%00b"), None);
    }

    #[cfg(windows)]
    #[test]
    fn file_urls_resolve_to_windows_paths() {
        assert_eq!(
            file_url_to_path("file:///C:/Temp/a%20b").as_deref(),
            Some(std::path::Path::new(r"C:\Temp\a b"))
        );
        // A rooted URL without a drive is rooted on the current drive.
        assert_eq!(
            file_url_to_path("file:///tmp/x").as_deref(),
            Some(std::path::Path::new(r"\tmp\x"))
        );
        assert_eq!(file_url_to_path("file://remote/path"), None);
        assert_eq!(file_url_to_path("file:///tmp/%zz"), None);
        assert_eq!(file_url_to_path("file:///tmp/a%00b"), None);
    }

    #[test]
    fn percent_variables_expand_and_unknown_names_stay_literal() {
        let lookup = |name: &str| match name {
            "USERPROFILE" => Some(r"C:\Users\test".to_string()),
            "EMPTY" => Some(String::new()),
            _ => None,
        };
        assert_eq!(
            super::expand_percent_variables_with(r"%USERPROFILE%\src\main.rs", lookup),
            r"C:\Users\test\src\main.rs"
        );
        // An unknown or empty name keeps its spelling, so a literal `%` file name still resolves.
        assert_eq!(
            super::expand_percent_variables_with(r"%NOPE%\100%\x", lookup),
            r"%NOPE%\100%\x"
        );
        assert_eq!(
            super::expand_percent_variables_with(r"%EMPTY%\x", lookup),
            r"%EMPTY%\x"
        );
        // A trailing lone `%` is not a variable reference.
        assert_eq!(super::expand_percent_variables_with("50%", lookup), "50%");
    }

    #[test]
    fn expand_path_windows_userprofile() {
        use std::ffi::OsString;

        let untouched = super::expand_path_with("~/doc", || None);
        assert_eq!(untouched, std::path::PathBuf::from("~/doc"));

        let expanded = super::expand_path_with("~/doc", || Some(OsString::from("/fake/profile")));
        assert_eq!(expanded, std::path::PathBuf::from("/fake/profile/doc"));

        #[cfg(windows)]
        {
            let expanded_backslash =
                super::expand_path_with("~\\doc", || Some(OsString::from(r"C:\Users\test")));
            assert_eq!(
                expanded_backslash,
                std::path::PathBuf::from(r"C:\Users\test\doc")
            );
        }

        let mut order = Vec::new();
        let lookup = |home: Option<&'static str>,
                      profile: Option<&'static str>,
                      log: &mut Vec<&'static str>| {
            home.map(|v| {
                log.push("HOME");
                OsString::from(v)
            })
            .or_else(|| {
                log.push("USERPROFILE");
                profile.map(OsString::from)
            })
        };

        let resolved_profile =
            super::expand_path_with("~/doc", || lookup(None, Some("/profile"), &mut order));
        assert_eq!(order, vec!["USERPROFILE"]);
        assert_eq!(resolved_profile, std::path::PathBuf::from("/profile/doc"));

        order.clear();
        let resolved_home = super::expand_path_with("~/doc", || {
            lookup(Some("/home"), Some("/profile"), &mut order)
        });
        assert_eq!(order, vec!["HOME"]);
        assert_eq!(resolved_home, std::path::PathBuf::from("/home/doc"));
    }
}
