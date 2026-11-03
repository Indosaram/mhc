//! Port of `tool/load-cell.ts`: `%load <path | local://... | file://...>` reads a local file as
//! the cell's source when its turn comes in the kernel's queue. Every failure is a refusal that
//! names the target as the user wrote it, never an absolute path.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub const MAX_LOAD_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct LoadedCell {
    pub code: String,
    pub source_file: PathBuf,
}

pub struct LoadCellOptions {
    pub cwd: PathBuf,
    pub artifacts_dir: Option<PathBuf>,
}

/// `^([a-z][a-z0-9+.-]*)://` (case-insensitive): the URL scheme of a target, lowercased.
fn url_scheme(target: &str) -> Option<String> {
    let (scheme, _) = target.split_once("://")?;
    let mut chars = scheme.chars();
    let valid = chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    valid.then(|| scheme.to_ascii_lowercase())
}

/// `path.resolve(base, path)` without touching the file system.
fn resolve(base: &Path, path: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in base.join(path).components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    resolved
}

fn resolve_target(target: &str, options: &LoadCellOptions) -> Result<PathBuf, String> {
    let malformed = || format!("%load could not read the path in {target}");
    match url_scheme(target).as_deref() {
        Some("file") => url::Url::parse(target)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .ok_or_else(malformed),
        Some("local") => {
            let Some(artifacts_dir) = &options.artifacts_dir else {
                return Err("%load local:// needs a session artifacts directory".into());
            };
            let root = artifacts_dir.join("local");
            let decoded = percent_decode(&target["local://".len()..]).ok_or_else(malformed)?;
            let path = resolve(&root, Path::new(&decoded));
            if !path.starts_with(&root) {
                return Err(format!("%load path escapes local://: {target}"));
            }
            Ok(path)
        }
        Some(scheme) => Err(format!(
            "%load reads local files only (a path, local:// or file://); it does not fetch {scheme}:// URLs"
        )),
        None => Ok(resolve(&options.cwd, Path::new(target))),
    }
}

/// `decodeURIComponent`: `None` on a malformed escape or invalid UTF-8, like the URIError it throws.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn open_nonblocking(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0o4000);
    }
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0004);
    }
    // FILE_FLAG_BACKUP_SEMANTICS, as libuv sets on every open: without it Windows refuses to open a
    // directory (access denied) and the handle never reaches the `not a file` check.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0200_0000);
    }
    options.open(path)
}

/// Reads a `%load` target. One non-blocking open, then checks on that same handle: a FIFO or
/// device swapped in after a check can neither block the host nor be read, and the size cap keeps
/// a huge or still-growing file from stalling the host.
pub fn load_cell(target: &str, options: &LoadCellOptions) -> Result<LoadedCell, String> {
    let path = resolve_target(target, options)?;
    let too_large = || format!("%load reads files up to 8 MiB: {target}");
    let failure = |error: std::io::Error| {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::NotFound | ErrorKind::NotADirectory => format!("file not found: {target}"),
            ErrorKind::PermissionDenied => format!("permission denied: {target}"),
            _ => match error.raw_os_error() {
                Some(code) => format!("%load could not read {target} (os error {code})"),
                None => format!("%load could not read {target}"),
            },
        }
    };
    let file = open_nonblocking(&path).map_err(failure)?;
    let info = file.metadata().map_err(failure)?;
    if !info.is_file() {
        return Err(format!("not a file: {target}"));
    }
    if info.len() > MAX_LOAD_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(MAX_LOAD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    if bytes.len() as u64 > MAX_LOAD_BYTES {
        return Err(too_large());
    }
    Ok(LoadedCell {
        code: String::from_utf8_lossy(&bytes).into_owned(),
        source_file: path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(root: &Path) -> LoadCellOptions {
        LoadCellOptions {
            cwd: root.to_path_buf(),
            artifacts_dir: Some(root.join("artifacts")),
        }
    }

    #[test]
    fn reads_relative_local_and_file_targets() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("cell.js"), "print(1)").unwrap();
        std::fs::create_dir_all(root.path().join("artifacts/local")).unwrap();
        std::fs::write(root.path().join("artifacts/local/a b.js"), "print(2)").unwrap();
        let options = options(root.path());
        assert_eq!(load_cell("cell.js", &options).unwrap().code, "print(1)");
        assert_eq!(
            load_cell("local://a%20b.js", &options).unwrap().code,
            "print(2)"
        );
        let url = url::Url::from_file_path(root.path().join("cell.js")).unwrap();
        let loaded = load_cell(url.as_str(), &options).unwrap();
        assert_eq!(loaded.source_file, root.path().join("cell.js"));
    }

    #[test]
    fn refusals_name_the_target_as_written() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("dir")).unwrap();
        let options = options(root.path());
        assert_eq!(
            load_cell("missing.js", &options).unwrap_err(),
            "file not found: missing.js"
        );
        assert_eq!(load_cell("dir", &options).unwrap_err(), "not a file: dir");
        assert_eq!(
            load_cell("local://../x.js", &options).unwrap_err(),
            "%load path escapes local://: local://../x.js"
        );
        assert_eq!(
            load_cell("https://example.com/a.js", &options).unwrap_err(),
            "%load reads local files only (a path, local:// or file://); it does not fetch https:// URLs"
        );
        assert_eq!(
            load_cell("local://%zz", &options).unwrap_err(),
            "%load could not read the path in local://%zz"
        );
        let no_artifacts = LoadCellOptions {
            cwd: root.path().into(),
            artifacts_dir: None,
        };
        assert_eq!(
            load_cell("local://a.js", &no_artifacts).unwrap_err(),
            "%load local:// needs a session artifacts directory"
        );
        let big = root.path().join("big.js");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_LOAD_BYTES + 1)
            .unwrap();
        assert_eq!(
            load_cell("big.js", &options).unwrap_err(),
            "%load reads files up to 8 MiB: big.js"
        );
    }
}
