use super::*;
use crate::daemon_request_error::DaemonRequestError;
use pretty_assertions::assert_eq;

fn paths() -> DaemonPaths {
    DaemonPaths::under_dir("/tmp/omo-lsp-test/v1", "1")
}

#[test]
fn caller_cancelled_request_reports_cancellation() {
    let p = paths();
    let result = daemon_failure_result(&p, &DaemonRequestError::cancelled(true).into());
    assert!(result.is_error);
    assert_eq!(
        result.text(),
        format!("LSP daemon request cancelled: the caller aborted this request (for example, the turn was interrupted).\nThe daemon stays available; no LSP work was applied. Retry when you are ready.\nSocket: {}", p.socket.display())
    );
}

#[test]
fn timed_out_request_reports_the_timeout_budget() {
    let p = paths();
    let result = daemon_failure_result(&p, &DaemonRequestError::timed_out(true, 1234).into());
    assert!(result.text().starts_with(
        "LSP daemon request timed out after 1234ms: the daemon did not respond in time.\n"
    ));
    assert!(!result.text().contains("unreachable"));
    assert!(
        result
            .text()
            .ends_with(&format!("Logs: {}", p.log.display()))
    );
}

#[test]
fn genuine_transport_failure_reports_unreachable() {
    let p = paths();
    let result = daemon_failure_result(
        &p,
        &DaemonRequestError::new("connect ENOENT", false).into(),
    );
    assert_eq!(
        result.text(),
        format!("LSP daemon unreachable: connect ENOENT.\nThe MCP server is a thin proxy and never runs language servers in-process.\nSocket: {}\nLogs: {}\nThe daemon is auto-started on demand and will be retried on the next request.", p.socket.display(), p.log.display())
    );
}

#[test]
fn authentication_rejection_reports_unreachable() {
    let result = daemon_failure_result(
        &paths(),
        &DaemonRequestError::authentication_rejected().into(),
    );
    assert!(
        result
            .text()
            .starts_with("LSP daemon unreachable: daemon authentication failed before dispatch.")
    );
    assert!(!result.text().contains("cancelled"));
    assert!(!result.text().contains("timed out"));
}

#[test]
fn non_error_cause_is_stringified_into_unreachable_report() {
    let result = daemon_failure_result(
        &paths(),
        &DaemonCallError::Other("plain string failure".to_string()),
    );
    assert!(
        result
            .text()
            .starts_with("LSP daemon unreachable: plain string failure.")
    );
}

#[test]
fn error_names_match_ts_classes() {
    assert_eq!(
        DaemonRequestError::new("x", false).name(),
        "DaemonRequestError"
    );
    assert_eq!(
        DaemonRequestError::authentication_rejected().name(),
        "DaemonAuthenticationRejectedError"
    );
    assert_eq!(
        DaemonRequestError::cancelled(false).name(),
        "DaemonRequestCancelledError"
    );
    assert_eq!(
        DaemonRequestError::timed_out(false, 1).name(),
        "DaemonRequestTimedOutError"
    );
}
