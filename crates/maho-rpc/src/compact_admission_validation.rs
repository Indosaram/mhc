//! senpi `modes/rpc/compact-admission-validation.ts`: syntactic validation of compact
//! admission IDs and server-side caller-authority checks before any command side effect.

use crate::compact_admission_error::{CompactAdmissionError, UnknownAdmissionResponse};
use crate::compact_admissions::CompactAdmissions;
use crate::compact_recovery_ids::parse_bounded_id;
use maho_core::engine_socket_flag::compact_updates_enabled;
use serde_json::{Value, json};
use std::sync::Arc;

pub trait AdmissionValidationWriter: Send + Sync {
    fn current_connection(&self) -> Option<String>;
    fn socket_epoch_of(&self, connection_id: &str) -> Option<String>;
    fn holds_admission(
        &self,
        connection_id: &str,
        admission_id: &str,
        session_id: Option<&str>,
    ) -> bool;
}

#[derive(Debug, Clone)]
pub struct AdmissionValidationCommand {
    pub id: Option<String>,
    pub command_type: String,
    pub session_id: Option<String>,
    pub admission_id: Option<Value>,
}

fn refusal_response(id: Option<&str>, command: &str, code: &str, reason: &str) -> Value {
    let mut resp = json!({
        "type": "response",
        "command": command,
        "success": false,
        "error": code,
        "errorCode": code,
        "errorData": {
            "kind": "request_refused",
            "reason": reason,
            "hint": "new_request",
        },
    });
    if let Some(id) = id {
        resp["id"] = Value::String(id.to_owned());
    }
    resp
}

/// RULING-85 Q2: a present malformed ID is never an implicit legacy request.
pub fn invalid_compact_admission(command: &AdmissionValidationCommand) -> Option<Value> {
    if !compact_updates_enabled() {
        return None;
    }
    invalid_compact_admission_with_enabled(command, true)
}

pub fn invalid_compact_admission_with_enabled(
    command: &AdmissionValidationCommand,
    enabled: bool,
) -> Option<Value> {
    if !enabled {
        return None;
    }
    let requires_id =
        command.command_type == "cancel_attempt" || command.command_type == "reset_stream";
    let Some(raw_id) = command.admission_id.as_ref() else {
        return if requires_id {
            Some(refusal_response(
                command.id.as_deref(),
                &command.command_type,
                "admission_malformed_request",
                "malformed_request",
            ))
        } else {
            None
        };
    };
    if let Some(id_str) = raw_id.as_str()
        && parse_bounded_id(id_str).is_ok()
    {
        return None;
    }
    Some(refusal_response(
        command.id.as_deref(),
        &command.command_type,
        "admission_malformed_request",
        "malformed_request",
    ))
}

/// RULING-94: only the server-side caller's authority is examined, before any command side effect.
pub async fn unknown_compact_admission(
    admissions: Option<&Arc<CompactAdmissions>>,
    writer: &dyn AdmissionValidationWriter,
    command: &AdmissionValidationCommand,
) -> Option<Value> {
    let admissions = admissions?;
    let admission_id = command.admission_id.as_ref()?.as_str()?;
    // These commands introduce client IDs; their bounded attempt index owns retry/conflict/retirement.
    if matches!(
        command.command_type.as_str(),
        "open_session" | "claim_session" | "attach_session" | "subscribe_sessions"
    ) {
        return None;
    }
    // RULING-96: recovery IDs select token/lease authority, not the ordinary attempt ledger.
    if matches!(
        command.command_type.as_str(),
        "get_recovery_page" | "release_recovery"
    ) {
        return None;
    }
    let peer = writer.current_connection();
    let epoch = peer.as_deref().and_then(|p| writer.socket_epoch_of(p));
    let held = peer
        .as_deref()
        .is_some_and(|p| writer.holds_admission(p, admission_id, command.session_id.as_deref()));
    if held {
        return None;
    }
    let refusal = serde_json::to_value(UnknownAdmissionResponse::new(
        command.id.clone(),
        &command.command_type,
        admission_id,
    ))
    .unwrap_or(Value::Null);
    if command.command_type == "cancel_attempt"
        && let Some(epoch) = epoch
    {
        return match admissions.pending(&epoch, admission_id).await {
            Ok(true) => None,
            Ok(false) => Some(refusal),
            Err(CompactAdmissionError { reason }) => Some(refusal_response(
                command.id.as_deref(),
                &command.command_type,
                &format!("admission_{}", reason.as_str()),
                reason.as_str(),
            )),
        };
    }
    Some(refusal)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockWriter {
        held: bool,
    }

    impl AdmissionValidationWriter for MockWriter {
        fn current_connection(&self) -> Option<String> {
            Some("c1".into())
        }
        fn socket_epoch_of(&self, _: &str) -> Option<String> {
            Some("se-1".into())
        }
        fn holds_admission(&self, _: &str, _: &str, _: Option<&str>) -> bool {
            self.held
        }
    }

    #[test]
    fn rejects_malformed_or_missing_required_admission_id() {
        let bad = AdmissionValidationCommand {
            id: Some("1".into()),
            command_type: "prompt".into(),
            session_id: None,
            admission_id: Some(json!("bad/id")),
        };
        let resp = invalid_compact_admission_with_enabled(&bad, true).expect("refusal");
        assert_eq!(resp["errorCode"], "admission_malformed_request");

        let missing_cancel = AdmissionValidationCommand {
            id: Some("2".into()),
            command_type: "cancel_attempt".into(),
            session_id: None,
            admission_id: None,
        };
        assert!(invalid_compact_admission_with_enabled(&missing_cancel, true).is_some());
        assert!(invalid_compact_admission_with_enabled(&missing_cancel, false).is_none());
    }

    #[tokio::test]
    async fn refuses_unknown_held_admission_and_allows_introductory_commands() {
        let admissions = Arc::new(CompactAdmissions::default());
        let writer = MockWriter { held: false };
        let open_cmd = AdmissionValidationCommand {
            id: Some("1".into()),
            command_type: "open_session".into(),
            session_id: None,
            admission_id: Some(json!("adm-1")),
        };
        assert!(
            unknown_compact_admission(Some(&admissions), &writer, &open_cmd)
                .await
                .is_none()
        );

        let prompt_cmd = AdmissionValidationCommand {
            id: Some("2".into()),
            command_type: "prompt".into(),
            session_id: Some("s1".into()),
            admission_id: Some(json!("adm-unknown")),
        };
        let refusal = unknown_compact_admission(Some(&admissions), &writer, &prompt_cmd)
            .await
            .expect("refusal");
        assert_eq!(refusal["errorCode"], "unknown_admission");
        assert_eq!(refusal["admissionId"], "adm-unknown");
    }
}
