//! The receive half, outside the writer lock (sync-design §5-§6): pull after the applied cursor in
//! pages, download each object, check its SHA-256 against the entry, verify it, and journal it as
//! `verified`. An entry this device already holds is a no-op when its (seq, batch id, digest)
//! match and tampering when anything differs. The first refusal stops the pull: nothing after it
//! is verified, so entries are applied strictly in order.

use std::collections::BTreeMap;

use super::engine_deps::EngineDeps;
use super::error::EngineResult;
use super::journal::{IncomingRecord, IncomingStage, SyncJournal, WireItem, to_base64};
use super::ports::{EntryKind, PulledEntry, RefusalCode, SnapshotRef, Verdict};
use crate::support::sha256::sha256_hex;

const PAGE: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    pub refused: u64,
    pub snapshot: Option<u64>,
}

fn wire_items(verdict_items: &[super::ports::PayloadItem]) -> Vec<WireItem> {
    verdict_items
        .iter()
        .map(|i| WireItem {
            key: i.key.clone(),
            ops: to_base64(&i.ops),
        })
        .collect()
}

async fn take_snapshot(
    deps: &EngineDeps,
    journal: &SyncJournal,
    entry: &PulledEntry,
    snapshot: &SnapshotRef,
) -> EngineResult<Option<RefusalCode>> {
    let mut chunks = Vec::new();
    for id in &snapshot.object_ids {
        chunks.push(deps.transport.download(id).await?);
    }
    if let (Some(digest), [only]) = (&snapshot.digest, chunks.as_slice())
        && sha256_hex(only) != *digest
    {
        return Ok(Some(RefusalCode::ContentHash));
    }
    journal.put_incoming(&IncomingRecord {
        entry: entry.clone(),
        stage: IncomingStage::Downloaded,
        items: vec![],
        snapshot: true,
    })?;
    deps.fault_at("downloaded")?;
    match deps.verifier.verify_snapshot(snapshot, &chunks).await? {
        Verdict::Refused(code) => Ok(Some(code)),
        Verdict::Ok { payload, .. } => {
            journal.put_incoming(&IncomingRecord {
                entry: entry.clone(),
                stage: IncomingStage::Verified,
                items: wire_items(&payload.items),
                snapshot: true,
            })?;
            deps.fault_at("verified")?;
            Ok(None)
        }
    }
}

fn refusal(
    deps: &EngineDeps,
    journal: &SyncJournal,
    entry: &PulledEntry,
    code: RefusalCode,
    snapshot: Option<u64>,
) -> EngineResult<Received> {
    deps.log_line(&format!(
        "sync.batch.rejected seq={} batch={}: {}",
        entry.seq,
        entry.batch_id.as_deref().unwrap_or("null"),
        code
    ));
    journal.update_state(|s| {
        s.refused += 1;
        s.paused = Some(super::journal::Paused {
            seq: entry.seq,
            code,
        });
    })?;
    Ok(Received {
        refused: 1,
        snapshot,
    })
}

/// Everything the receive loop knows about entries this device already holds.
struct Held {
    /// Applied batch ids -> (seq, digest).
    index: BTreeMap<String, (u64, String)>,
    by_seq: BTreeMap<u64, IncomingRecord>,
    /// Batch ids of held incoming records -> (seq, digest).
    batches: BTreeMap<String, (u64, String)>,
}

impl Held {
    fn elsewhere(&self, batch_id: &str) -> Option<&(u64, String)> {
        self.index
            .get(batch_id)
            .or_else(|| self.batches.get(batch_id))
    }
}

async fn take(
    deps: &EngineDeps,
    journal: &SyncJournal,
    held: &Held,
    entry: &PulledEntry,
) -> EngineResult<Option<RefusalCode>> {
    let bytes = if entry.kind == EntryKind::Keyring {
        Vec::new()
    } else {
        deps.transport.download(&entry.object_id).await?
    };
    if entry.kind == EntryKind::Batch && sha256_hex(&bytes) != entry.digest {
        return Ok(Some(RefusalCode::ContentHash));
    }
    journal.put_incoming(&IncomingRecord {
        entry: entry.clone(),
        stage: IncomingStage::Downloaded,
        items: vec![],
        snapshot: false,
    })?;
    deps.fault_at("downloaded")?;
    let (payload, batch_id) = match deps.verifier.verify_entry(entry, &bytes).await? {
        Verdict::Refused(code) => return Ok(Some(code)),
        Verdict::Ok { payload, batch_id } => (payload, batch_id),
    };
    // The pull row names the batch; an object whose sealed header names another is not this entry.
    if let Some(named) = &entry.batch_id
        && *named != batch_id
    {
        return Ok(Some(RefusalCode::ObjectBinding));
    }
    if let Some((seq, _)) = held.elsewhere(&batch_id)
        && *seq != entry.seq
    {
        return Ok(Some(RefusalCode::ReplayedContent));
    }
    let named = PulledEntry {
        batch_id: Some(batch_id),
        ..entry.clone()
    };
    journal.put_incoming(&IncomingRecord {
        entry: named,
        stage: IncomingStage::Verified,
        items: wire_items(&payload.items),
        snapshot: false,
    })?;
    deps.fault_at("verified")?;
    Ok(None)
}

