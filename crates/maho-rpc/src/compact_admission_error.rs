//! senpi `modes/rpc/compact-admission-error.ts`: typed admission error reasons and the
//! `unknown_admission` response shape.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnknownAdmissionErrorData {
    pub code: String,
    pub admission_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnknownAdmissionResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub command: String,
    pub success: bool,
    pub error: String,
    pub error_code: String,
    pub error_data: UnknownAdmissionErrorData,
    pub code: String,
    pub admission_id: String,
}

impl UnknownAdmissionResponse {
    pub fn new(
        id: Option<String>,
        command: impl Into<String>,
        admission_id: impl Into<String>,
    ) -> Self {
        let admission_id = admission_id.into();
        Self {
            id,
            kind: "response".into(),
            command: command.into(),
            success: false,
            error: "unknown_admission".into(),
            error_code: "unknown_admission".into(),
            error_data: UnknownAdmissionErrorData {
                code: "unknown_admission".into(),
                admission_id: admission_id.clone(),
            },
            code: "unknown_admission".into(),
            admission_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactAdmissionReason {
    MalformedRequest,
    AdmissionConflict,
    AdmissionRetired,
    AdmissionUnknown,
    AdmissionLimit,
    AdmissionBusy,
    StorageUnavailable,
}

impl CompactAdmissionReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MalformedRequest => "malformed_request",
            Self::AdmissionConflict => "admission_conflict",
            Self::AdmissionRetired => "admission_retired",
            Self::AdmissionUnknown => "admission_unknown",
            Self::AdmissionLimit => "admission_limit",
            Self::AdmissionBusy => "admission_busy",
            Self::StorageUnavailable => "storage_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", .reason.as_str())]
pub struct CompactAdmissionError {
    pub reason: CompactAdmissionReason,
}

impl CompactAdmissionError {
    pub const fn new(reason: CompactAdmissionReason) -> Self {
        Self { reason }
    }
}

impl From<CompactAdmissionReason> for CompactAdmissionError {
    fn from(reason: CompactAdmissionReason) -> Self {
        Self::new(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_strings_and_unknown_response_match_wire_contract() {
        let err = CompactAdmissionError::new(CompactAdmissionReason::AdmissionRetired);
        assert_eq!(err.to_string(), "admission_retired");

        let resp = UnknownAdmissionResponse::new(Some("req-1".into()), "prompt", "adm-1");
        let json = serde_json::to_value(&resp).expect("serialize");
        assert_eq!(json["type"], "response");
        assert_eq!(json["success"], false);
        assert_eq!(json["error"], "unknown_admission");
        assert_eq!(json["errorCode"], "unknown_admission");
        assert_eq!(json["errorData"]["admissionId"], "adm-1");
    }
}
