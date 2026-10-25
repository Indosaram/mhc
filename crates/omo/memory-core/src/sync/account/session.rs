use super::{
    account_chain::open_account_chain, http::create_account_http, sealer::create_chain_sealer,
    verifier::create_chain_verifier,
};
use crate::sync::{
    crypto::{
        anchor::ChainAnchor,
        rejection::RejectionCounter,
        trust_pin::{PinLoad, load_trust_anchor},
    },
    engine::{
        adapters::http_transport::{HttpTransportOptions, create_http_transport},
        error::{EngineError, EngineResult},
        open::SyncSession,
    },
    keyring_port::DeviceKeyring,
};
use std::{path::PathBuf, rc::Rc};
#[derive(Debug, PartialEq, Eq)]
pub enum AccountSessionIdle {
    NoPin,
    CorruptPin,
    NotMember,
}
pub enum AccountSessionOutcome {
    Session(SyncSession),
    Idle(AccountSessionIdle),
}
pub struct AccountSessionOptions {
    pub http: HttpTransportOptions,
    pub account: String,
    pub keyring: Rc<dyn DeviceKeyring>,
    pub pin_dir: PathBuf,
    pub space_id: Vec<u8>,
    pub peer: Option<Rc<dyn Fn() -> String>>,
    pub now: Option<Rc<dyn Fn() -> u64>>,
}
pub async fn open_account_session(
    options: AccountSessionOptions,
) -> EngineResult<AccountSessionOutcome> {
    if options.space_id.len() != 16 {
        return Err(EngineError::Other(
            "openAccountSession: a space id is 16 bytes".into(),
        ));
    }
    let anchor = match load_trust_anchor(&options.pin_dir, &options.account)
        .map_err(|e| EngineError::Recovery(e.into()))?
    {
        PinLoad::Loaded { anchor } => anchor,
        PinLoad::Refused { reason, .. } => {
            return Ok(AccountSessionOutcome::Idle(if reason == "no_pin" {
                AccountSessionIdle::NoPin
            } else {
                AccountSessionIdle::CorruptPin
            }));
        }
    };
    let chain = open_account_chain(
        options.account,
        ChainAnchor::Pinned(anchor),
        options.keyring.clone(),
        create_account_http(options.http.clone()),
        RejectionCounter::default(),
    )
    .await?;
    if !chain.is_member() {
        return Ok(AccountSessionOutcome::Idle(AccountSessionIdle::NotMember));
    }
    let chain = Rc::new(tokio::sync::Mutex::new(chain));
    Ok(AccountSessionOutcome::Session(SyncSession {
        transport: Rc::new(create_http_transport(options.http)),
        verifier: Rc::new(create_chain_verifier(
            chain.clone(),
            options.space_id.clone(),
            RejectionCounter::default(),
        )),
        sealer: Rc::new(create_chain_sealer(
            chain,
            options.keyring,
            options.space_id,
            options.peer,
            options
                .now
                .unwrap_or_else(|| Rc::new(|| crate::support::time::now_millis().max(0) as u64)),
        )),
    }))
}
