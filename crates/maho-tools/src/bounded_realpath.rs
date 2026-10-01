use std::{future::Future, path::PathBuf, time::Duration};
pub const RESOLUTION_DEADLINE_MS: u64 = 2000;
pub fn is_missing_path_error(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory)
}
pub async fn with_resolution_deadline<T>(operation: impl Future<Output = T>) -> Option<T> {
    tokio::time::timeout(Duration::from_millis(RESOLUTION_DEADLINE_MS), operation).await.ok()
}
pub fn fold_path_for_case_insensitive_filesystem(path: PathBuf) -> PathBuf {
    #[cfg(target_os = "linux")]
    { path }
    #[cfg(not(target_os = "linux"))]
    { use unicode_normalization::UnicodeNormalization; PathBuf::from(path.to_string_lossy().nfc().collect::<String>().to_lowercase()) }
}
