use super::{account_chain::AccountChain, verifier::OPS_RECORD};
use crate::sync::{
    crypto::{
        build::{BuildBatchInput, build_batch, sign_record},
        entry::SyncRecordFields,
        links::next_link,
        signatures::{decode_b64, sha256_b64},
        vendor::sync_format::object_types::{ObjectKind, SyncObjectRecord},
    },
    engine::{error::EngineError, ports::*},
    keyring_port::DeviceKeyring,
};
use std::rc::Rc;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealRefusal {
    NoEpochKey,
    UnverifiedBase,
    NotMember,
}
#[derive(Debug)]
pub struct SealRefusedError {
    pub reason: SealRefusal,
}
impl std::fmt::Display for SealRefusedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self.reason {
            SealRefusal::NoEpochKey => "no_epoch_key",
            SealRefusal::UnverifiedBase => "unverified_base",
            SealRefusal::NotMember => "not_member",
        };
        write!(f, "memory sync cannot seal a batch: {reason}")
    }
}
impl std::error::Error for SealRefusedError {}
pub struct ChainSealer {
    pub chain: Rc<tokio::sync::Mutex<AccountChain>>,
    pub keyring: Rc<dyn DeviceKeyring>,
    pub space_id: Vec<u8>,
    pub peer: Option<Rc<dyn Fn() -> String>>,
    pub now: Rc<dyn Fn() -> u64>,
}
pub fn create_chain_sealer(
    chain: Rc<tokio::sync::Mutex<AccountChain>>,
    keyring: Rc<dyn DeviceKeyring>,
    space_id: Vec<u8>,
    peer: Option<Rc<dyn Fn() -> String>>,
    now: Rc<dyn Fn() -> u64>,
) -> ChainSealer {
    ChainSealer {
        chain,
        keyring,
        space_id,
        peer,
        now,
    }
}
impl Sealer for ChainSealer {
    fn seal<'a>(
        &'a self,
        batch_id: &'a str,
        base_seq: u64,
        payload: &'a BatchPayload,
    ) -> PortFuture<'a, SealedBatch> {
        Box::pin(async move {
            let mut chain = self.chain.lock().await;
            chain.reach(base_seq).await?;
            let refused = |reason: &str| {
                EngineError::Seal(SealRefusedError {
                    reason: match reason {
                        "not_member" => SealRefusal::NotMember,
                        "no_epoch_key" => SealRefusal::NoEpochKey,
                        _ => SealRefusal::UnverifiedBase,
                    },
                })
            };
            if !chain.is_member() {
                return Err(refused("not_member"));
            }
            let epoch = chain
                .state()
                .authority()
                .ok_or_else(|| refused("not_member"))?
                .epoch;
            let prior = chain
                .link_at(base_seq)
                .ok_or_else(|| refused("unverified_base"))?
                .to_owned();
            let key = chain
                .epoch_key(epoch)
                .ok_or_else(|| refused("no_epoch_key"))?;
            let batch = decode_b64(batch_id, None).ok_or_else(|| refused("unverified_base"))?;
            use aws_lc_rs::rand::{SecureRandom, SystemRandom};
            let mut salt = [0; 32];
            SystemRandom::new()
                .fill(&mut salt)
                .map_err(|_| refused("no_epoch_key"))?;
            let peers: Vec<_> = self.peer.iter().map(|peer| peer()).collect();
            let records: Vec<_> = payload
                .items
                .iter()
                .map(|item| SyncObjectRecord {
                    r#type: OPS_RECORD,
                    space_id: self.space_id.clone(),
                    item_key: item.key.clone(),
                    body: item.ops.clone(),
                })
                .collect();
            let bytes = build_batch(&BuildBatchInput {
                account: &chain.account,
                kind: ObjectKind::Ops,
                epoch,
                epoch_key: &key,
                keyring: self.keyring.as_ref(),
                peers: &peers,
                batch_id: &batch,
                salt: &salt,
                created_at: (self.now)(),
                chunk: None,
                records: &records,
            })
            .map_err(|e| EngineError::Recovery(e.into()))?;
            let digest = sha256_b64(&bytes);
            let link = next_link(&prior, &digest).ok_or_else(|| refused("unverified_base"))?;
            let record_sig = sign_record(
                self.keyring.as_ref(),
                &SyncRecordFields {
                    account: chain.account.clone(),
                    base_seq,
                    digest,
                    epoch,
                    link: link.clone(),
                    keyring_link: None,
                },
            )
            .map_err(|e| EngineError::Recovery(e.into()))?;
            Ok(SealedBatch {
                bytes,
                epoch,
                link,
                record_sig,
            })
        })
    }
}
