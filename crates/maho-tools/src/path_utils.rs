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
    PathBuf::from(normalized)
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
                    return Err(std::io::Error::from_raw_os_error(40));
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
    use super::resolve_read_path_async;

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
