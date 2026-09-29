//! File existence and symlink helpers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn normalize_darwin_realpath(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix("/private")) {
        Some(rest) if rest.starts_with("/var/") => PathBuf::from(rest),
        _ => path,
    }
}

/// True when the entry is a visible regular `.md` file.
pub fn is_markdown_file(name: &str, is_file: bool) -> bool {
    !name.starts_with('.') && name.ends_with(".md") && is_file
}

/// Lenient existence check: any access failure (missing, permission) reads as absent.
pub fn file_exists(path: impl AsRef<Path>) -> bool {
    fs::metadata(path).is_ok()
}

/// Strict existence check: only `NotFound` reads as absent; other errors propagate.
pub fn file_exists_strict(path: impl AsRef<Path>) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn is_symbolic_link(path: impl AsRef<Path>) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Canonical path with macOS `/private/var` collapsed to `/var`; the input on failure.
pub fn resolve_symlink(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    fs::canonicalize(path).map_or_else(|_| path.to_path_buf(), normalize_darwin_realpath)
}

/// Same contract as [`resolve_symlink`]; kept for surface parity with the async TS variant.
pub fn resolve_symlink_async(path: impl AsRef<Path>) -> PathBuf {
    resolve_symlink(path)
}
