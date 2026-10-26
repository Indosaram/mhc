use super::{
    account_chain::open_account_chain, http::AccountHttp, pairing::*, pairing_http::PairingHttp,
};
use crate::sync::{
    crypto::{
        anchor::{ChainAnchor, PublicKeys, pairing_anchor},
        parse::Principal,
        rejection::RejectionCounter,
        trust_pin::pin_trust_anchor,
    },
    engine::{
        error::{EngineError, EngineResult},
        ports::PortFuture,
    },
    keyring_port::DeviceKeyring,
};
use std::{path::Path, rc::Rc};
pub trait CeremonyClock {
    fn now(&self) -> u64;
    fn sleep(&self, ms: u64) -> PortFuture<'_, ()>;
}
pub const POLL_MS: u64 = 2000;
#[derive(Debug, PartialEq, Eq)]
pub enum JoinOutcome {
    Joined,
    Expired,
    Refused(String),
}
pub fn principal_of(keyring: &dyn DeviceKeyring) -> Principal {
    let keys = keyring.public_keys();
    Principal {
        kind: "device".into(),
        id: keyring.principal().id,
        sealing_public_key: keys.sealing,
        signing_public_key: keys.signing,
    }
}
pub async fn join_account(
    account: &str,
    keyring: Rc<dyn DeviceKeyring>,
    pin_dir: &Path,
    chain: AccountHttp,
    pairing: &PairingHttp,
    clock: &dyn CeremonyClock,
    show_code: &dyn Fn(String) -> PortFuture<'static, ()>,
) -> EngineResult<JoinOutcome> {
    let own = principal_of(keyring.as_ref());
    let nonce = new_pairing_nonce().map_err(EngineError::Recovery)?;
    let commitment = approval_commitment(
        account,
        &PairingParty {
            principal: &own,
            nonce: &nonce,
        },
    )
    .map_err(EngineError::Recovery)?;
    let opened = pairing.open_pending(account, &commitment).await?;
    let mut row = opened.clone();
    let answer = loop {
        if let Some(answer) = row.answer {
            break answer;
        }
        if clock.now() >= opened.expires_at {
            return Ok(JoinOutcome::Expired);
        }
        clock.sleep(POLL_MS).await?;
        let Some(next) = pairing.read_pending(&opened.id).await? else {
            return Ok(JoinOutcome::Expired);
        };
        row = next;
    };
    pairing.reveal_pending(&opened.id, &own, &nonce).await?;
    show_code(
        approval_code(
            account,
            [
                PairingParty {
                    principal: &own,
                    nonce: &nonce,
                },
                PairingParty {
                    principal: &answer.principal,
                    nonce: &answer.approver_nonce,
                },
            ],
        )
        .map_err(EngineError::Recovery)?,
    )
    .await?;
    let anchor = ChainAnchor::Bootstrap(pairing_anchor(
        account.into(),
        keyring.as_ref(),
        PublicKeys {
            sealing_public_key: answer.principal.sealing_public_key,
            signing_public_key: answer.principal.signing_public_key,
        },
        nonce,
        answer.approver_nonce,
    ));
    loop {
        let admitted = chain.list_keys().await?.is_some_and(|keys| {
            keys.iter().any(|entry| {
                entry["body"]["kind"] == "approval" && entry["body"]["principal"]["id"] == own.id
            })
        });
        if admitted {
            let mut walked = open_account_chain(
                account.into(),
                anchor,
                keyring,
                chain,
                RejectionCounter::default(),
            )
            .await?;
            let Some(pin) = walked
                .state()
                .pin()
                .cloned()
                .filter(|_| walked.state().anchored())
            else {
                return Ok(JoinOutcome::Refused(
                    "the approval does not match this device's ceremony".into(),
                ));
            };
            let epoch = walked.state().authority().map_or(0, |a| a.epoch);
            if !walked.is_member() || walked.epoch_key(epoch).is_none() {
                return Ok(JoinOutcome::Refused(
                    "the approval admits this device without a key it can open".into(),
                ));
            }
            pin_trust_anchor(pin_dir, account, &pin)
                .map_err(|e| EngineError::Recovery(e.into()))?;
            return Ok(JoinOutcome::Joined);
        }
        if clock.now() >= opened.expires_at {
            return Ok(JoinOutcome::Expired);
        }
        clock.sleep(POLL_MS).await?;
    }
}
