//! Re-exported from `maho-machine-ops` (`senpi#5592`).

pub use maho_machine_ops::files::bounded_realpath::{
    RESOLUTION_DEADLINE_MS, fold_path_for_case_insensitive_filesystem, is_missing_path_error,
    symlink_loop_error, with_resolution_deadline, with_resolution_deadline_ms,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
