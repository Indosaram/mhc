//! Read-only filesystem seam used by the loader (tests inject their own).

use std::io;
use std::path::{Path, PathBuf};

/// The filesystem operations the loader needs.
pub trait ConfigFs: Send + Sync {
    /// Whether anything exists at `path` (following symlinks).
    fn exists(&self, path: &Path) -> bool;
    /// Whether `path` is a directory (following symlinks).
    fn is_dir(&self, path: &Path) -> bool;
    /// Whether `path` is a regular file (not following symlinks).
    fn is_regular_file(&self, path: &Path) -> bool;
    /// Whether `path` itself is a symlink.
    fn is_symlink(&self, path: &Path) -> bool;
    /// Reads the whole file as UTF-8.
    fn read(&self, path: &Path) -> io::Result<String>;
    /// The symlink-resolved real path.
    fn realpath(&self, path: &Path) -> io::Result<PathBuf>;
}

/// The real filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealFs;

impl ConfigFs for RealFs {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }
    fn is_regular_file(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
    }
    fn is_symlink(&self, path: &Path) -> bool {
        maho_support::is_symlink(path)
    }
    fn read(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
    fn realpath(&self, path: &Path) -> io::Result<PathBuf> {
        path.canonicalize()
    }
}
