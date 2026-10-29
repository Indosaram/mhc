//! The push half, outside the writer lock (sync-design §6): seal the journaled pending ops as one
//! batch on the applied cursor, reserve, upload, append. Each step is journaled before the next,
//! and the batch id is the append's idempotency key, so a crash after the server accepted the batch
//! re-sends the same id and gets the original seq back instead of a second entry.

use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::batches::{MAX_BATCH_BYTES, split_batches};
use super::engine_deps::EngineDeps;
use super::error::{EngineError, EngineResult};
use super::journal::{
    OutgoingBatch, OutgoingStage, PendingOps, SyncJournal, WireItem, from_base64, to_base64,
};
use super::ports::{
    AppendRequest, AppendResult, BatchPayload, EntryKind, PayloadItem, PulledEntry,
};
use crate::support::sha256::sha256_hex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    Idle,
    Pushed,
    Stale,
}

const BACKOFF_BASE_MS: u64 = 250;
const BACKOFF_CAP_MS: u64 = 8_000;

fn fill(buf: &mut [u8]) -> EngineResult<()> {
    SystemRandom::new()
        .fill(buf)
        .map_err(|_| EngineError::Other("system random unavailable".to_string()))
}

/// A uniform `[0, 1)` draw from the system RNG (the default for `staleBackoffMs`).
pub fn system_random() -> f64 {
    let mut bytes = [0u8; 8];
    if fill(&mut bytes).is_err() {
        return 0.5;
    }
    (u64::from_le_bytes(bytes) >> 11) as f64 / (1u64 << 53) as f64
}

/// Full-window jitter in the upper half: `[window/2, window)`, the window doubling to the cap.
pub fn stale_backoff_ms(attempt: u32, random: &dyn Fn() -> f64) -> u64 {
    let doubled = BACKOFF_BASE_MS.saturating_mul(1u64 << attempt.min(20));
    let window = doubled.min(BACKOFF_CAP_MS) as f64;
    (window / 2.0 + random() * (window / 2.0)).floor() as u64
}

fn carried(out: &OutgoingBatch, item: &WireItem) -> bool {
    out.items
        .iter()
        .any(|o| o.key == item.key && o.ops == item.ops)
}

async fn seal(
    deps: &EngineDeps,
    journal: &SyncJournal,
    items: Vec<WireItem>,
    batch_id: String,
) -> EngineResult<OutgoingBatch> {
    let all = journal.pending()?.items;
    let base_seq = journal.state()?.applied;
    let payload = BatchPayload {
        items: items
            .iter()
            .map(|i| {
                Ok(PayloadItem {
                    key: i.key.clone(),
                    ops: from_base64(&i.ops)?,
                })
            })
            .collect::<EngineResult<_>>()?,
    };
    let sealed = deps.sealer.seal(&batch_id, base_seq, &payload).await?;
    let out = OutgoingBatch {
        batch_id,
        base_seq,
        digest: sha256_hex(&sealed.bytes),
        bytes: to_base64(&sealed.bytes),
        upload_id: None,
        epoch: sealed.epoch,
        link: sealed.link,
        record_sig: sealed.record_sig,
        stage: OutgoingStage::Sealed,
        items,
    };
    journal.set_pending(&PendingOps {
        items: all,
        outgoing: Some(out.clone()),
    })?;
    deps.fault_at("sealed")?;
    Ok(out)
}

pub async fn push(deps: &EngineDeps, journal: &SyncJournal) -> EngineResult<PushOutcome> {
    let batch_bytes = deps.batch_bytes.unwrap_or(MAX_BATCH_BYTES);
    let mut outcome = PushOutcome::Idle;
    loop {
        match push_one(deps, journal, batch_bytes).await? {
            PushOutcome::Idle => return Ok(outcome),
            PushOutcome::Stale => return Ok(PushOutcome::Stale),
            PushOutcome::Pushed => outcome = PushOutcome::Pushed,
        }
    }
}

async fn push_one(
    deps: &EngineDeps,
    journal: &SyncJournal,
    batch_bytes: usize,
) -> EngineResult<PushOutcome> {
    let pending = journal.pending()?;
    let mut out = match pending.outgoing.clone() {
        Some(out) => out,
        None => {
            let Some(first) = split_batches(&pending.items, batch_bytes)?
                .into_iter()
                .next()
            else {
                return Ok(PushOutcome::Idle);
            };
            let mut id = [0u8; 16];
            fill(&mut id)?;
            seal(deps, journal, first, URL_SAFE_NO_PAD.encode(id)).await?
        }
    };
    let bytes = from_base64(&out.bytes)?;
    if out.stage == OutgoingStage::Sealed {
        let upload_id = deps.transport.reserve(bytes.len(), &out.digest).await?;
        out.upload_id = Some(upload_id);
        out.stage = OutgoingStage::Reserved;
        journal.set_pending(&PendingOps {
            outgoing: Some(out.clone()),
            ..pending.clone()
        })?;
        deps.fault_at("reserved")?;
    }
    if out.stage == OutgoingStage::Reserved {
        deps.transport
            .upload(out.upload_id.as_deref().unwrap_or(""), &bytes)
            .await?;
        out.stage = OutgoingStage::Uploaded;
        journal.set_pending(&PendingOps {
            outgoing: Some(out.clone()),
            ..pending.clone()
        })?;
        deps.fault_at("uploaded")?;
    }
    let result = deps
        .transport
        .append(AppendRequest {
            upload_id: out.upload_id.clone().unwrap_or_default(),
            digest: out.digest.clone(),
            base_seq: out.base_seq,
            batch_id: out.batch_id.clone(),
            epoch: out.epoch,
            link: out.link.clone(),
            record_sig: out.record_sig.clone(),
        })
        .await?;
    let seq = match result {
        AppendResult::StaleBase { .. } => {
            journal.set_pending(&PendingOps {
                items: journal.pending()?.items,
                outgoing: None,
            })?;
            return Ok(PushOutcome::Stale);
        }
        AppendResult::Ok { seq } => seq,
    };
    deps.fault_at("append-landed")?;
    journal.record_applied(&[PulledEntry {
        seq,
        kind: EntryKind::Batch,
        batch_id: Some(out.batch_id.clone()),
        digest: out.digest.clone(),
        object_id: String::new(),
        author: "self".to_string(),
    }])?;
    if seq == journal.state()?.applied + 1 {
        journal.update_state(|s| s.applied = seq)?;
    }
    let remaining: Vec<WireItem> = journal
        .pending()?
        .items
        .into_iter()
        .filter(|i| !carried(&out, i))
        .collect();
    journal.set_pending(&PendingOps {
        items: remaining,
        outgoing: None,
    })?;
    deps.fault_at("appended")?;
    Ok(PushOutcome::Pushed)
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;
