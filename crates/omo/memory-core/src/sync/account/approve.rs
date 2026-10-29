use super::{
    account_chain::open_account_chain,
    http::AccountHttp,
    join::{CeremonyClock, POLL_MS, principal_of},
    pairing::*,
    pairing_http::PairingHttp,
};
use crate::sync::{
    crypto::{
        anchor::ChainAnchor,
        links::wrap_set_digest,
        rejection::RejectionCounter,
        trust_pin::{PinLoad, load_trust_anchor},
    },
    engine::{
        error::{EngineError, EngineResult},
        ports::PortFuture,
    },
    keyring_port::DeviceKeyring,
    recovery::{
        key_material::{WrapAddress, seal_wrap},
        keyring_append::{own_material, signed_append},
        ports::TransportRefusal,
        served_chain::{committed_wrap, expected_history},
    },
};
use serde_json::json;
use std::{path::Path, rc::Rc};
#[derive(Debug, PartialEq, Eq)]
pub enum ApproveOutcome {
    Approved(u64),
    Expired,
    Declined,
    Refused(String),
}
pub struct ApprovalConfirmation<'a> {
    pub pending_id: &'a str,
    pub confirm_code: &'a dyn Fn(String) -> PortFuture<'static, bool>,
}

pub async fn approve_device(
    account: &str,
    keyring: Rc<dyn DeviceKeyring>,
    pin_dir: &Path,
    chain: AccountHttp,
    pairing: &PairingHttp,
    clock: &dyn CeremonyClock,
    confirmation: ApprovalConfirmation<'_>,
) -> EngineResult<ApproveOutcome> {
    let ApprovalConfirmation { pending_id, confirm_code } = confirmation;
    let PinLoad::Loaded { anchor } =
        load_trust_anchor(pin_dir, account).map_err(|e| EngineError::Recovery(e.into()))?
    else {
        return Ok(ApproveOutcome::Refused(
            "this device has not joined the account's memory".into(),
        ));
    };
    let own = principal_of(keyring.as_ref());
    let nonce = new_pairing_nonce().map_err(EngineError::Recovery)?;
    let answered = pairing.answer_pending(pending_id, &own, &nonce).await?;
    let mut row = answered.clone();
    let joining = loop {
        if let Some(reveal) = row.reveal {
            break reveal;
        }
        if clock.now() >= answered.expires_at {
            return Ok(ApproveOutcome::Expired);
        }
        clock.sleep(POLL_MS).await?;
        let Some(next) = pairing.read_pending(pending_id).await? else {
            return Ok(ApproveOutcome::Expired);
        };
        row = next;
    };
    if approval_commitment(
        account,
        &PairingParty {
            principal: &joining.principal,
            nonce: &joining.joining_nonce,
        },
    )
    .map_err(EngineError::Recovery)?
        != answered.commitment
    {
        return Ok(ApproveOutcome::Refused(
            "the joining device revealed keys its commitment did not name".into(),
        ));
    }
    if !confirm_code(
        approval_code(
            account,
            [
                PairingParty {
                    principal: &own,
                    nonce: &nonce,
                },
                PairingParty {
                    principal: &joining.principal,
                    nonce: &joining.joining_nonce,
                },
            ],
        )
        .map_err(EngineError::Recovery)?,
    )
    .await?
    {
        return Ok(ApproveOutcome::Declined);
    }
    for _ in 0..3 {
        let verified = open_account_chain(
            account.into(),
            ChainAnchor::Pinned(anchor.clone()),
            keyring.clone(),
            chain.clone(),
            RejectionCounter::default(),
        )
        .await?;
        let state = verified.state();
        let (Some(authority), Some(head)) = (state.authority(), state.head()) else {
            return Ok(ApproveOutcome::Refused(
                "this device is not a member of the account's memory".into(),
            ));
        };
        if !verified.is_member() {
            return Ok(ApproveOutcome::Refused(
                "this device is not a member of the account's memory".into(),
            ));
        }
        let material = committed_wrap(state, authority.epoch, &format!("device:{}", own.id))
            .and_then(|wrap| {
                own_material(
                    keyring.as_ref(),
                    account,
                    &wrap,
                    &expected_history(authority, authority.epoch),
                )
            });
        let Some(material) = material else {
            return Ok(ApproveOutcome::Refused(
                "this device cannot open the current epoch key".into(),
            ));
        };
        let wraps = vec![
            seal_wrap(
                &WrapAddress {
                    account,
                    epoch: authority.epoch,
                    id: &format!("device:{}", joining.principal.id),
                },
                &joining.principal.sealing_public_key,
                &material,
            )
            .map_err(EngineError::Recovery)?,
        ];
        let Some(digest) = wrap_set_digest(&wraps) else {
            return Ok(ApproveOutcome::Refused(
                "the approval wraps do not digest".into(),
            ));
        };
        let body = json!({"kind":"approval","account":account,"epoch":authority.epoch,"principal":joining.principal,"wraps_digest":digest,"pairing":{"commitment":answered.commitment,"joining_nonce":joining.joining_nonce,"approver_nonce":nonce}});
        match chain
            .append_keys(
                signed_append(keyring.as_ref(), head, body, wraps)
                    .map_err(EngineError::Recovery)?,
            )
            .await?
        {
            Ok(seq) => return Ok(ApproveOutcome::Approved(seq)),
            Err(TransportRefusal::StaleBase) => {}
            Err(reason) => {
                return Ok(ApproveOutcome::Refused(format!(
                    "the service refused the approval ({reason:?})"
                )));
            }
        }
    }
    Ok(ApproveOutcome::Refused(
        "the account kept changing while approving; try again".into(),
    ))
}
