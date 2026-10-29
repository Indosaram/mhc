use super::*;
use pretty_assertions::assert_eq;

#[test]
fn injected_logger_receives_normal_diagnostic_without_environment_gate() {
    let mut messages: Vec<String> = Vec::new();

    report_best_effort_cleanup_error_with("client stop", &"connection closed", |message| {
        messages.push(message.to_string());
    });

    assert_eq!(
        messages,
        vec!["[lsp] ignored client stop failure during cleanup: connection closed".to_string()]
    );
}
