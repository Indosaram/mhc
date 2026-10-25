use super::account_chain::{AccountChain, ChainPosition};
use crate::sync::{
    crypto::{
        open::{BatchBinding, OpenOutcome, open_batch},
        rejection::RejectionCounter,
        vendor::sync_format::object_types::{ObjectKind, SyncObjectRecord},
    },
    engine::{adapters::http_transport::hex_of_b64url, ports::*},
};
use std::{cell::RefCell, rc::Rc};
pub const OPS_RECORD: u8 = 1;
pub fn space_items(records: &[SyncObjectRecord], space_id: &[u8], record_type: u8) -> BatchPayload {
    BatchPayload {
        items: records
            .iter()
            .filter(|r| r.r#type == record_type && r.space_id == space_id)
            .map(|r| PayloadItem {
                key: r.item_key.clone(),
                ops: r.body.clone(),
            })
            .collect(),
    }
}
pub struct ChainVerifier {
    pub chain: Rc<tokio::sync::Mutex<AccountChain>>,
    pub space_id: Vec<u8>,
    pub rejections: RefCell<RejectionCounter>,
}
pub fn create_chain_verifier(
    chain: Rc<tokio::sync::Mutex<AccountChain>>,
    space_id: Vec<u8>,
    rejections: RejectionCounter,
) -> ChainVerifier {
    ChainVerifier {
        chain,
        space_id,
        rejections: RefCell::new(rejections),
    }
}
fn refusal(reason: &str) -> RefusalCode {
    match reason {
        "link_mismatch" => RefusalCode::ChainLink,
        "bad_signature" | "not_member" | "author_mismatch" => RefusalCode::Signature,
        "epoch_mismatch" => RefusalCode::Epoch,
        "account_mismatch" => RefusalCode::AccountBinding,
        "digest_mismatch" => RefusalCode::ContentHash,
        "kind_mismatch" => RefusalCode::ObjectBinding,
        _ => RefusalCode::Malformed,
    }
}
impl Verifier for ChainVerifier {
    fn verify_entry<'a>(
        &'a self,
        entry: &'a PulledEntry,
        bytes: &'a [u8],
    ) -> PortFuture<'a, Verdict> {
        Box::pin(async move {
            let mut chain = self.chain.lock().await;
            Ok(match chain.reach(entry.seq).await? {
                ChainPosition::Refused(reason) => Verdict::Refused(refusal(reason)),
                ChainPosition::Pending | ChainPosition::Unknown => {
                    Verdict::Refused(RefusalCode::SequenceGap)
                }
                ChainPosition::Keyring => {
                    if entry.kind == EntryKind::Keyring {
                        Verdict::Ok {
                            payload: BatchPayload::default(),
                            batch_id: format!("keyring:{}", entry.seq),
                        }
                    } else {
                        Verdict::Refused(RefusalCode::ObjectBinding)
                    }
                }
                ChainPosition::Vetoed => {
                    if entry.kind == EntryKind::Batch {
                        Verdict::Ok {
                            payload: BatchPayload::default(),
                            batch_id: entry
                                .batch_id
                                .clone()
                                .unwrap_or_else(|| format!("vetoed:{}", entry.seq)),
                        }
                    } else {
                        Verdict::Refused(RefusalCode::ObjectBinding)
                    }
                }
                ChainPosition::Batch(batch) => {
                    if entry.kind != EntryKind::Batch
                        || hex_of_b64url(&batch.digest) != entry.digest
                    {
                        return Ok(Verdict::Refused(RefusalCode::ContentHash));
                    }
                    let Some(key) = chain.epoch_key(batch.epoch) else {
                        return Ok(Verdict::Refused(RefusalCode::Epoch));
                    };
                    match open_batch(
                        bytes,
                        &BatchBinding {
                            account: &chain.account,
                            epoch: batch.epoch,
                            author_id: &batch.author_id,
                            signing_public_key: &batch.signing_public_key,
                            digest: &batch.digest,
                            kind: ObjectKind::Ops,
                            chunk: None,
                        },
                        &key,
                        &mut self.rejections.borrow_mut(),
                    ) {
                        OpenOutcome::Opened { batch_id, records } => Verdict::Ok {
                            batch_id,
                            payload: space_items(&records, &self.space_id, OPS_RECORD),
                        },
                        OpenOutcome::Rejected { reason } => Verdict::Refused(refusal(reason)),
                    }
                }
            })
        })
    }
    fn verify_snapshot<'a>(
        &'a self,
        _snapshot: &'a SnapshotRef,
        _chunks: &'a [Vec<u8>],
    ) -> PortFuture<'a, Verdict> {
        Box::pin(async move {
            self.rejections.borrow_mut().record("snapshot_unsupported");
            Ok(Verdict::Refused(RefusalCode::Malformed))
        })
    }
}
