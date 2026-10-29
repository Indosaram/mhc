use super::{
    RecoveryError, keyring_append::signed_append, ports::*, salt_inputs::MAX_RECOVERY_KEYS,
    served_chain::*,
};
use crate::sync::{
    crypto::{
        anchor::{ChainAnchor, TrustAnchor},
        rejection::RejectionCounter,
    },
    keyring_port::DeviceKeyring,
};
use serde::Serialize;
use serde_json::json;

#[derive(Debug, PartialEq, Eq)]
pub enum EnrollOutcome {
    Enrolled { credential_id: String, seq: u64 },
    Refused(EnrollReason),
}
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum EnrollReason {
    Bridge(EnrollRefusal),
    Transport(TransportRefusal),
    Local(EnrollLocalRefusal),
}
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnrollLocalRefusal {
    Unavailable,
    Unverified,
    NotMember,
    TooManyRecoveryKeys,
}
pub struct EnrollInput<'a> {
    pub anchor: TrustAnchor,
    pub account: &'a str,
    pub rp_id: &'a str,
    pub user_name: &'a str,
    pub roaming: bool,
    pub keyring: &'a dyn DeviceKeyring,
    pub bridge: &'a dyn RecoveryBridge,
    pub transport: &'a dyn RecoveryTransport,
    pub rejections: &'a mut RejectionCounter,
    pub on_event: EventSink<'a>,
}
pub async fn enroll_recovery_key(
    mut input: EnrollInput<'_>,
) -> Result<EnrollOutcome, RecoveryError> {
    let outcome = enroll_inner(&mut input).await?;
    if let Some(event) = input.on_event {
        event(match &outcome {
            EnrollOutcome::Enrolled { .. } => RecoveryEvent::Enrolled,
            EnrollOutcome::Refused(_) => RecoveryEvent::EnrollRefused,
        });
    }
    Ok(outcome)
}
async fn enroll_inner(input: &mut EnrollInput<'_>) -> Result<EnrollOutcome, RecoveryError> {
    let refuse = |reason| EnrollOutcome::Refused(EnrollReason::Local(reason));
    let state = match load_served_chain(
        input.transport,
        input.account,
        input.rejections,
        ChainAnchor::Pinned(input.anchor.clone()),
    )
    .await?
    {
        Ok(state) => state,
        Err(ServedRefusal::Unavailable) => return Ok(refuse(EnrollLocalRefusal::Unavailable)),
        Err(ServedRefusal::Unverified | ServedRefusal::AnchorMismatch) => {
            return Ok(refuse(EnrollLocalRefusal::Unverified));
        }
    };
    let (Some(authority), Some(head)) = (state.authority(), state.head()) else {
        return Ok(refuse(EnrollLocalRefusal::NotMember));
    };
    let id = input.keyring.principal().id;
    if authority
        .members
        .get(&id)
        .map(|p| p.signing_public_key.as_str())
        != Some(input.keyring.public_keys().signing.as_str())
    {
        return Ok(refuse(EnrollLocalRefusal::NotMember));
    }
    let Some(device_wrap) = committed_wrap(&state, authority.epoch, &format!("device:{id}")) else {
        return Ok(refuse(EnrollLocalRefusal::NotMember));
    };
    if authority.registry.len() >= MAX_RECOVERY_KEYS {
        return Ok(refuse(EnrollLocalRefusal::TooManyRecoveryKeys));
    }
    let enrolled = match input
        .bridge
        .enroll(EnrollBridgeRequest {
            account: input.account.into(),
            rp_id: input.rp_id.into(),
            user_name: input.user_name.into(),
            roaming: input.roaming,
            used_credential_ids: authority.enrolled_ever.iter().cloned().collect(),
            custody_root: authority.custody_root.clone(),
            predecessor: head.keyring_link.clone(),
            epoch: authority.epoch,
            device_wrap,
            expected_history: expected_history(authority, authority.epoch),
        })
        .await?
    {
        Ok(enrolled) => enrolled,
        Err(reason) => return Ok(EnrollOutcome::Refused(EnrollReason::Bridge(reason))),
    };
    let mut body = json!(enrolled.key);
    body["kind"] = "recovery-key".into();
    body["account"] = input.account.into();
    body["epoch"] = authority.epoch.into();
    body["countersign"] = enrolled.countersign.into();
    body["wraps_digest"] = enrolled.wraps_digest.into();
    match input
        .transport
        .append_keys(signed_append(
            input.keyring,
            head,
            body,
            vec![enrolled.wrap],
        )?)
        .await?
    {
        Ok(seq) => Ok(EnrollOutcome::Enrolled {
            credential_id: enrolled.key.credential_id,
            seq,
        }),
        Err(reason) => {
            input
                .bridge
                .signal_removed(input.rp_id, &enrolled.key.credential_id)
                .await?;
            Ok(EnrollOutcome::Refused(EnrollReason::Transport(reason)))
        }
    }
}
