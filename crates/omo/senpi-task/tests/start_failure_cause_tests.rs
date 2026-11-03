use senpi_task::manager::start_failure_cause::{
    classify_start_cause, classify_start_message, describe_start_cause, StartCauseClass,
    StartOperation,
};
use senpi_task::runners::rpc::errors::RpcCommandError;
use senpi_task::runners::rpc::protocol_client::RpcClientError;
use serde_json::json;

fn refusal_error(response: serde_json::Value) -> RpcClientError {
    RpcClientError::Command(RpcCommandError::with_refusal("prompt", "secret", &response))
}

#[test]
fn timeout_waiting_for_prompt_response_names_request_timeout() {
    let cause = classify_start_message(
        None,
        "Timeout waiting for response to prompt. Stderr: ENOENT /Users/alice/creds.json api_key=sk-secret",
    )
    .expect("classified");
    assert_eq!(cause.cause_class, StartCauseClass::RequestTimeout);
    assert_eq!(cause.timed_out_command.as_deref(), Some("prompt"));
    let desc = describe_start_cause(&cause, StartOperation::Prompt);
    assert!(!desc.contains("sk-secret"));
    assert!(!desc.contains("/Users/alice/creds.json"));
}

#[test]
fn host_refusal_with_typed_error_code() {
    let err = refusal_error(json!({ "errorCode": "host_memory_pressure" }));
    let cause = classify_start_cause(&err).expect("classified");
    assert_eq!(cause.cause_class, StartCauseClass::HostRefused);
    assert_eq!(cause.cause_code.as_deref(), Some("host_memory_pressure"));
    let desc = describe_start_cause(&cause, StartOperation::Prompt);
    assert!(desc.contains("host_memory_pressure"));
    assert!(!desc.contains("secret"));
}

#[test]
fn host_refusal_with_invalid_code_omits_code() {
    for code in [
        "sk-live-secret-key-12345",
        "too long error code exceeding normal identifier lengths that shouldn't leak through the api boundary",
    ] {
        let err = refusal_error(json!({ "errorCode": code }));
        let cause = classify_start_cause(&err).expect("classified");
        assert_eq!(cause.cause_class, StartCauseClass::HostRefused);
        assert_eq!(cause.cause_code, None);
        let desc = describe_start_cause(&cause, StartOperation::Prompt);
        assert!(!desc.contains(code));
        assert!(!desc.contains("secret"));
    }
}

#[test]
fn host_refusal_with_no_error_code() {
    let err = refusal_error(json!({ "error": "No API key found for openai." }));
    let cause = classify_start_cause(&err).expect("classified");
    assert_eq!(cause.cause_class, StartCauseClass::HostRefused);
    assert_eq!(cause.cause_code, None);
    let desc = describe_start_cause(&cause, StartOperation::Prompt);
    assert!(!desc.contains("secret"));
}

#[test]
fn lost_transport_names_connection_lost() {
    for code in ["rpc_transport_gone", "session_detached", "transport_recovery_expired"] {
        let cause = classify_start_message(Some(code), "Shared RPC host unavailable").expect("classified");
        assert_eq!(cause.cause_class, StartCauseClass::TransportLost);
        assert_eq!(cause.timed_out_command, None);
    }
}

#[test]
fn session_switch_timeout_names_switch_session() {
    let cause = classify_start_message(
        None,
        "Timeout waiting for response to switch_session. Stderr: details",
    )
    .expect("classified");
    assert_eq!(cause.cause_class, StartCauseClass::RequestTimeout);
    assert_eq!(cause.timed_out_command.as_deref(), Some("switch_session"));
    let desc = describe_start_cause(&cause, StartOperation::Session);
    assert!(!desc.contains("Stderr: details"));
}

#[test]
fn unknown_cause_returns_none() {
    assert_eq!(classify_start_message(None, "generic failure message"), None);
    assert_eq!(classify_start_message(Some("unknown_error"), "something broke"), None);
}
