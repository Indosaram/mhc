//! Path handling for results (spec §6).

/// True for `C:\...` / `C:/...` style paths and UNC paths.
fn is_windows_style(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || path.starts_with("\\\\")
}

/// Forward-slash form of `path`.
fn slashed(path: &str) -> String {
    path.replace('\\', "/")
}

fn trim_trailing(path: &str) -> &str {
    if path.len() > 1 { path.trim_end_matches('/') } else { path }
}

/// `file` relative to `workdir` with `/` separators; a file outside `workdir` stays absolute
/// (slashed). Windows-style paths are compared case-insensitively.
pub fn relative_to(workdir: &str, file: &str) -> String {
    let windows = is_windows_style(workdir) || is_windows_style(file);
    let work = slashed(workdir);
    let file = slashed(file);
    let work = trim_trailing(&work);
    if !is_absolute(&file) {
        return strip_dot(&file);
    }
    let (w, f) = if windows {
        (work.to_ascii_lowercase(), file.to_ascii_lowercase())
    } else {
        (work.to_owned(), file.clone())
    };
    if f == w {
        return ".".to_owned();
    }
    let prefix = if w.ends_with('/') { w.clone() } else { format!("{w}/") };
    if f.starts_with(&prefix) {
        return strip_dot(&file[prefix.len()..]);
    }
    file
}

fn strip_dot(path: &str) -> String {
    let mut rest = path;
    while let Some(stripped) = rest.strip_prefix("./") {
        rest = stripped;
    }
    rest.to_owned()
}

/// True for a rooted or drive-letter path (slashes already normalised).
pub fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || is_windows_style(path)
}
