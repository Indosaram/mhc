//! Recursive tree copy keeping modes and mtimes, and a `(size, mtime)` manifest of a tree. Port of
//! the MIT oh-my-pi `pi-iso` `tree.rs` (`copy_dir_contents`, `copy_symlink`, `remove_existing`)
//! and the `Rcopy` per-entry actions of `rcopy.rs`; the manifest is NEW (plain-mode change
//! detection, spec §4).
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Removes the tree, file or symlink at `path`; a missing path is fine.
///
/// # Errors
/// Removal failed.
pub fn remove_existing(path: &Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Recreates the symlink `src` at `dst` with the same target.
///
/// # Errors
/// Reading or creating the link failed.
pub fn copy_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    let target = fs::read_link(src)?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, dst)
    }
    #[cfg(windows)]
    {
        if fs::metadata(src).is_ok_and(|m| m.is_dir()) {
            std::os::windows::fs::symlink_dir(target, dst)
        } else {
            std::os::windows::fs::symlink_file(target, dst)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, dst);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symlinks are not supported here",
        ))
    }
}

/// Mirrors `src`'s mtime onto `dst` (best effort; the mtime only speeds up change detection).
pub fn copy_mtime(src: &Path, dst: &Path) {
    let Ok(mtime) = fs::metadata(src).and_then(|meta| meta.modified()) else {
        return;
    };
    if let Ok(file) = fs::File::open(dst) {
        let _ = file.set_modified(mtime);
    }
}

/// Copies one regular file, keeping its mode (via `fs::copy`) and mtime.
///
/// # Errors
/// The copy failed.
pub fn copy_file(src: &Path, dst: &Path) -> io::Result<()> {
    fs::copy(src, dst)?;
    copy_mtime(src, dst);
    Ok(())
}

/// Copies one path of any kind (file, symlink, directory tree) from `src` to `dst`.
///
/// # Errors
/// Any entry could not be copied.
pub fn copy_path(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if meta.file_type().is_symlink() {
        remove_existing(dst)?;
        copy_symlink(src, dst)
    } else if meta.is_dir() {
        fs::create_dir_all(dst)?;
        copy_dir_contents(src, dst, &[])?;
        copy_mtime(src, dst);
        Ok(())
    } else {
        copy_file(src, dst)
    }
}

/// Copies the entries of `src` into the existing directory `dst`, recursing; top-level names in
/// `skip` are left out and symlinks are recreated, never followed.
///
/// # Errors
/// Any entry could not be copied.
pub fn copy_dir_contents(src: &Path, dst: &Path, skip: &[&str]) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_str().is_some_and(|name| skip.contains(&name)) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            copy_symlink(&from, &to)?;
        } else if kind.is_dir() {
            fs::create_dir_all(&to)?;
            copy_dir_contents(&from, &to, &[])?;
            copy_mtime(&from, &to);
        } else {
            copy_file(&from, &to)?;
        }
    }
    Ok(())
}

/// Size and modification time of one entry of a manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryMeta {
    pub len: u64,
    pub mtime_ns: u128,
    pub symlink: bool,
}

impl EntryMeta {
    /// The entry at `path` (not following symlinks); `None` when absent or a directory.
    pub fn of(path: &Path) -> Option<Self> {
        let meta = fs::symlink_metadata(path).ok()?;
        if meta.is_dir() {
            return None;
        }
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        Some(Self {
            len: meta.len(),
            mtime_ns,
            symlink: meta.file_type().is_symlink(),
        })
    }
}

/// Relative path (with `/`) → meta of every non-directory entry under `root`.
pub type Manifest = BTreeMap<String, EntryMeta>;

/// Walks `root` (symlinks not followed).
///
/// # Errors
/// A directory could not be read.
pub fn manifest(root: &Path) -> io::Result<Manifest> {
    let mut out = Manifest::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
                continue;
            }
            if let (Some(meta), Ok(rel)) = (EntryMeta::of(&path), path.strip_prefix(root)) {
                out.insert(rel_string(rel), meta);
            }
        }
    }
    Ok(out)
}

/// `rel` with `/` separators.
pub fn rel_string(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_keeps_mtimes_symlinks_and_skips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("sub")).expect("mkdir");
        fs::write(src.join("sub/f.txt"), "x").expect("write");
        fs::write(src.join("skip.me"), "x").expect("write");
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        fs::File::open(src.join("sub/f.txt"))
            .and_then(|f| f.set_modified(old))
            .expect("mtime");
        #[cfg(unix)]
        std::os::unix::fs::symlink("sub/f.txt", src.join("link")).expect("symlink");
        let dst = dir.path().join("dst");
        fs::create_dir_all(&dst).expect("mkdir");
        copy_dir_contents(&src, &dst, &["skip.me"]).expect("copy");
        assert!(!dst.join("skip.me").exists());
        assert_eq!(
            fs::metadata(dst.join("sub/f.txt"))
                .and_then(|m| m.modified())
                .expect("mtime"),
            old
        );
        #[cfg(unix)]
        assert!(
            fs::symlink_metadata(dst.join("link"))
                .expect("link")
                .file_type()
                .is_symlink()
        );
        let a = manifest(&src).expect("manifest");
        let b = manifest(&dst).expect("manifest");
        assert_eq!(a.get("sub/f.txt"), b.get("sub/f.txt"));
        assert!(b.contains_key("sub/f.txt") && !b.contains_key("skip.me"));
    }
}
