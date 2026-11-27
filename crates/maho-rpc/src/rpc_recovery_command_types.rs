//! senpi `modes/rpc/rpc-recovery-command-types.ts`: the recovery commands of a socket host
//! (senpi#4272): pages under a checkpoint token or an event lease, and receipts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryCommandLeaseKind {
    Event,
    Checkpoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCommandLeaseRef {
    pub kind: RecoveryCommandLeaseKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RpcRecoveryCommand {
    CancelAttempt {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        admission_id: String,
    },
    ResetStream {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        admission_id: String,
    },
    /// One bounded page of a frozen checkpoint component or an event lease (R4.B3, S13).
    GetRecoveryPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lease: Option<RecoveryCommandLeaseRef>,
        admission_id: String,
        component_ref: String,
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    /// The consumer's receipt for an event lease, sent only after it committed the content (S13).
    ReleaseRecovery {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        lease: RecoveryCommandLeaseRef,
        admission_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deserializes_all_recovery_command_variants() {
        let cancel: RpcRecoveryCommand = serde_json::from_value(json!({
            "id": "1",
            "type": "cancel_attempt",
            "admissionId": "adm-1"
        }))
        .expect("cancel");
        assert!(matches!(
            cancel,
            RpcRecoveryCommand::CancelAttempt {
                admission_id,
                ..
            } if admission_id == "adm-1"
        ));

        let page: RpcRecoveryCommand = serde_json::from_value(json!({
            "type": "get_recovery_page",
            "lease": { "kind": "event", "id": "lease-1" },
            "admissionId": "adm-2",
            "componentRef": "record",
            "requestId": "req-1",
            "cursor": "p1:1:10:0000"
        }))
        .expect("page");
        assert!(matches!(
            page,
            RpcRecoveryCommand::GetRecoveryPage {
                lease: Some(RecoveryCommandLeaseRef {
                    kind: RecoveryCommandLeaseKind::Event,
                    ..
                }),
                ..
            }
        ));

        let release: RpcRecoveryCommand = serde_json::from_value(json!({
            "type": "release_recovery",
            "lease": { "kind": "event", "id": "lease-1" },
            "admissionId": "adm-2"
        }))
        .expect("release");
        assert!(matches!(
            release,
            RpcRecoveryCommand::ReleaseRecovery { .. }
        ));
    }
}
