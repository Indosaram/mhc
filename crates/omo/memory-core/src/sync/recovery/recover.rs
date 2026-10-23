use super::{
    RecoveryError,
    key_material::{WrapAddress, open_bridge_sealed_wrap},
    keyring_append::{RotationInput, rotation_append},
    ports::*,
    recover_join::*,
    salt_inputs::*,
    served_chain::*,
    write_hold::*,
};
use crate::sync::{
    crypto::{
        anchor::*, parse::Principal, rejection::RejectionCounter, signatures::device_id_of,
        trust_pin::*,
    },
    keyring_port::DeviceKeyring,
};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum RecoverOutcome {
    Recovered { epoch: u64, credential_id: String },
    JoinedReadOnly { credential_id: String },
    Refused(RecoverRefusalReason),
    PinConflict { next: &'static str },
    PinCorrupt { next: &'static str },
}
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum RecoverRefusalReason {
    Bridge(RecoverRefusal),
    Transport(TransportRefusal),
    Local(RecoverLocalRefusal),
}
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoverLocalRefusal {
    ApprovalRequired,
    Unavailable,
    Unverified,
    NoRecoveryKey,
    TooManyRecoveryKeys,
    BadDevice,
    JoinRefused,
    JoinUnconfirmed,
}
pub struct RecoverInput<'a> {
    pub account: &'a str,
    pub rp_id: &'a str,
    pub keyring: &'a dyn DeviceKeyring,
    pub bridge: Option<&'a dyn RecoveryBridge>,
    pub transport: &'a dyn RecoveryTransport,
    pub rejections: &'a mut RejectionCounter,
    pub state_dir: &'a Path,
    pub acknowledge_unknown_credential: bool,
    pub on_event: EventSink<'a>,
}
struct AbortHandle<'a> {
    bridge: &'a dyn RecoveryBridge,
    handle: &'a str,
}
impl Drop for AbortHandle<'_> {
    fn drop(&mut self) {
        self.bridge.recover_abort(self.handle);
    }
}
fn local(reason: RecoverLocalRefusal) -> RecoverOutcome {
    RecoverOutcome::Refused(RecoverRefusalReason::Local(reason))
}
pub async fn recover_on_fresh_device(
    mut input: RecoverInput<'_>,
) -> Result<RecoverOutcome, RecoveryError> {
    let result = recover_inner(&mut input).await?;
    if matches!(
        result,
        RecoverOutcome::Refused(_)
            | RecoverOutcome::PinConflict { .. }
            | RecoverOutcome::PinCorrupt { .. }
    ) && let Some(event) = input.on_event
    {
        event(RecoveryEvent::RecoverRefused);
    }
    Ok(result)
}
async fn recover_inner(input: &mut RecoverInput<'_>) -> Result<RecoverOutcome, RecoveryError> {
    let Some(bridge) = input.bridge.filter(|bridge| bridge.prf_supported()) else {
        return Ok(local(RecoverLocalRefusal::ApprovalRequired));
    };
    let keys = input.keyring.public_keys();
    let id = input.keyring.principal().id;
    if device_id_of(&keys.signing).as_deref() != Some(&id) {
        return Ok(local(RecoverLocalRefusal::BadDevice));
    }
    let pinned = match load_trust_anchor(input.state_dir, input.account)? {
        PinLoad::Loaded { anchor } => Some(anchor.genesis_digest().to_owned()),
        PinLoad::Refused {
            reason: "corrupt_pin",
            ..
        } => {
            return Ok(RecoverOutcome::PinCorrupt {
                next: ANCHOR_NEXT_STEP,
            });
        }
        PinLoad::Refused { .. } => None,
    };
    let salts = match served_salt_inputs(input.transport).await? {
        Ok(salts) if salts.is_empty() => return Ok(local(RecoverLocalRefusal::NoRecoveryKey)),
        Ok(salts) => salts,
        Err(SaltRefusal::Unavailable) => return Ok(local(RecoverLocalRefusal::Unavailable)),
        Err(SaltRefusal::TooManyRecoveryKeys) => {
            return Ok(local(RecoverLocalRefusal::TooManyRecoveryKeys));
        }
    };
    let known_credential_ids = salts
        .iter()
        .map(|salt| salt.credential_id.clone())
        .collect();
    let begun = match bridge
        .recover_begin(RecoverBeginRequest {
            account: input.account.into(),
            rp_id: input.rp_id.into(),
            salts,
            known_credential_ids,
            acknowledge_unknown: input.acknowledge_unknown_credential,
        })
        .await?
    {
        Ok(begun) => begun,
        Err(reason) => {
            return Ok(RecoverOutcome::Refused(RecoverRefusalReason::Bridge(
                reason,
            )));
        }
    };
    let _abort = AbortHandle {
        bridge,
        handle: &begun.handle,
    };
    let device = Principal {
        kind: "device".into(),
        id,
        sealing_public_key: keys.sealing,
        signing_public_key: keys.signing,
    };
    recover_with_handle(input, bridge, &begun, pinned.as_deref(), device).await
}
async fn recover_with_handle(
    input: &mut RecoverInput<'_>,
    bridge: &dyn RecoveryBridge,
    begun: &BegunRecovery,
    pinned: Option<&str>,
    device: Principal,
) -> Result<RecoverOutcome, RecoveryError> {
    let anchor = ChainAnchor::Bootstrap(recovery_anchor(
        input.account.into(),
        begun.credential_id.clone(),
        PublicKeys {
            sealing_public_key: begun.sealing_public_key.clone(),
            signing_public_key: begun.signing_public_key.clone(),
        },
    ));
    let served = load_served_chain(
        input.transport,
        input.account,
        input.rejections,
        anchor.clone(),
    )
    .await?;
    let wrap_rows = input.transport.list_recovery_wraps().await?;
    let state = match served {
        Ok(state) => state,
        Err(ServedRefusal::AnchorMismatch) => {
            return Ok(RecoverOutcome::Refused(RecoverRefusalReason::Bridge(
                RecoverRefusal::KeyMismatch,
            )));
        }
        Err(ServedRefusal::Unavailable) => return Ok(local(RecoverLocalRefusal::Unavailable)),
        Err(ServedRefusal::Unverified) => return Ok(local(RecoverLocalRefusal::Unverified)),
    };
    let Some(wrap_rows) = wrap_rows.as_array() else {
        return Ok(local(RecoverLocalRefusal::Unavailable));
    };
    let (Some(authority), Some(head), Some(pin)) = (state.authority(), state.head(), state.pin())
    else {
        return Ok(local(RecoverLocalRefusal::Unverified));
    };
    if pinned.is_some_and(|digest| digest != pin.genesis_digest()) {
        return Ok(RecoverOutcome::PinConflict {
            next: ANCHOR_NEXT_STEP,
        });
    }
    let Some(candidate) = recovery_candidates(&state, wrap_rows)
        .into_iter()
        .find(|candidate| candidate.key.credential_id == begun.credential_id)
    else {
        return Ok(RecoverOutcome::Refused(RecoverRefusalReason::Bridge(
            RecoverRefusal::NotEnrolled,
        )));
    };
    let recovered = match bridge
        .recover_finish(RecoverFinishRequest {
            handle: begun.handle.clone(),
            account: input.account.into(),
            candidate,
            device: device.clone(),
            join: JoinBase {
                epoch: authority.epoch,
                keyring_head: head.keyring_link.clone(),
                base_seq: head.seq,
                prior_link: head.link.clone(),
            },
        })
        .await?
    {
        Ok(recovered) => recovered,
        Err(reason) => {
            return Ok(RecoverOutcome::Refused(RecoverRefusalReason::Bridge(
                reason,
            )));
        }
    };
    hold_writes(input.state_dir, input.account, &recovered.credential_id)?;
    let confirmed = match append_and_confirm_join(ConfirmJoinInput {
        transport: input.transport,
        account: input.account,
        rejections: input.rejections,
        anchor: anchor.clone(),
        head,
        recovered: &recovered,
    })
    .await?
    {
        Ok(confirmed) => confirmed,
        Err(JoinRefusal::Transport(reason)) => {
            return Ok(RecoverOutcome::Refused(RecoverRefusalReason::Transport(
                reason,
            )));
        }
        Err(JoinRefusal::JoinUnconfirmed) => {
            return Ok(local(RecoverLocalRefusal::JoinUnconfirmed));
        }
    };
    match pin_trust_anchor(input.state_dir, input.account, &confirmed.pin) {
        Ok(_) => {}
        Err(TrustPinError::Refused(TrustPinProblem::PinConflict)) => {
            return Ok(RecoverOutcome::PinConflict {
                next: ANCHOR_NEXT_STEP,
            });
        }
        Err(error) => return Err(error.into()),
    }
    let expected = expected_history(&confirmed.authority, authority.epoch);
    let retained = match open_bridge_sealed_wrap(
        input.keyring,
        &recovered.device_wrap,
        &WrapAddress {
            account: input.account,
            epoch: authority.epoch,
            id: &format!("device:{}", device.id),
        },
        &expected,
    ) {
        Ok(retained) => retained,
        Err(_) => {
            return Ok(RecoverOutcome::Refused(RecoverRefusalReason::Bridge(
                RecoverRefusal::WrapRefused,
            )));
        }
    };
    let rotation = rotation_append(RotationInput {
        keyring: input.keyring,
        head: &confirmed.head,
        account: input.account,
        authority: &confirmed.authority,
        retained: &retained,
        members: std::slice::from_ref(&device),
        registry: std::slice::from_ref(&confirmed.used_key),
    })?;
    let rotated = input.transport.append_keys(rotation.clone()).await?;
    drop(retained);
    let read_only = || {
        if let Some(event) = input.on_event {
            event(RecoveryEvent::JoinedReadOnly);
        }
        RecoverOutcome::JoinedReadOnly {
            credential_id: recovered.credential_id.clone(),
        }
    };
    if rotated.is_err() {
        return Ok(read_only());
    }
    let next_epoch = confirmed
        .authority
        .epoch
        .checked_add(1)
        .ok_or(RecoveryError::InvalidInput("epoch overflow"))?;
    if !rotation_stored(
        input.transport,
        input.account,
        input.rejections,
        anchor,
        &rotation,
        next_epoch,
        &device,
    )
    .await
    {
        return Ok(read_only());
    }
    release_write_hold(input.state_dir, input.account)?;
    if let Some(event) = input.on_event {
        event(RecoveryEvent::Recovered);
    }
    Ok(RecoverOutcome::Recovered {
        epoch: next_epoch,
        credential_id: recovered.credential_id,
    })
}
