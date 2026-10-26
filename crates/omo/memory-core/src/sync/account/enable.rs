use super::{account_chain::open_account_chain, http::AccountHttp, pairing_http::PairingHttp};
use crate::sync::{
    crypto::{
        anchor::ChainAnchor,
        rejection::RejectionCounter,
        trust_pin::{PinLoad, load_trust_anchor},
    },
    engine::error::{EngineError, EngineResult},
    keyring_port::DeviceKeyring,
};
use std::{path::Path, rc::Rc};
#[derive(Debug, PartialEq, Eq)]
pub enum EnableOutcome {
    Joined,
    JoinRequired,
    NeedsFirstDevice,
}
pub async fn enable_sync(
    account: &str,
    service_device_id: &str,
    keyring: Rc<dyn DeviceKeyring>,
    pin_dir: &Path,
    chain: AccountHttp,
    pairing: &PairingHttp,
) -> EngineResult<EnableOutcome> {
    pairing.set_account_sync(true).await?;
    pairing.set_device_sync(service_device_id, true).await?;
    if chain.list_keys().await?.is_none_or(|keys| keys.is_empty()) {
        return Ok(EnableOutcome::NeedsFirstDevice);
    }
    let PinLoad::Loaded { anchor } =
        load_trust_anchor(pin_dir, account).map_err(|e| EngineError::Recovery(e.into()))?
    else {
        return Ok(EnableOutcome::JoinRequired);
    };
    let chain = open_account_chain(
        account.into(),
        ChainAnchor::Pinned(anchor),
        keyring,
        chain,
        RejectionCounter::default(),
    )
    .await?;
    Ok(if chain.is_member() {
        EnableOutcome::Joined
    } else {
        EnableOutcome::JoinRequired
    })
}
