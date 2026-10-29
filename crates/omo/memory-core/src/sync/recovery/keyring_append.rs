use super::{RecoveryError, derive::random, key_material::*, ports::KeyringAppendRequest};
use crate::sync::{
    crypto::{
        build::sign_record,
        entry::*,
        fold::Authority,
        links::*,
        parse::{Principal, RecoveryKey},
        signatures::*,
        unwrap::{WrapOutcome, open_wrap},
    },
    keyring_port::DeviceKeyring,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use zeroize::Zeroizing;

pub fn signed_append(
    keyring: &dyn DeviceKeyring,
    head: &Head,
    body: Value,
    wraps: Vec<WrapEntry>,
) -> Result<KeyringAppendRequest, RecoveryError> {
    let digest = sha256_b64(&canonical(&body)?);
    let link = next_link(&head.link, &digest).ok_or(RecoveryError::InvalidInput(
        "the served head does not decode",
    ))?;
    let keyring_link = next_link(&head.keyring_link, &digest).ok_or(
        RecoveryError::InvalidInput("the served head does not decode"),
    )?;
    let fields = SyncRecordFields {
        account: body["account"]
            .as_str()
            .ok_or(RecoveryError::InvalidInput("missing append account"))?
            .into(),
        base_seq: head.seq,
        digest,
        epoch: body["epoch"]
            .as_u64()
            .ok_or(RecoveryError::InvalidInput("missing append epoch"))?,
        link: link.clone(),
        keyring_link: Some(keyring_link.clone()),
    };
    Ok(KeyringAppendRequest {
        record: body,
        base_seq: head.seq,
        link,
        keyring_link,
        record_sig: sign_record(keyring, &fields)?,
        wraps,
    })
}
pub struct RotationInput<'a> {
    pub keyring: &'a dyn DeviceKeyring,
    pub head: &'a Head,
    pub account: &'a str,
    pub authority: &'a Authority,
    pub retained: &'a EpochMaterial,
    pub members: &'a [Principal],
    pub registry: &'a [RecoveryKey],
}
pub fn rotation_append(input: RotationInput<'_>) -> Result<KeyringAppendRequest, RecoveryError> {
    let epoch = input
        .authority
        .epoch
        .checked_add(1)
        .ok_or(RecoveryError::InvalidInput("epoch overflow"))?;
    let mut history: Vec<_> = input
        .retained
        .history
        .iter()
        .map(|row| HistoryKey {
            epoch: row.epoch,
            origin: row.origin.clone(),
            key: row.key.clone(),
        })
        .collect();
    history.push(HistoryKey {
        epoch: input.retained.epoch,
        origin: input
            .authority
            .epoch_origins
            .get(&input.authority.epoch)
            .cloned()
            .unwrap_or_default(),
        key: input.retained.current.clone(),
    });
    let material = EpochMaterial {
        epoch,
        current: Zeroizing::new(random::<32>()?.to_vec()),
        history,
    };
    let mut recipients: Vec<_> = input
        .members
        .iter()
        .map(|p| (format!("device:{}", p.id), p.sealing_public_key.as_str()))
        .chain(input.registry.iter().map(|p| {
            (
                format!("recovery:{}", p.credential_id),
                p.sealing_public_key.as_str(),
            )
        }))
        .collect();
    recipients.sort_by(|a, b| a.0.cmp(&b.0));
    let wraps = recipients
        .iter()
        .map(|(id, key)| {
            seal_wrap(
                &WrapAddress {
                    account: input.account,
                    epoch,
                    id,
                },
                key,
                &material,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let digest = wrap_set_digest(&wraps).ok_or(RecoveryError::InvalidInput(
        "a rotation wrap set holds a duplicate recipient",
    ))?;
    let mut members = input.members.to_vec();
    members.sort_by(|a, b| a.id.cmp(&b.id));
    signed_append(
        input.keyring,
        input.head,
        json!({"kind":"rotation","account":input.account,"epoch":epoch,"members":members,"wraps_digest":digest,"fork_resolution":null}),
        wraps,
    )
}
pub fn own_material(
    keyring: &dyn DeviceKeyring,
    account: &str,
    wrap: &VerifiedWrap,
    expected: &BTreeMap<u64, String>,
) -> Option<EpochMaterial> {
    match open_wrap(keyring, wrap, account, expected) {
        WrapOutcome::Opened { keys } => Some(from_opened(
            wrap.entry().epoch,
            keys.current,
            keys.history,
            expected,
        )),
        WrapOutcome::Rejected { .. } => None,
    }
}
