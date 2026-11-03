use std::{future::Future, path::PathBuf, time::Duration};
pub const RESOLUTION_DEADLINE_MS: u64 = 2000;
pub fn is_missing_path_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}
pub async fn with_resolution_deadline<T>(operation: impl Future<Output = T>) -> Option<T> {
    with_resolution_deadline_ms(operation, RESOLUTION_DEADLINE_MS).await
}
/// `withResolutionDeadline(operation, timeoutMs)`: `None` is `RESOLUTION_TIMED_OUT`.
pub async fn with_resolution_deadline_ms<T>(
    operation: impl Future<Output = T>,
    timeout_ms: u64,
) -> Option<T> {
    tokio::time::timeout(Duration::from_millis(timeout_ms), operation)
        .await
        .ok()
}
pub fn fold_path_for_case_insensitive_filesystem(path: PathBuf) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        path
    }
    #[cfg(not(target_os = "linux"))]
    {
        use unicode_normalization::UnicodeNormalization;
        PathBuf::from(
            path.to_string_lossy()
                .nfc()
                .collect::<String>()
                .to_lowercase(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deadline_returns_timed_out_for_a_resolution_that_never_settles() {
        let result = with_resolution_deadline_ms(std::future::pending::<()>(), 10).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn deadline_passes_a_settled_resolution_through() {
        assert_eq!(with_resolution_deadline(async { 7 }).await, Some(7));
    }

    #[test]
    fn missing_path_errors_are_enoent_and_enotdir_only() {
        assert!(is_missing_path_error(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));
        assert!(is_missing_path_error(&std::io::Error::from(
            std::io::ErrorKind::NotADirectory
        )));
        assert!(!is_missing_path_error(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_paths_are_not_case_folded() {
        assert_eq!(
            fold_path_for_case_insensitive_filesystem(PathBuf::from("/A/B")),
            PathBuf::from("/A/B")
        );
    }
}
