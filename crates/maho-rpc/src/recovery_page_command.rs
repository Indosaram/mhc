//! senpi `modes/rpc/recovery-page-command.ts`: `get_recovery_page` (checkpoint token or event
//! lease) and `release_recovery` (event lease receipt) command handlers for a socket host.

use crate::compact_event_leases::EventLeases;
use crate::compact_recovery_requests::{
    RecoveryPageFailure, RecoveryPageRequest, RecoveryPageRequests, RequestRefusal,
    RequestRefusalReason,
};
use crate::compact_recovery_types::{
    RecoveryCaller, RecoveryFailure, RecoveryStreamState, RepairReason,
};
use crate::rpc_recovery_command_types::RecoveryCommandLeaseKind;
use crate::rpc_types::{RPC_ERROR_MISSING_SESSION_ID, RPC_ERROR_UNSUPPORTED_TRANSPORT};
use futures::future::BoxFuture;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::sync::OnceLock;

type HmacSha256 = Hmac<Sha256>;

fn repair_advice_key() -> &'static [u8; 32] {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    KEY.get_or_init(|| {
        let mut key = [0u8; 32];
        if getrandom::fill(&mut key).is_err() {
            let a = uuid::Uuid::new_v4();
            let b = uuid::Uuid::new_v4();
            key[..16].copy_from_slice(a.as_bytes());
            key[16..].copy_from_slice(b.as_bytes());
        }
        key
    })
}

fn token_retry_after_ms(token: &str) -> u64 {
    let mut mac = HmacSha256::new_from_slice(repair_advice_key())
        .unwrap_or_else(|e| panic!("HMAC key invariant: {e}"));
    mac.update(token.as_bytes());
    let digest = mac.finalize().into_bytes();
    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    100 + u64::from(n % 1_901)
}

