use super::{RecoveryError, ports::*, served_chain::*};
use crate::sync::crypto::{
    anchor::{ChainAnchor, PinCandidate},
    entry::Head,
    fold::Authority,
    parse::{Principal, RecoveryKey},
    rejection::RejectionCounter,
};
pub struct ConfirmedJoin {
    pub authority: Authority,
    pub head: Head,
    pub pin: PinCandidate,
    pub used_key: RecoveryKey,
}
#[derive(Debug, PartialEq, Eq)]
pub enum JoinRefusal {
    Transport(TransportRefusal),
    JoinUnconfirmed,
}
pub struct ConfirmJoinInput<'a> {
    pub transport: &'a dyn RecoveryTransport,
    pub account: &'a str,
    pub rejections: &'a mut RejectionCounter,
    pub anchor: ChainAnchor,
    pub head: &'a Head,
    pub recovered: &'a RecoveredKey,
}
pub async fn append_and_confirm_join(
    input: ConfirmJoinInput<'_>,
) -> Result<Result<ConfirmedJoin, JoinRefusal>, RecoveryError> {
    let join = &input.recovered.join;
    let appended = input
        .transport
        .recovery_join(KeyringAppendRequest {
            record: join.body.clone(),
            base_seq: input.head.seq,
            link: join.link.clone(),
            keyring_link: join.keyring_link.clone(),
            record_sig: join.record_sig.clone(),
            wraps: Vec::new(),
        })
        .await?;
    if let Err(reason) = appended {
        return Ok(Err(JoinRefusal::Transport(reason)));
    }
    let Ok(state) = load_served_chain(
        input.transport,
        input.account,
        input.rejections,
        input.anchor,
    )
    .await?
    else {
        return Ok(Err(JoinRefusal::JoinUnconfirmed));
    };
    let (Some(authority), Some(head), Some(pin)) = (state.authority(), state.head(), state.pin())
    else {
        return Ok(Err(JoinRefusal::JoinUnconfirmed));
    };
    let Some(used_key) = authority.registry.get(&input.recovered.credential_id) else {
        return Ok(Err(JoinRefusal::JoinUnconfirmed));
    };
    if head.keyring_link != join.keyring_link {
        return Ok(Err(JoinRefusal::JoinUnconfirmed));
    }
    Ok(Ok(ConfirmedJoin {
        authority: authority.clone(),
        head: head.clone(),
        pin: pin.clone(),
        used_key: used_key.clone(),
    }))
}
/// Confirms an acknowledged rotation on a fresh anchored reload: the verified keyring must contain
/// the rotation this device sent, at `epoch`, and this device must still be a member at the head.
/// The head may be a later record that descends from our rotation, which is not a fork.
pub async fn rotation_stored(
    transport: &dyn RecoveryTransport,
    account: &str,
    rejections: &mut RejectionCounter,
    anchor: ChainAnchor,
    sent: &KeyringAppendRequest,
    epoch: u64,
    device: &Principal,
) -> bool {
    let Ok(Ok(state)) = load_served_chain(transport, account, rejections, anchor).await else {
        return false;
    };
    let Some(authority) = state.authority() else {
        return false;
    };
    let Some(ours) = state.received().iter().find(|entry| {
        entry.entry.fields.keyring_link.as_deref() == Some(sent.keyring_link.as_str())
    }) else {
        return false;
    };
    ours.entry.fields.epoch == epoch
        && authority.epoch >= epoch
        && !authority.cancelled.contains(&ours.entry.fields.digest)
        && authority
            .members
            .get(&device.id)
            .is_some_and(|p| p.signing_public_key == device.signing_public_key)
}
