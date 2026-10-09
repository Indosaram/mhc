//! Failure notice reporting (port of `src/failure-notice.ts`).

use std::sync::Arc;

use comment_checker_core::CheckFailure;
use maho_ext_api::{ComponentLogger, ExtensionUi, NotificationType};

pub const COMMENT_CHECKER_COULD_NOT_RUN: &str =
    "omo-senpi comment-checker could not run; component disabled for this session";

pub fn report_comment_checker_failure(
    logger: Option<&Arc<dyn ComponentLogger>>,
    ui: Option<&Arc<dyn ExtensionUi>>,
    binary_path: &str,
    failure: &CheckFailure,
) {
    if let Some(logger) = logger {
        let details = serde_json::json!({
            "binaryPath": binary_path,
            "exitCode": failure.exit_code,
            "stderr": failure.stderr,
        });
        logger.warn(COMMENT_CHECKER_COULD_NOT_RUN, Some(&details));
    }
    if let Some(ui) = ui {
        ui.notify(
            &comment_checker_failure_notice(binary_path, failure),
            NotificationType::Warning,
        );
    }
}

pub fn comment_checker_failure_notice(binary_path: &str, failure: &CheckFailure) -> String {
    let cause = match failure.exit_code {
        None => "it failed to start".to_string(),
        Some(code) => format!("it exited with code {code}"),
    };
    let detail = failure.stderr.lines().next().unwrap_or("").trim();
    let detail_suffix = if !detail.is_empty() {
        format!(" ({detail})")
    } else {
        String::new()
    };
    format!(
        "OmO comment checker could not run, so it is off for the rest of this session: {cause}{detail_suffix}. Checker: {binary_path}"
    )
}
