//! `manager/start-failure-cause.ts`: classifies WHY a child's first prompt (or its session reopen)
//! was rejected. The caught message is untrusted child/host output, so it is never copied: only a
//! closed class, an allow-listed command name, a shape-checked host error code and a shape-checked
//! provider leave this module.

use crate::runners::rpc::errors::{ERROR_CODE_SHAPE, PROVIDER_SHAPE, RpcCommandError};
use crate::runners::rpc::protocol_client::RpcClientError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartCauseClass {
    RequestTimeout,
    HostRefused,
    TransportLost,
    CredentialsMissing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartCause {
    pub cause_class: StartCauseClass,
    pub timed_out_command: Option<String>,
    pub cause_code: Option<String>,
    pub provider: Option<String>,
}

/// `RPC_RESPONSE_TIMEOUT_PREFIX` (`runners/rpc-host/reattach.ts`).
pub const RPC_RESPONSE_TIMEOUT_PREFIX: &str = "Timeout waiting for response to ";
const TIMED_OUT_COMMANDS: [&str; 6] = [
    "prompt",
    "switch_session",
    "open_session",
    "get_entries",
    "steer",
    "follow_up",
];
const TRANSPORT_LOSS_CODES: [&str; 3] = [
    "rpc_transport_gone",
    "session_detached",
    "transport_recovery_expired",
];
// The engine's typed "this provider cannot authenticate" refusals. An expired login that could not
// be refreshed is as provider-wide as a missing key, so both walk the chain the same way.
const CREDENTIAL_CODES: [&str; 2] = [
    "provider_credentials_missing",
    "provider_credentials_expired",
];

fn class(cause_class: StartCauseClass) -> StartCause {
    StartCause {
        cause_class,
        timed_out_command: None,
        cause_code: None,
        provider: None,
    }
}

/// `classifyStartCause` for the stdio RPC client's error.
pub fn classify_start_cause(cause: &RpcClientError) -> Option<StartCause> {
    match cause {
        RpcClientError::Command(refusal) => Some(host_refusal(refusal)),
        other => classify_start_message(None, &other.to_string()),
    }
}

/// `classifyStartCause` for an error that only carries an optional code and a message.
pub fn classify_start_message(code: Option<&str>, message: &str) -> Option<StartCause> {
    if code.is_some_and(|code| TRANSPORT_LOSS_CODES.contains(&code)) {
        return Some(class(StartCauseClass::TransportLost));
    }
    message
        .strip_prefix(RPC_RESPONSE_TIMEOUT_PREFIX)
        .map(request_timeout)
}

fn host_refusal(refusal: &RpcCommandError) -> StartCause {
    if let Some(code) = refusal
        .error_code
        .as_deref()
        .filter(|code| CREDENTIAL_CODES.contains(code))
    {
        return StartCause {
            cause_code: Some(code.to_string()),
            provider: refusal
                .error_data
                .as_ref()
                .map(|data| data.provider.clone())
                .filter(|provider| PROVIDER_SHAPE.is_match(provider)),
            ..class(StartCauseClass::CredentialsMissing)
        };
    }
    StartCause {
        cause_code: refusal
            .error_code
            .clone()
            .filter(|code| ERROR_CODE_SHAPE.is_match(code)),
        ..class(StartCauseClass::HostRefused)
    }
}

fn request_timeout(rest: &str) -> StartCause {
    let command = rest
        .split_once('.')
        .map(|(command, _)| command)
        .filter(|command| {
            !command.is_empty() && command.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
        .filter(|command| TIMED_OUT_COMMANDS.contains(command));
    StartCause {
        timed_out_command: command.map(str::to_string),
        ..class(StartCauseClass::RequestTimeout)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOperation {
    Prompt,
    Session,
}

pub fn describe_start_cause(cause: &StartCause, operation: StartOperation) -> String {
    match cause.cause_class {
        StartCauseClass::RequestTimeout => {
            let command = cause
                .timed_out_command
                .as_deref()
                .unwrap_or(match operation {
                    StartOperation::Prompt => "prompt",
                    StartOperation::Session => "session",
                });
            format!("no answer to the {command} request in time (request_timeout)")
        }
        StartCauseClass::HostRefused => {
            let code = cause.cause_code.as_deref().map_or_else(
                || "host_refused".to_string(),
                |code| format!("host_refused: {code}"),
            );
            match operation {
                StartOperation::Prompt => format!("the prompt was refused ({code})"),
                StartOperation::Session => format!("the session open was refused ({code})"),
            }
        }
        StartCauseClass::TransportLost => {
            "the connection to the child was lost (transport_lost)".to_string()
        }
        StartCauseClass::CredentialsMissing => {
            let provider = cause.provider.as_deref().unwrap_or("its provider");
            let code = cause.cause_code.as_deref().unwrap_or("credentials_missing");
            format!(
                "{provider} has no usable credentials ({code}); sign in to {provider} or add its API key"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn refused(response: serde_json::Value) -> RpcClientError {
        RpcClientError::Command(RpcCommandError::with_refusal("prompt", "secret", &response))
    }

    #[test]
    fn credential_refusal_classifies_with_validated_provider() {
        let cause = classify_start_cause(&refused(json!({
            "errorCode": "provider_credentials_missing", "errorData": {"provider": "openai"}
        })))
        .expect("classified");
        assert_eq!(cause.cause_class, StartCauseClass::CredentialsMissing);
        assert_eq!(cause.provider.as_deref(), Some("openai"));
        assert_eq!(
            describe_start_cause(&cause, StartOperation::Prompt),
            "openai has no usable credentials (provider_credentials_missing); sign in to openai or add its API key"
        );
        let expired = classify_start_cause(&refused(
            json!({"errorCode": "provider_credentials_expired"}),
        ))
        .expect("classified");
        assert_eq!(expired.cause_class, StartCauseClass::CredentialsMissing);
        assert_eq!(expired.provider, None);
        assert!(
            describe_start_cause(&expired, StartOperation::Prompt)
                .starts_with("its provider has no usable credentials")
        );
    }

    #[test]
    fn other_refusals_timeouts_and_transport_loss_keep_their_classes() {
        let refusal = classify_start_cause(&refused(json!({"errorCode": "host_memory_pressure"})))
            .expect("classified");
        assert_eq!(refusal.cause_class, StartCauseClass::HostRefused);
        assert_eq!(
            describe_start_cause(&refusal, StartOperation::Prompt),
            "the prompt was refused (host_refused: host_memory_pressure)"
        );
        let timeout =
            classify_start_message(None, "Timeout waiting for response to prompt. Stderr: x")
                .expect("classified");
        assert_eq!(timeout.timed_out_command.as_deref(), Some("prompt"));
        assert_eq!(
            classify_start_message(Some("rpc_transport_gone"), "x").map(|c| c.cause_class),
            Some(StartCauseClass::TransportLost)
        );
        assert_eq!(classify_start_message(None, "boom"), None);
    }
}
