//! The only filesystem surface session discovery uses (upstream `session-history-fs.ts`).
//!
//! The Rust port is synchronous: callers run it on a worker thread, and cancellation is an
//! [`AbortFlag`] rather than an `AbortSignal`.

use std::{
    fs,
    io::{self, Read},
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::UNIX_EPOCH,
};

/// Cooperative cancellation flag shared between a scan and its caller.
pub type AbortFlag = Arc<AtomicBool>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileStats {
    pub kind: FileKind,
    pub size: u64,
    /// Whole milliseconds since the epoch, floored; `None` when unavailable.
    pub mtime_ms: Option<i64>,
    pub dev: u64,
    /// `None` unless the platform reports a positive inode.
    pub ino: Option<u64>,
    /// `None` unless the platform reports a positive birth time.
    pub birthtime_ms: Option<i64>,
}

pub trait SessionHistoryFile {
    /// Reads up to `size` bytes; `None` at end of file.
    fn read(&mut self, size: usize) -> io::Result<Option<Vec<u8>>>;
    fn stat(&mut self) -> io::Result<FileStats>;
    fn close(&mut self) -> io::Result<()>;
}

/// Tests wrap this trait to count and fault operations.
pub trait SessionHistoryFs: Send + Sync {
    fn read_directory(&self, path: &str) -> io::Result<Vec<String>>;
    fn stat(&self, path: &str) -> io::Result<FileStats>;
    fn real_path(&self, path: &str) -> io::Result<String>;
    fn read_file_string(&self, path: &str) -> io::Result<String>;
    fn open(&self, path: &str) -> io::Result<Box<dyn SessionHistoryFile>>;
}

fn system_time_ms(time: io::Result<std::time::SystemTime>) -> Option<i64> {
    let time = time.ok()?;
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_millis()).ok(),
        Err(before) => i64::try_from(before.duration().as_millis())
            .ok()
            .map(|ms| -ms),
    }
}

fn to_file_stats(meta: &fs::Metadata) -> FileStats {
    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), Some(meta.ino()).filter(|ino| *ino > 0))
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0_u64, None);
    FileStats {
        kind: if meta.is_file() {
            FileKind::File
        } else if meta.is_dir() {
            FileKind::Directory
        } else {
            FileKind::Other
        },
        size: meta.len(),
        mtime_ms: system_time_ms(meta.modified()),
        dev,
        ino,
        birthtime_ms: system_time_ms(meta.created()).filter(|ms| *ms > 0),
    }
}

struct NodeFile {
    file: fs::File,
}

impl SessionHistoryFile for NodeFile {
    fn read(&mut self, size: usize) -> io::Result<Option<Vec<u8>>> {
        let mut buffer = vec![0_u8; size];
        let read = self.file.read(&mut buffer)?;
        if read == 0 {
            return Ok(None);
        }
        buffer.truncate(read);
        Ok(Some(buffer))
    }
    fn stat(&mut self) -> io::Result<FileStats> {
        Ok(to_file_stats(&self.file.metadata()?))
    }
    fn close(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The real filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct NodeSessionHistoryFs;

impl SessionHistoryFs for NodeSessionHistoryFs {
    fn read_directory(&self, path: &str) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(path)? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }
    fn stat(&self, path: &str) -> io::Result<FileStats> {
        Ok(to_file_stats(&fs::metadata(path)?))
    }
    fn real_path(&self, path: &str) -> io::Result<String> {
        Ok(fs::canonicalize(Path::new(path))?
            .to_string_lossy()
            .into_owned())
    }
    fn read_file_string(&self, path: &str) -> io::Result<String> {
        fs::read_to_string(path)
    }
    fn open(&self, path: &str) -> io::Result<Box<dyn SessionHistoryFile>> {
        Ok(Box::new(NodeFile {
            file: fs::File::open(path)?,
        }))
    }
}

pub fn node_session_history_fs() -> Arc<dyn SessionHistoryFs> {
    Arc::new(NodeSessionHistoryFs)
}

pub fn stat_or_null(fs: &dyn SessionHistoryFs, path: &str) -> Option<FileStats> {
    fs.stat(path).ok()
}

pub fn list_directory_or_empty(fs: &dyn SessionHistoryFs, path: &str) -> Vec<String> {
    fs.read_directory(path).unwrap_or_default()
}

pub fn real_path_or(fs: &dyn SessionHistoryFs, path: &str) -> String {
    fs.real_path(path).unwrap_or_else(|_| path.to_owned())
}

pub fn read_file_string_or_empty(fs: &dyn SessionHistoryFs, path: &str) -> String {
    fs.read_file_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_and_helpers_on_a_real_directory() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("a.jsonl");
        fs::write(&file, b"hello")?;
        let fs_impl = NodeSessionHistoryFs;
        let path = file.to_string_lossy().into_owned();
        let stats = stat_or_null(&fs_impl, &path).ok_or("stat")?;
        assert_eq!(stats.kind, FileKind::File);
        assert_eq!(stats.size, 5);
        assert!(stats.mtime_ms.is_some());
        let mut handle = fs_impl.open(&path)?;
        assert_eq!(handle.read(3)?, Some(b"hel".to_vec()));
        assert_eq!(handle.read(10)?, Some(b"lo".to_vec()));
        assert_eq!(handle.read(10)?, None);
        assert!(stat_or_null(&fs_impl, "/definitely/not/here").is_none());
        assert!(list_directory_or_empty(&fs_impl, "/definitely/not/here").is_empty());
        assert_eq!(
            real_path_or(&fs_impl, "/definitely/not/here"),
            "/definitely/not/here"
        );
        assert_eq!(
            read_file_string_or_empty(&fs_impl, "/definitely/not/here"),
            ""
        );
        Ok(())
    }
}
