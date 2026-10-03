use maho_codemode::tool::{interrupt_note::*, eval_kernel_reset_refused_error::*, types::EvalLanguage};

#[test]
fn reset_refusal_has_machine_code() {
    let error = EvalKernelResetRefusedError::new(EvalLanguage::Py, &["cell-1".into()]);
    assert_eq!(error.code, "eval_kernel_busy_reset_refused");
    assert_eq!(error.name, "EvalKernelResetRefusedError");
}

#[test]
fn absent_interrupt_does_not_invent_state() {
    assert!(interruption_state_note(EvalLanguage::Py, None).is_none());
}

#[tokio::test(start_paused = true)]
async fn completed_outcome_appends_note() {
    let message = describe_timeout_state("timeout", Some(std::future::ready((true, Some(" detail ".into()))))).await;
    assert!(message.ends_with(" detail"));
}

#[tokio::test(start_paused = true)]
async fn unresponsive_outcome_is_bounded() {
    let pending = std::future::pending::<(bool, Option<String>)>();
    let start = tokio::time::Instant::now();
    let message = describe_timeout_state("timeout", Some(pending)).await;
    assert_eq!(start.elapsed(), std::time::Duration::from_millis(TIMEOUT_STATE_GRACE_MS));
    assert!(!message.is_empty());
}
