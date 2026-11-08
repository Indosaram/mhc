use std::{future::Future, path::PathBuf, time::Duration};
pub const RESOLUTION_DEADLINE_MS: u64 = 2000;

/// The resolution hit its symlink/reparse-point budget.
///
/// Raw errno 40 is ELOOP on Linux but means nothing on Windows (and is `EMSGSIZE` elsewhere), so
/// the failure is built from a kind and message instead of a numeric OS code.
pub fn symlink_loop_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "ELOOP: too many levels of symbolic links",
    )
}
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
        // A verbatim prefix is matched literally by Win32: lowercasing `\\?\UNC\` yields `\\?\unc\`,
        // which no longer names the same path. Fold only what follows it.
        let text = path.to_string_lossy();
        let (prefix, tail) = if let Some(tail) = text.strip_prefix(r"\\?\UNC\") {
            (r"\\?\UNC\", tail)
        } else if let Some(tail) = text.strip_prefix(r"\\?\") {
            (r"\\?\", tail)
        } else {
            ("", text.as_ref())
        };
        let folded: String = tail.nfc().collect::<String>().to_lowercase();
        PathBuf::from(format!("{prefix}{folded}"))
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

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn folding_keeps_the_verbatim_prefix_intact() {
        assert_eq!(
            fold_path_for_case_insensitive_filesystem(PathBuf::from(r"\\?\C:\Users\Mixed\File")),
            PathBuf::from(r"\\?\c:\users\mixed\file")
        );
        assert_eq!(
            fold_path_for_case_insensitive_filesystem(PathBuf::from(r"\\?\UNC\Server\Share\Mixed")),
            PathBuf::from(r"\\?\UNC\server\share\mixed")
        );
    }
}
