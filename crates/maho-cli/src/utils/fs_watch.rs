use std::path::{Path, PathBuf};
pub const FS_WATCH_RETRY_DELAY_MS: u64 = 5000;
pub fn canonical_watch_path(path: &Path) -> PathBuf {
    if cfg!(windows) { std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()) } else { path.to_path_buf() }
}
pub async fn probe_directory_openable(directory: &Path) -> std::io::Result<()> {
    let mut directory = tokio::fs::read_dir(directory).await?;
    directory.next_entry().await?;
    Ok(())
}
