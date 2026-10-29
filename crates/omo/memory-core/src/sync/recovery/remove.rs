use super::{
    RecoveryError, key_material::EpochMaterial, keyring_append::*, ports::*, served_chain::*,
};
use crate::sync::{
    crypto::{
        anchor::{ChainAnchor, TrustAnchor},
        chain::ChainState,
        entry::Head,
        fold::Authority,
        rejection::RejectionCounter,
    },
    keyring_port::DeviceKeyring,
};
use serde::Serialize;
use serde_json::json;
#[derive(Debug, PartialEq, Eq)]
pub enum RemoveOutcome {
    Removed {
        epoch: u64,
        platform_signal: PlatformSignal,
        delete_from_passkey_manager: bool,
    },
    Refused(RemoveRefusal),
    RotationPending {
        platform_signal: PlatformSignal,
        delete_from_passkey_manager: bool,
    },
}
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum RemoveRefusal {
    Transport(TransportRefusal),
    Local(RemoveLocalRefusal),
}
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoveLocalRefusal {
    Unavailable,
    Unverified,
    NotMember,
    NotEnrolled,
    ConfirmLastKey,
    WrapRefused,
}
pub struct RemoveInput<'a> {
    pub anchor: TrustAnchor,
    pub account: &'a str,
    pub rp_id: &'a str,
    pub credential_id: &'a str,
    pub confirm_giving_up_recovery: bool,
    pub keyring: &'a dyn DeviceKeyring,
    pub bridge: Option<&'a dyn RecoveryBridge>,
    pub transport: &'a dyn RecoveryTransport,
    pub rejections: &'a mut RejectionCounter,
    pub on_event: EventSink<'a>,
}
fn local(reason: RemoveLocalRefusal) -> RemoveOutcome {
    RemoveOutcome::Refused(RemoveRefusal::Local(reason))
}
fn removal_pending(state: &ChainState, authority: &Authority, credential: &str) -> bool {
    !authority.registry.contains_key(credential)
        && state.received().iter().any(|entry| {
            entry.entry.body["kind"] == "recovery-key-removed"
                && entry.entry.body["credential_id"] == credential
                && entry.entry.fields.epoch == authority.epoch
        })
}
pub async fn remove_recovery_key(
    mut input: RemoveInput<'_>,
) -> Result<RemoveOutcome, RecoveryError> {
    let state = match load_served_chain(
        input.transport,
        input.account,
        input.rejections,
        ChainAnchor::Pinned(input.anchor.clone()),
    )
    .await?
    {
        Ok(state) => state,
        Err(ServedRefusal::Unavailable) => return Ok(local(RemoveLocalRefusal::Unavailable)),
        Err(ServedRefusal::Unverified | ServedRefusal::AnchorMismatch) => {
            return Ok(local(RemoveLocalRefusal::Unverified));
        }
    };
    let (Some(authority), Some(head)) = (state.authority(), state.head()) else {
        return Ok(local(RemoveLocalRefusal::NotMember));
    };
    let id = input.keyring.principal().id;
    if authority
        .members
        .get(&id)
        .is_none_or(|p| p.signing_public_key != input.keyring.public_keys().signing)
    {
        return Ok(local(RemoveLocalRefusal::NotMember));
    }
    let resuming = removal_pending(&state, authority, input.credential_id);
    if !resuming && !authority.registry.contains_key(input.credential_id) {
        return Ok(local(RemoveLocalRefusal::NotEnrolled));
    }
    if !resuming && authority.registry.len() == 1 && !input.confirm_giving_up_recovery {
        return Ok(local(RemoveLocalRefusal::ConfirmLastKey));
    }
    let retained =
        committed_wrap(&state, authority.epoch, &format!("device:{id}")).and_then(|wrap| {
            own_material(
                input.keyring,
                input.account,
                &wrap,
                &expected_history(authority, authority.epoch),
            )
        });
    let Some(retained) = retained else {
        return Ok(local(RemoveLocalRefusal::WrapRefused));
    };
    remove_with_material(&mut input, head, authority.epoch, resuming, &retained).await
}
async fn remove_with_material(
    input: &mut RemoveInput<'_>,
    head: &Head,
    epoch: u64,
    resuming: bool,
    retained: &EpochMaterial,
) -> Result<RemoveOutcome, RecoveryError> {
    if !resuming {
        let body = json!({"kind":"recovery-key-removed","account":input.account,"epoch":epoch,"credential_id":input.credential_id});
        if let Err(reason) = input
            .transport
            .append_keys(signed_append(input.keyring, head, body, Vec::new())?)
            .await?
        {
            return Ok(RemoveOutcome::Refused(RemoveRefusal::Transport(reason)));
        }
    }
    let platform_signal = match input.bridge {
        Some(bridge) => {
            bridge
                .signal_removed(input.rp_id, input.credential_id)
                .await?
        }
        None => PlatformSignal::Unsupported,
    };
    let pending = || RemoveOutcome::RotationPending {
        platform_signal,
        delete_from_passkey_manager: true,
    };
    let after = load_served_chain(
        input.transport,
        input.account,
        input.rejections,
        ChainAnchor::Pinned(input.anchor.clone()),
    )
    .await?;
    let Ok(after) = after else {
        return Ok(pending());
    };
    let (Some(authority), Some(head)) = (after.authority(), after.head()) else {
        return Ok(pending());
    };
    if authority.epoch != epoch || authority.registry.contains_key(input.credential_id) {
        return Ok(pending());
    }
    let members: Vec<_> = authority.members.values().cloned().collect();
    let registry: Vec<_> = authority.registry.values().cloned().collect();
    let rotated = input
        .transport
        .append_keys(rotation_append(RotationInput {
            keyring: input.keyring,
            head,
            account: input.account,
            authority,
            retained,
            members: &members,
            registry: &registry,
        })?)
        .await?;
    if rotated.is_err() {
        return Ok(pending());
    }
    let stored = load_served_chain(
        input.transport,
        input.account,
        input.rejections,
        ChainAnchor::Pinned(input.anchor.clone()),
    )
    .await?;
    let Ok(stored) = stored else {
        return Ok(pending());
    };
    let next = epoch
        .checked_add(1)
        .ok_or(RecoveryError::InvalidInput("epoch overflow"))?;
    if !stored
        .authority()
        .is_some_and(|a| a.epoch == next && !a.registry.contains_key(input.credential_id))
    {
        return Ok(pending());
    }
    if let Some(event) = input.on_event {
        event(RecoveryEvent::Removed);
    }
    Ok(RemoveOutcome::Removed {
        epoch: next,
        platform_signal,
        delete_from_passkey_manager: true,
    })
}