pub async fn receive(deps: &EngineDeps, journal: &SyncJournal) -> EngineResult<Received> {
    let state = journal.state()?;
    let by_seq: BTreeMap<u64, IncomingRecord> = journal
        .incoming()?
        .into_iter()
        .map(|r| (r.entry.seq, r))
        .collect();
    let held = Held {
        index: journal
            .applied()?
            .into_iter()
            .map(|(k, v)| (k, (v.seq, v.digest)))
            .collect(),
        batches: by_seq
            .values()
            .filter_map(|r| {
                r.entry
                    .batch_id
                    .clone()
                    .map(|b| (b, (r.entry.seq, r.entry.digest.clone())))
            })
            .collect(),
        by_seq,
    };
    let mut expected = state.applied + 1;
    let mut after = state.applied;
    let mut snapshot: Option<u64> = None;
    loop {
        let page = deps.transport.pull(after, PAGE).await?;
        if let Some(s) = page.snapshot.as_ref().filter(|s| s.covers_seq >= expected) {
            let entry = PulledEntry {
                seq: s.covers_seq,
                kind: EntryKind::Batch,
                batch_id: Some(format!("snapshot:{}", s.covers_seq)),
                digest: s.digest.clone().unwrap_or_default(),
                object_id: s.object_ids.join(","),
                author: "snapshot".to_string(),
            };
            let verified =
                held.by_seq.get(&s.covers_seq).map(|r| r.stage) == Some(IncomingStage::Verified);
            if !verified && let Some(code) = take_snapshot(deps, journal, &entry, s).await? {
                return refusal(deps, journal, &entry, code, snapshot);
            }
            snapshot = Some(s.covers_seq);
            expected = s.covers_seq + 1;
        }
        for entry in &page.entries {
            let known = entry.batch_id.as_deref().and_then(|b| held.elsewhere(b));
            if entry.seq < expected {
                let original = known
                    .map(|(seq, digest)| (*seq, digest.as_str()))
                    .or_else(|| {
                        held.by_seq
                            .get(&entry.seq)
                            .map(|r| (r.entry.seq, r.entry.digest.as_str()))
                    });
                if let Some((seq, digest)) = original
                    && (seq != entry.seq || digest != entry.digest)
                {
                    return refusal(deps, journal, entry, RefusalCode::ReplayedContent, snapshot);
                }
                continue;
            }
            if entry.seq != expected {
                return refusal(deps, journal, entry, RefusalCode::SequenceGap, snapshot);
            }
            if let Some((seq, _)) = known
                && *seq != entry.seq
            {
                return refusal(deps, journal, entry, RefusalCode::ReplayedContent, snapshot);
            }
            let reusable = held.by_seq.get(&entry.seq).is_some_and(|m| {
                m.stage == IncomingStage::Verified && m.entry.digest == entry.digest
            });
            if !reusable && let Some(code) = take(deps, journal, &held, entry).await? {
                return refusal(deps, journal, entry, code, snapshot);
            }
            expected = entry.seq + 1;
        }
        let Some(last) = page.entries.last() else {
            return finish(journal, &state.paused, snapshot);
        };
        if !page.more {
            return finish(journal, &state.paused, snapshot);
        }
        after = last.seq;
    }
}

fn finish(
    journal: &SyncJournal,
    paused: &Option<super::journal::Paused>,
    snapshot: Option<u64>,
) -> EngineResult<Received> {
    if paused.is_some() {
        journal.update_state(|s| s.paused = None)?;
    }
    Ok(Received {
        refused: 0,
        snapshot,
    })
}