fn random_retry_after_ms() -> u64 {
    let mut buf = [0u8; 4];
    let n = if getrandom::fill(&mut buf).is_ok() {
        u32::from_be_bytes(buf)
    } else {
        0
    };
    100 + u64::from(n % 1_901)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairSourceInfo {
    pub session_epoch: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryRepairAdvice {
    pub state: String,
    pub health_revision: Option<String>,
    pub source: Option<RepairSourceInfo>,
    pub hint: String,
    pub retry_after_ms: u64,
}

/// Host writer view needed by `answer_recovery_page` and `answer_release_recovery`.
pub trait RecoveryCommandWriter: Send + Sync {
    fn current_connection(&self) -> Option<String>;
    fn socket_epoch_of(&self, connection_id: &str) -> Option<String>;
    fn recovery_source_info(&self, session_id: &str) -> Option<RepairSourceInfo>;
    fn health_revision(&self, session_id: &str) -> BoxFuture<'_, Result<Option<String>, String>>;
    fn ordinary_route(&self, connection_id: &str, session_id: &str) -> Option<RecoveryStreamState>;
    fn is_compact_subscription(
        &self,
        connection_id: &str,
        admission_id: &str,
        session_id: &str,
    ) -> bool;
    fn unregister_subscription(&self, connection_id: &str, session_id: &str);
}

#[derive(Debug, Clone)]
pub struct RecoveryPageCommandInput {
    pub id: Option<String>,
    pub session_id: Option<String>,
    pub token: Option<String>,
    pub lease: Option<(String, String)>,
    pub admission_id: String,
    pub component_ref: String,
    pub request_id: String,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReleaseRecoveryCommandInput {
    pub id: Option<String>,
    pub session_id: Option<String>,
    pub lease_kind: Option<RecoveryCommandLeaseKind>,
    pub lease_id: Option<String>,
    pub admission_id: Option<String>,
}

fn error_response(id: Option<&str>, command: &str, code: &str, data: Option<Value>) -> Value {
    let mut resp = json!({
        "type": "response",
        "command": command,
        "success": false,
        "error": code,
        "errorCode": code,
    });
    if let Some(id) = id {
        resp["id"] = Value::String(id.to_owned());
    }
    if let Some(data) = data {
        resp["errorData"] = data;
    }
    resp
}

fn code_of(failure: &RecoveryPageFailure) -> String {
    match failure {
        RecoveryPageFailure::RequestRefused(r) => format!("recovery_{}", r.reason.as_str()),
        RecoveryPageFailure::Recovery(f) => format!("recovery_{}", f.kind_str()),
    }
}

fn caller_of(
    writer: &dyn RecoveryCommandWriter,
    id: Option<&str>,
    command: &str,
    session_id: Option<&str>,
) -> Result<RecoveryCaller, Value> {
    let Some(session_id) = session_id else {
        return Err(error_response(
            id,
            command,
            RPC_ERROR_MISSING_SESSION_ID,
            None,
        ));
    };
    let Some(conn) = writer.current_connection() else {
        return Err(error_response(
            id,
            command,
            RPC_ERROR_UNSUPPORTED_TRANSPORT,
            None,
        ));
    };
    let Some(socket_epoch) = writer.socket_epoch_of(&conn) else {
        return Err(error_response(
            id,
            command,
            RPC_ERROR_UNSUPPORTED_TRANSPORT,
            None,
        ));
    };
    Ok(RecoveryCaller {
        socket_epoch,
        session_id: session_id.to_owned(),
    })
}

fn refused_response(id: Option<&str>, command: &str, reason: RequestRefusalReason) -> Value {
    let refusal = RequestRefusal::new(reason);
    let code = format!("recovery_{}", reason.as_str());
    error_response(id, command, &code, serde_json::to_value(refusal).ok())
}

pub async fn answer_recovery_page(
    pages: &RecoveryPageRequests,
    writer: &dyn RecoveryCommandWriter,
    command: &RecoveryPageCommandInput,
) -> Value {
    let caller = match caller_of(
        writer,
        command.id.as_deref(),
        "get_recovery_page",
        command.session_id.as_deref(),
    ) {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    let req = RecoveryPageRequest {
        request_id: command.request_id.clone(),
        admission_id: command.admission_id.clone(),
        token: command.token.clone(),
        lease: command.lease.clone(),
        component_ref: command.component_ref.clone(),
        cursor: command.cursor.clone(),
    };
    let mut outcome = pages.request(&caller, &req, None).await;

    let is_foreign_or_refused = matches!(
        &outcome,
        Err(RecoveryPageFailure::RequestRefused(_))
            | Err(RecoveryPageFailure::Recovery(
                RecoveryFailure::RepairNeeded {
                    reason: RepairReason::UnknownToken,
                }
            ))
    );

    let source_info = if is_foreign_or_refused {
        None
    } else {
        writer.recovery_source_info(&caller.session_id)
    };
    let health_revision = if !is_foreign_or_refused {
        match writer.health_revision(&caller.session_id).await {
            Ok(rev) => rev,
            Err(_) => {
                outcome = Err(RecoveryPageFailure::Recovery(
                    crate::compact_recovery_types::storage_unavailable(
                        crate::compact_recovery_types::StorageUnavailableReason::Unavailable,
                    ),
                ));
                None
            }
        }
    } else {
        None
    };

    if let Err(RecoveryPageFailure::RequestRefused(ref r)) = outcome {
        let code = format!("recovery_{}", r.reason.as_str());
        return error_response(
            command.id.as_deref(),
            "get_recovery_page",
            &code,
            serde_json::to_value(r).ok(),
        );
    }

    let token_or_lease_id = command
        .lease
        .as_ref()
        .map(|(_, id)| id.as_str())
        .or(command.token.as_deref())
        .unwrap_or("");
    let (state_str, retry_after_ms) = match &outcome {
        Ok(_) => ("ok".to_owned(), 0),
        Err(RecoveryPageFailure::Recovery(f)) => {
            let ms = if matches!(
                f,
                RecoveryFailure::RepairNeeded {
                    reason: RepairReason::UnknownToken
                }
            ) {
                token_retry_after_ms(token_or_lease_id)
            } else {
                random_retry_after_ms()
            };
            (f.kind_str().to_owned(), ms)
        }
        Err(RecoveryPageFailure::RequestRefused(_)) => unreachable!(),
    };

    let repair = RecoveryRepairAdvice {
        state: state_str,
        health_revision,
        source: source_info,
        hint: "restart_from_catalog".into(),
        retry_after_ms,
    };

    let conn = writer.current_connection().unwrap_or_default();
    let ordinary = writer.ordinary_route(&conn, &caller.session_id);
    let ok_gen = outcome.as_ref().ok().and_then(|v| v.generation);
    let generation = ok_gen.or_else(|| {
        if ordinary
            .as_ref()
            .is_some_and(|o| o.routes.iter().any(|r| r == &command.admission_id))
        {
            ordinary.map(|o| o.generation)
        } else if writer.is_compact_subscription(&conn, &command.admission_id, &caller.session_id) {
            Some(0)
        } else {
            None
        }
    });

    match outcome {
        Ok(mut answer) => {
            answer.repair = serde_json::to_value(&repair).ok();
            let mut resp = json!({
                "type": "response",
                "command": "get_recovery_page",
                "success": true,
                "data": serde_json::to_value(&answer).unwrap_or(Value::Null),
            });
            if let Some(id) = &command.id {
                resp["id"] = Value::String(id.clone());
            }
            if let Some(gen_) = generation {
                resp["sessionId"] = Value::String(caller.session_id);
                resp["admissionId"] = Value::String(command.admission_id.clone());
                resp["generation"] = json!(gen_);
            }
            resp
        }
        Err(ref failure) => {
            let code = code_of(failure);
            let mut data = match failure {
                RecoveryPageFailure::Recovery(f) => {
                    serde_json::to_value(f).unwrap_or_else(|_| json!({}))
                }
                RecoveryPageFailure::RequestRefused(r) => {
                    serde_json::to_value(r).unwrap_or_else(|_| json!({}))
                }
            };
            if let Some(obj) = data.as_object_mut() {
                obj.insert(
                    "repair".into(),
                    serde_json::to_value(&repair).unwrap_or(Value::Null),
                );
            }
            error_response(
                command.id.as_deref(),
                "get_recovery_page",
                &code,
                Some(data),
            )
        }
    }
}

pub fn answer_release_recovery(
    events: &EventLeases,
    writer: &dyn RecoveryCommandWriter,
    command: &ReleaseRecoveryCommandInput,
) -> Value {
    let caller = match caller_of(
        writer,
        command.id.as_deref(),
        "release_recovery",
        command.session_id.as_deref(),
    ) {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    if command.lease_kind == Some(RecoveryCommandLeaseKind::Checkpoint) {
        return refused_response(
            command.id.as_deref(),
            "release_recovery",
            RequestRefusalReason::LeaseKindUnsupported,
        );
    }
    let (Some(RecoveryCommandLeaseKind::Event), Some(lease_id), Some(admission_id)) = (
        command.lease_kind,
        command.lease_id.as_deref(),
        command.admission_id.as_deref(),
    ) else {
        return refused_response(
            command.id.as_deref(),
            "release_recovery",
            RequestRefusalReason::MalformedRequest,
        );
    };

    match events.release(&caller, lease_id, admission_id) {
        Ok(receipt) => {
            let conn = writer.current_connection().unwrap_or_default();
            writer.unregister_subscription(&conn, &caller.session_id);
            let mut resp = json!({
                "type": "response",
                "command": "release_recovery",
                "success": true,
                "data": serde_json::to_value(&receipt).unwrap_or(Value::Null),
            });
            if let Some(id) = &command.id {
                resp["id"] = Value::String(id.clone());
            }
            resp
        }
        Err(failure) => {
            let code = format!("recovery_{}", failure.kind_str());
            error_response(
                command.id.as_deref(),
                "release_recovery",
                &code,
                serde_json::to_value(&failure).ok(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_record_staging::RecordContent;
    use crate::compact_recovery_state::RecoveryCheckpoints;
    use std::sync::Mutex;

    struct MockWriter {
        conn: Option<String>,
        epoch: Option<String>,
        unregistered: Mutex<Vec<String>>,
    }

    impl RecoveryCommandWriter for MockWriter {
        fn current_connection(&self) -> Option<String> {
            self.conn.clone()
        }
        fn socket_epoch_of(&self, _: &str) -> Option<String> {
            self.epoch.clone()
        }
        fn recovery_source_info(&self, _: &str) -> Option<RepairSourceInfo> {
            Some(RepairSourceInfo {
                session_epoch: "ep-1".into(),
                sequence: 7,
            })
        }
        fn health_revision(&self, _: &str) -> BoxFuture<'_, Result<Option<String>, String>> {
            Box::pin(async { Ok(Some("1".into())) })
        }
        fn ordinary_route(&self, _: &str, _: &str) -> Option<RecoveryStreamState> {
            None
        }
        fn is_compact_subscription(&self, _: &str, _: &str, _: &str) -> bool {
            true
        }
        fn unregister_subscription(&self, _: &str, session_id: &str) {
            self.unregistered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(session_id.to_owned());
        }
    }

    #[tokio::test]
    async fn answers_page_and_release_and_refuses_missing_session_or_checkpoint_release() {
        let checkpoints = RecoveryCheckpoints::default();
        let events = EventLeases::default();
        let pages = RecoveryPageRequests::new(checkpoints, None, Some(events.clone()));
        let writer = MockWriter {
            conn: Some("c1".into()),
            epoch: Some("se-1".into()),
            unregistered: Mutex::new(Vec::new()),
        };

        let missing = answer_recovery_page(
            &pages,
            &writer,
            &RecoveryPageCommandInput {
                id: Some("1".into()),
                session_id: None,
                token: Some("tok".into()),
                lease: None,
                admission_id: "adm-1".into(),
                component_ref: "manifest".into(),
                request_id: "req-1".into(),
                cursor: None,
            },
        )
        .await;
        assert_eq!(missing["errorCode"], RPC_ERROR_MISSING_SESSION_ID);

        let record = json!({"type":"message_end"});
        let lease = events
            .mint(
                "se-1",
                "s1",
                "adm-1",
                "ep-1",
                1,
                &record,
                RecordContent::Text("{}".into()),
                None,
            )
            .expect("mint");

        let page_resp = answer_recovery_page(
            &pages,
            &writer,
            &RecoveryPageCommandInput {
                id: Some("2".into()),
                session_id: Some("s1".into()),
                token: None,
                lease: Some(("event".into(), lease.id.clone())),
                admission_id: "adm-1".into(),
                component_ref: "record".into(),
                request_id: "req-2".into(),
                cursor: None,
            },
        )
        .await;
        assert_eq!(page_resp["success"], true);
        assert_eq!(page_resp["data"]["repair"]["state"], "ok");

        let bad_release = answer_release_recovery(
            &events,
            &writer,
            &ReleaseRecoveryCommandInput {
                id: Some("3".into()),
                session_id: Some("s1".into()),
                lease_kind: Some(RecoveryCommandLeaseKind::Checkpoint),
                lease_id: Some("tok".into()),
                admission_id: Some("adm-1".into()),
            },
        );
        assert_eq!(bad_release["errorCode"], "recovery_lease_kind_unsupported");

        let ok_release = answer_release_recovery(
            &events,
            &writer,
            &ReleaseRecoveryCommandInput {
                id: Some("4".into()),
                session_id: Some("s1".into()),
                lease_kind: Some(RecoveryCommandLeaseKind::Event),
                lease_id: Some(lease.id),
                admission_id: Some("adm-1".into()),
            },
        );
        assert_eq!(ok_release["success"], true);
        assert_eq!(ok_release["data"]["outcome"], "released");
    }
}
