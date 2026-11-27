//! Port of senpi `packages/coding-agent/src/modes/rpc/client-admission-intake.ts`.
//!
//! Admits `prompt`, `steer`, and `follow_up` client inputs with idempotency (`clientMessageId`),
//! host-minted ids for queued id-less inputs, and fingerprint conflict checking.

use crate::client_admission_record::{
    AdmissionDisposition, ClientAdmissionRecord, ClientMessageIdConflict, HOST_MINTED_ID_PREFIX,
    PreparedAdmissionInput, admission_snapshot, client_input_fingerprint,
};
use crate::client_admissions::{AdmitError, ClientAdmissions};
use serde_json::Value;
use std::sync::Arc;

pub struct IntakeHooks<'a> {
    pub client_message_id: Option<String>,
    pub client_turn_id: Option<String>,
    admissions: &'a ClientAdmissions,
    durable_session_id: String,
    fingerprint: String,
    host_minted: bool,
    minted_id: String,
}

impl IntakeHooks<'_> {
    pub fn record_queued(&self, prepared: PreparedAdmissionInput) {
        self.admissions.save(ClientAdmissionRecord {
            client_message_id: self.minted_id.clone(),
            client_turn_id: self.client_turn_id.clone(),
            durable_session_id: self.durable_session_id.clone(),
            fingerprint: self.fingerprint.clone(),
            turn_id: None,
            host_minted: self.host_minted.then_some(true),
            disposition: AdmissionDisposition::Queued,
            state: crate::client_admission_record::AdmissionState::Queued { prepared },
        });
    }

    pub fn record_accepted(
        &self,
        disposition: AdmissionDisposition,
        turn_id: Option<&str>,
    ) -> Option<Value> {
        if let Some(existing) = self.admissions.record(&self.minted_id) {
            return Some(admission_snapshot(&existing));
        }
        let record = ClientAdmissionRecord {
            client_message_id: self.minted_id.clone(),
            client_turn_id: self.client_turn_id.clone(),
            durable_session_id: self.durable_session_id.clone(),
            fingerprint: self.fingerprint.clone(),
            turn_id: turn_id.map(str::to_owned),
            host_minted: self.host_minted.then_some(true),
            disposition,
            state: if disposition == AdmissionDisposition::Handled {
                crate::client_admission_record::AdmissionState::Completed { outcome: None }
            } else {
                crate::client_admission_record::AdmissionState::Running
            },
        };
        self.admissions.save(record.clone());
        Some(admission_snapshot(&record))
    }
}

/// Admits one client input through `admissions`, using a host-minted id for id-less queued inputs
/// (`steer`/`follow_up`) and returning the durable admission snapshot (or `None` when id-less).
pub async fn admit_client_input_via_intake<F, Fut>(
    admissions: &Arc<ClientAdmissions>,
    durable_session_id: &str,
    command: &Value,
    invoke: F,
) -> Result<Option<Value>, AdmitError>
where
    F: FnOnce(Option<String>, Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<AdmissionDisposition, String>>,
{
    let cmd_type = command.get("type").and_then(Value::as_str).unwrap_or("");
    let client_msg_id = command
        .get("clientMessageId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let client_turn_id = command
        .get("clientTurnId")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let host_minted = client_msg_id.is_none() && cmd_type != "prompt";
    if client_msg_id.is_none() && !host_minted {
        let _ = invoke(None, client_turn_id)
            .await
            .map_err(AdmitError::Failed)?;
        return Ok(None);
    }

    let effective_id = client_msg_id.clone().unwrap_or_else(|| {
        format!(
            "{HOST_MINTED_ID_PREFIX}{durable_session_id}:{}",
            uuid::Uuid::new_v4()
        )
    });
    let fingerprint = client_input_fingerprint(command);
    if let Some(existing) = admissions.record(&effective_id) {
        if existing.fingerprint != fingerprint {
            return Err(AdmitError::Conflict(ClientMessageIdConflict {
                client_message_id: effective_id,
            }));
        }
        return Ok(if host_minted {
            None
        } else {
            Some(admission_snapshot(&existing))
        });
    }

    let hooks = IntakeHooks {
        client_message_id: client_msg_id.clone(),
        client_turn_id: client_turn_id.clone(),
        admissions,
        durable_session_id: durable_session_id.to_owned(),
        fingerprint: fingerprint.clone(),
        host_minted,
        minted_id: effective_id.clone(),
    };
    let disposition = invoke(Some(effective_id.clone()), client_turn_id)
        .await
        .map_err(AdmitError::Failed)?;
    let _ = hooks.record_accepted(disposition, None);
    let saved = admissions.record(&effective_id).ok_or_else(|| {
        AdmitError::Failed(format!(
            "RPC input was {disposition:?} without a durable admission"
        ))
    })?;
    if host_minted {
        Ok(None)
    } else {
        Ok(Some(admission_snapshot(&saved)))
    }
}
