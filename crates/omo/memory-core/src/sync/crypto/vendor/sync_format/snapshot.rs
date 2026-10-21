use super::super::super::{parse::Principal, signatures::*};
use super::{object::verify_object, object_types::*, receive::receive_keyring_chain};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEnvelope {
    pub account: String,
    pub author: Principal,
    pub covers_seq: u64,
    pub digest: String,
    pub link: String,
    pub keyring_head: String,
    pub record_sig: String,
}
pub fn snapshot_preimage(s: &SnapshotEnvelope) -> Value {
    json!({"t":"omo-sync/v1/snapshot","account":s.account,"author":s.author,"covers_seq":s.covers_seq,"digest":s.digest,"link":s.link,"keyring_head":s.keyring_head})
}
pub fn import_snapshot(
    snapshot: &SnapshotEnvelope,
    chunks: &[Vec<u8>],
    entries: &[Value],
) -> Result<Vec<SyncObjectHeader>, &'static str> {
    let covered: Vec<_> = entries
        .iter()
        .filter(|e| e["seq"].as_u64().is_some_and(|n| n <= snapshot.covers_seq))
        .cloned()
        .collect();
    let fold = receive_keyring_chain(&covered).map_err(|_| "snapshot_chain_refused")?;
    if covered
        .iter()
        .rev()
        .find(|e| e["kind"] == "keyring")
        .and_then(|e| e["keyring_link"].as_str())
        != Some(&snapshot.keyring_head)
    {
        return Err("snapshot_keyring_head");
    }
    let member = fold
        .members
        .get(&snapshot.author.id)
        .ok_or("snapshot_author_not_member")?;
    if member["signing_public_key"] != snapshot.author.signing_public_key
        || member["sealing_public_key"] != snapshot.author.sealing_public_key
        || !verify_tagged(
            "omo-sync/v1/snapshot",
            &snapshot_preimage(snapshot),
            &snapshot.author.signing_public_key,
            &snapshot.record_sig,
        )
    {
        return Err("snapshot_bad_record_sig");
    }
    let epoch = covered
        .iter()
        .rev()
        .find(|e| {
            !e["digest"]
                .as_str()
                .is_some_and(|d| fold.cancelled.contains(d))
                && (e["body"]["kind"] == "genesis" || e["body"]["kind"] == "rotation")
        })
        .and_then(|e| e["epoch"].as_u64())
        .ok_or("snapshot_epoch")?;
    let key = decode_b64(&snapshot.author.signing_public_key, Some(32))
        .ok_or("snapshot_bad_record_sig")?;
    let ctx = ObjectContext {
        account: &snapshot.account,
        epoch,
        author_id: &snapshot.author.id,
        author_signing_public_key: &key,
    };
    let mut headers: Vec<Option<SyncObjectHeader>> = vec![None; chunks.len()];
    let mut digests = vec![Vec::new(); chunks.len()];
    for chunk in chunks {
        let header = verify_object(chunk, &ctx).map_err(|r| match r {
            "account_mismatch" => "snapshot_account",
            "epoch_mismatch" => "snapshot_epoch",
            "author_mismatch" => "snapshot_author_mismatch",
            _ => "snapshot_bad_record_sig",
        })?;
        if header.kind != ObjectKind::Snapshot {
            return Err("snapshot_kind");
        }
        let placement = header.chunk.as_ref().ok_or("snapshot_kind")?;
        let index = usize::try_from(placement.index).map_err(|_| "snapshot_chunk_coverage")?;
        if usize::try_from(placement.count).ok() != Some(chunks.len())
            || index >= headers.len()
            || headers[index].is_some()
        {
            return Err("snapshot_chunk_coverage");
        }
        digests[index] = sha256(chunk);
        headers[index] = Some(header);
    }
    let headers = headers
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or("snapshot_chunk_coverage")?;
    if sha256_b64(&digests.concat()) != snapshot.digest {
        return Err("snapshot_digest");
    }
    Ok(headers)
}
