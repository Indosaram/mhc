use super::{
    rejection::RejectionCounter,
    signatures::*,
    strict_ed25519::{strict_ed25519_key, strict_ed25519_precheck},
    vendor::sync_format::{
        object::{decode_object, verify_object},
        object_types::*,
    },
};
pub struct BatchBinding<'a> {
    pub account: &'a str,
    pub epoch: u64,
    pub author_id: &'a str,
    pub signing_public_key: &'a str,
    pub digest: &'a str,
    pub kind: ObjectKind,
    pub chunk: Option<Chunk>,
}
pub type OpenFailureReason = &'static str;
#[derive(Debug)]
pub enum OpenOutcome {
    Opened {
        batch_id: String,
        records: Vec<SyncObjectRecord>,
    },
    Rejected {
        reason: OpenFailureReason,
    },
}
pub fn open_batch(
    bytes: &[u8],
    binding: &BatchBinding<'_>,
    epoch_key: &[u8],
    rejections: &mut RejectionCounter,
) -> OpenOutcome {
    let result = (|| {
        let key = decode_b64(binding.signing_public_key, Some(32))
            .filter(|k| strict_ed25519_key(k))
            .ok_or("bad_key")?;
        let ctx = ObjectContext {
            account: binding.account,
            epoch: binding.epoch,
            author_id: binding.author_id,
            author_signing_public_key: &key,
        };
        let header = verify_object(bytes, &ctx)?;
        if strict_ed25519_precheck(&key, &bytes[bytes.len().saturating_sub(64)..]).is_err() {
            return Err("bad_signature");
        }
        if header.kind != binding.kind || header.chunk != binding.chunk {
            return Err("kind_mismatch");
        }
        if sha256_b64(bytes) != binding.digest {
            return Err("digest_mismatch");
        }
        let records = decode_object(bytes, &ctx, epoch_key)?;
        Ok((header.batch_id, records))
    })();
    match result {
        Ok((batch_id, records)) => OpenOutcome::Opened { batch_id, records },
        Err(reason) => {
            rejections.record(reason);
            OpenOutcome::Rejected { reason }
        }
    }
}
