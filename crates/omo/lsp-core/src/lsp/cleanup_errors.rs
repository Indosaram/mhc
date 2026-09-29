/// TS `reportBestEffortCleanupError` with the default logger (stderr).
pub fn report_best_effort_cleanup_error(operation: &str, error: &dyn std::fmt::Display) {
    report_best_effort_cleanup_error_with(operation, error, |message| eprintln!("{message}"));
}

/// TS `reportBestEffortCleanupError` with an injected logger.
pub fn report_best_effort_cleanup_error_with(
    operation: &str,
    error: &dyn std::fmt::Display,
    mut logger: impl FnMut(&str),
) {
    logger(&format!(
        "[lsp] ignored {operation} failure during cleanup: {error}"
    ));
}

#[cfg(test)]
#[path = "cleanup_errors_tests.rs"]
mod tests;
