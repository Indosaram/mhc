//! Port of senpi `packages/coding-agent/src/modes/rpc/delegation-wire.ts`.
//!
//! The answers the delegated-connection gate gives (`senpi#5632`, `senpi#4249` R-1b d1): an ordinary
//! response in the router's error shape, or, for a private presenter record on a delegated
//! connection, the private reply shape `handleMcpForm` writes.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A response record the gate answers with, in the router's error shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegationResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub record_type: String,
    pub command: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_data: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl DelegationResponse {
    pub fn success(id: Option<String>, command: impl Into<String>, data: Value) -> Self {
        Self {
            id,
            record_type: "response".to_owned(),
            command: command.into(),
            success: true,
            data: Some(data),
            error: None,
            error_code: None,
            error_data: None,
            extra: Map::new(),
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// The refusal codes of omo-relay-spec decision 0018 (`delegationRefusal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationRefusal {
    DelegationMalformed,
    DelegationUnclassified,
    DelegationLocalOnly,
    DelegationNotGranted,
    DelegationUnauthenticated,
    DelegationLate,
    DelegationFixed,
}

impl DelegationRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DelegationMalformed => "delegation_malformed",
            Self::DelegationUnclassified => "delegation_unclassified",
            Self::DelegationLocalOnly => "delegation_local_only",
            Self::DelegationNotGranted => "delegation_not_granted",
            Self::DelegationUnauthenticated => "delegation_unauthenticated",
            Self::DelegationLate => "delegation_late",
            Self::DelegationFixed => "delegation_fixed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateRefusal {
    DelegationUnclassified,
    DelegationNotGranted,
    PresenterNotAuthorized,
}

impl PrivateRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DelegationUnclassified => "delegation_unclassified",
            Self::DelegationNotGranted => "delegation_not_granted",
            Self::PresenterNotAuthorized => "presenter_not_authorized",
        }
    }
}

/// `senpi#6752`: a `machine_op` record on a connection that may not run device operations, or one
/// that does not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineOpRefusal {
    MachineOpNotAuthorized,
    MachineOpMalformed,
    MachineOpConflict,
    MachineOpUnavailable,
}

impl MachineOpRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MachineOpNotAuthorized => "machine_op_not_authorized",
            Self::MachineOpMalformed => "machine_op_malformed",
            Self::MachineOpConflict => "machine_op_conflict",
            Self::MachineOpUnavailable => "machine_op_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineOpCommand {
    MachineOp,
    MachineOpResult,
    MachineOpCancel,
}

impl MachineOpCommand {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MachineOp => "machine_op",
            Self::MachineOpResult => "machine_op_result",
            Self::MachineOpCancel => "machine_op_cancel",
        }
    }
}

pub fn is_machine_op_command(record_type: &str) -> Option<MachineOpCommand> {
    match record_type {
        "machine_op" => Some(MachineOpCommand::MachineOp),
        "machine_op_result" => Some(MachineOpCommand::MachineOpResult),
        "machine_op_cancel" => Some(MachineOpCommand::MachineOpCancel),
        _ => None,
    }
}

pub fn machine_op_refusal(id: Option<String>, command: MachineOpCommand) -> DelegationResponse {
    refused(
        id,
        command.as_str(),
        MachineOpRefusal::MachineOpNotAuthorized.as_str(),
        &format!(
            "{} needs a connection that proved the endpoint's launch key and declared no delegation",
            command.as_str()
        ),
        None,
    )
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum DelegationVerdict {
    Pass,
    Consumed,
    Reply {
        response: DelegationResponse,
    },
    Private {
        id: Option<String>,
        error: PrivateRefusal,
    },
    Presenter {
        id: Option<String>,
    },
    ControlInfo {
        id: Option<String>,
    },
    MachineOp {
        id: Option<String>,
        command: MachineOpCommand,
    },
}

pub fn request_id(record: &Value) -> Option<String> {
    record
        .as_object()?
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub fn refused(
    id: Option<String>,
    command: impl Into<String>,
    code: &str,
    message: &str,
    data: Option<Value>,
) -> DelegationResponse {
    DelegationResponse {
        id,
        record_type: "response".to_owned(),
        command: command.into(),
        success: false,
        data: None,
        error: Some(format!("{code}: {message}")),
        error_code: Some(code.to_owned()),
        error_data: data,
        extra: Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn refused_formats_error_and_omits_absent_fields() {
        let resp = refused(
            Some("req-1".to_owned()),
            "prompt",
            DelegationRefusal::DelegationNotGranted.as_str(),
            "prompt needs the drive grant",
            Some(json!({"class": "drive"})),
        );
        let value = resp.to_value();
        assert_eq!(value["id"], "req-1");
        assert_eq!(value["type"], "response");
        assert_eq!(value["command"], "prompt");
        assert_eq!(value["success"], false);
        assert_eq!(
            value["error"],
            "delegation_not_granted: prompt needs the drive grant"
        );
        assert_eq!(value["errorCode"], "delegation_not_granted");
        assert_eq!(value["errorData"], json!({"class": "drive"}));
        assert!(value.get("data").is_none());
    }

    #[test]
    fn request_id_requires_string_id_on_object() {
        assert_eq!(request_id(&json!({"id": "abc"})), Some("abc".to_owned()));
        assert_eq!(request_id(&json!({"id": 42})), None);
        assert_eq!(request_id(&json!(["id"])), None);
    }

    #[test]
    fn machine_op_command_recognition_and_refusal() {
        assert_eq!(
            is_machine_op_command("machine_op"),
            Some(MachineOpCommand::MachineOp)
        );
        assert_eq!(
            is_machine_op_command("machine_op_result"),
            Some(MachineOpCommand::MachineOpResult)
        );
        assert_eq!(
            is_machine_op_command("machine_op_cancel"),
            Some(MachineOpCommand::MachineOpCancel)
        );
        assert_eq!(is_machine_op_command("prompt"), None);

        let r = machine_op_refusal(None, MachineOpCommand::MachineOp);
        assert_eq!(r.error_code.as_deref(), Some("machine_op_not_authorized"));
    }
}
