pub use super::vendor::sync_format::snapshot::SnapshotEnvelope as SnapshotRecord;
use super::{
    anchor::ChainAnchor,
    chain::{ChainState, at_verified_head, empty_chain, verify_keyring_entry},
    chain_checks::LinkMode,
    entry::*,
    fold::is_cancelled_admission,
    open::{BatchBinding, OpenOutcome, open_batch},
    rejection::RejectionCounter,
    signatures::*,
    strict_ed25519::strict_ed25519_precheck,
    vendor::sync_format::{
        object::verify_object, object_types::*, snapshot::import_snapshot as verify_snapshot,
    },
};
pub type SnapshotRejection = &'static str;
#[derive(Debug)]
pub enum SnapshotOutcome {
    Imported {
        state: Box<ChainState>,
        records: Vec<SyncObjectRecord>,
    },
    Rejected {
        reason: SnapshotRejection,
    },
}
pub fn import_snapshot(
    account: &str,
    anchor: ChainAnchor,
    keyring_entries: &[KeyringEntry],
    snapshot: &SnapshotRecord,
    chunks: &[Vec<u8>],
    epoch_key_for: impl Fn(u64) -> Option<Vec<u8>>,
    rejections: &mut RejectionCounter,
) -> SnapshotOutcome {
    let result = (|| {
        if snapshot.account != account {
            return Err(("account_mismatch", true));
        }
        let covered: Vec<_> = keyring_entries
            .iter()
            .filter(|e| e.seq <= snapshot.covers_seq)
            .collect();
        let mut state = empty_chain(account.into(), anchor);
        for entry in &covered {
            state = verify_keyring_entry(&state, entry, LinkMode::KeyringOnly)
                .map_err(|r| {
                    (
                        match r {
                            "untrusted_root" | "anchor_mismatch" => r,
                            _ => "keyring_prefix_refused",
                        },
                        true,
                    )
                })?
                .0;
        }
        if !state.anchored() {
            return Err(("unanchored", true));
        }
        let authority = state.authority().ok_or(("keyring_prefix_refused", true))?;
        if is_cancelled_admission(authority, &snapshot.author.id) {
            return Err(("not_member", true));
        }
        let wire: Vec<_> = covered
            .iter()
            .map(|e| SyncEntry::Keyring((*e).clone()).wire())
            .collect();
        let headers = verify_snapshot(snapshot, chunks, &wire).map_err(|r| {
            (
                match r {
                    "snapshot_chain_refused" => "keyring_prefix_refused",
                    "snapshot_keyring_head" => "bad_keyring_head",
                    "snapshot_author_not_member" => "not_member",
                    "snapshot_bad_record_sig" => "bad_signature",
                    "snapshot_digest" => "digest_mismatch",
                    _ => "chunk_refused",
                },
                true,
            )
        })?;
        let author_key =
            decode_b64(&snapshot.author.signing_public_key, Some(32)).unwrap_or_default();
        let record_sig = decode_b64(&snapshot.record_sig, Some(64)).unwrap_or_default();
        if strict_ed25519_precheck(&author_key, &record_sig).is_err() {
            return Err(("bad_signature", true));
        }
        let epoch = headers.first().map_or(0, |h| h.epoch);
        let key = decode_b64(&snapshot.author.signing_public_key, Some(32))
            .ok_or(("chunk_refused", true))?;
        let ctx = ObjectContext {
            account: &snapshot.account,
            epoch,
            author_id: &snapshot.author.id,
            author_signing_public_key: &key,
        };
        let mut placed = vec![None; chunks.len()];
        for chunk in chunks {
            let h = verify_object(chunk, &ctx).map_err(|_| ("chunk_refused", true))?;
            let index = h
                .chunk
                .and_then(|c| usize::try_from(c.index).ok())
                .ok_or(("chunk_refused", true))?;
            let slot = placed.get_mut(index).ok_or(("chunk_refused", true))?;
            *slot = Some(chunk);
        }
        let epoch_key = epoch_key_for(epoch).ok_or(("chunk_refused", true))?;
        let mut records = Vec::new();
        for (index, chunk) in placed.iter().enumerate() {
            let chunk = chunk.ok_or(("chunk_refused", true))?;
            let binding = BatchBinding {
                account: &snapshot.account,
                epoch,
                author_id: &snapshot.author.id,
                signing_public_key: &snapshot.author.signing_public_key,
                digest: &sha256_b64(chunk),
                kind: ObjectKind::Snapshot,
                chunk: Some(Chunk {
                    index: u64::try_from(index).map_err(|_| ("chunk_refused", true))?,
                    count: u64::try_from(placed.len()).map_err(|_| ("chunk_refused", true))?,
                }),
            };
            match open_batch(chunk, &binding, &epoch_key, rejections) {
                OpenOutcome::Opened {
                    records: opened, ..
                } => records.extend(opened),
                OpenOutcome::Rejected { .. } => return Err(("chunk_refused", false)),
            }
        }
        let state = at_verified_head(
            &state,
            Head {
                seq: snapshot.covers_seq,
                link: snapshot.link.clone(),
                keyring_link: snapshot.keyring_head.clone(),
            },
        )
        .ok_or(("keyring_prefix_refused", true))?;
        Ok((state, records))
    })();
    match result {
        Ok((state, records)) => SnapshotOutcome::Imported {
            state: Box::new(state),
            records,
        },
        Err((reason, count)) => {
            if count {
                rejections.record(reason);
            }
            SnapshotOutcome::Rejected { reason }
        }
    }
}
