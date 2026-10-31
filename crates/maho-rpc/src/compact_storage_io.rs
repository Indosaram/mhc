//! senpi `modes/rpc/compact-storage-io.ts`: the asynchronous file seam of compact current-state
//! storage (senpi#4272) and the failures it reports. Every spool access goes through
//! `CompactStorageIo`; nothing here blocks the host loop.
use futures::future::BoxFuture;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactStorageOperation {
    SpoolCreate,
    SpillWrite,
    ValueWrite,
    PageWrite,
    LeafRead,
    PageRead,
    Reclaim,
}

impl CompactStorageOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpoolCreate => "spool_create",
            Self::SpillWrite => "spill_write",
            Self::ValueWrite => "value_write",
            Self::PageWrite => "page_write",
            Self::LeafRead => "leaf_read",
            Self::PageRead => "page_read",
            Self::Reclaim => "reclaim",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactStorageFailure {
    pub operation: CompactStorageOperation,
    pub path: String,
    pub cause: String,
    /// Accepted bytes this failure leaves resident in memory instead of on disk.
    pub retained_in_memory_bytes: usize,
}

/// One leaf's bytes inside a segment file that packs a flush batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactFileRange {
    pub offset: u64,
    pub length: usize,
}

/// Asynchronous file access the host supplies; the default is tokio's filesystem.
pub trait CompactStorageIo: Send + Sync {
    /// Exclusive create (`wx`), mode 0600.
    fn write_file(&self, path: String, data: Vec<u8>) -> BoxFuture<'static, std::io::Result<()>>;
    /// The whole file, or at most `range.length` bytes from `range.offset`.
    fn read_file(
        &self,
        path: String,
        range: Option<CompactFileRange>,
    ) -> BoxFuture<'static, std::io::Result<Vec<u8>>>;
    fn unlink(&self, path: String) -> BoxFuture<'static, std::io::Result<()>>;
}

pub struct NodeCompactStorageIo;

impl CompactStorageIo for NodeCompactStorageIo {
    fn write_file(&self, path: String, data: Vec<u8>) -> BoxFuture<'static, std::io::Result<()>> {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&path).await?;
            file.write_all(&data).await?;
            file.flush().await
        })
    }

    fn read_file(
        &self,
        path: String,
        range: Option<CompactFileRange>,
    ) -> BoxFuture<'static, std::io::Result<Vec<u8>>> {
        Box::pin(async move {
            use tokio::io::{AsyncReadExt, AsyncSeekExt};
            let Some(range) = range else {
                return tokio::fs::read(&path).await;
            };
            let mut file = tokio::fs::File::open(&path).await?;
            file.seek(std::io::SeekFrom::Start(range.offset)).await?;
            let mut buffer = vec![0u8; range.length];
            let mut read = 0;
            while read < range.length {
                let n = file.read(&mut buffer[read..]).await?;
                if n == 0 {
                    break;
                }
                read += n;
            }
            buffer.truncate(read);
            Ok(buffer)
        })
    }

    fn unlink(&self, path: String) -> BoxFuture<'static, std::io::Result<()>> {
        Box::pin(async move { tokio::fs::remove_file(&path).await })
    }
}

pub fn node_compact_storage_io() -> Arc<dyn CompactStorageIo> {
    Arc::new(NodeCompactStorageIo)
}

/// Never fails: a host-supplied cause that cannot be described must not abort the failure report.
pub fn describe_cause(cause: &dyn std::fmt::Display) -> String {
    cause.to_string()
}

/// Committed storage that can no longer be read: a real error, never empty content.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("compact storage {} failed for {path}: {cause}", .operation.as_str())]
pub struct CompactStorageReadError {
    pub operation: CompactStorageOperation,
    pub path: String,
    pub cause: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_io_writes_exclusively_and_reads_ranges() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("leaf-1.bin").display().to_string();
        let io = node_compact_storage_io();
        io.write_file(path.clone(), b"abcdef".to_vec())
            .await
            .expect("write");
        assert_eq!(
            io.write_file(path.clone(), b"x".to_vec())
                .await
                .expect_err("exclusive")
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        let range = CompactFileRange {
            offset: 2,
            length: 3,
        };
        assert_eq!(
            io.read_file(path.clone(), Some(range))
                .await
                .expect("range"),
            b"cde"
        );
        assert_eq!(
            io.read_file(path.clone(), None).await.expect("whole"),
            b"abcdef"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        io.unlink(path.clone()).await.expect("unlink");
        assert_eq!(
            io.unlink(path).await.expect_err("gone").kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
