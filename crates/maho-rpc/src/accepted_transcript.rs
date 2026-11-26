//! senpi `modes/rpc/accepted-transcript.ts`: the transcript component of one epoch, changed only by
//! accepted transitions (R3.C1.1), with bounded ancestry tracking (`DEFAULT_TRANSCRIPT_ANCESTRY_BYTES`),
//! bounded/spillable tail (`accepted_transcript_tail`), and recoverable tail-bound refusals (RULING-80).

use crate::accepted_transcript_tail::{
    StagedTailEntry, TailBudget, TailHold, TailReadFailure, TranscriptTail, stage_bootstrap,
};
use maho_core::session_transcript_journal::{
    PersistedBase, PersistedBaseError, TranscriptPersistence, TranscriptTransition,
    TranscriptUnavailableReason, read_persisted_base,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

/// One worker frame's worth of unwritten entries (legacy constant kept for callers).
pub const DEFAULT_TRANSCRIPT_TAIL_BYTES: usize = 16 * 1024 * 1024;
/// What an epoch keeps of every accepted entry after persistence released its text (RULING-61 Q1 = C).
pub const DEFAULT_TRANSCRIPT_ANCESTRY_BYTES: usize = 8 * 1024 * 1024;
const ANCESTRY_RECORD_BYTES: usize = 40;
const KIND_NAME_BYTES: usize = 64;

fn string_bytes(value: &str) -> usize {
    if value.is_empty() {
        0
    } else {
        let len = if value.is_ascii() {
            value.len()
        } else {
            2 * value.encode_utf16().count()
        };
        (16 + len).div_ceil(8) * 8
    }
}

/// One accepted entry of the epoch, at its accepted index, as a frozen root names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedEntry {
    pub index: usize,
    pub id: String,
    pub kind: String,
    pub parent_id: Option<String>,
}

/// The first `count` entries of an epoch's columns (`freeze` captures in O(1) without copying).
#[derive(Debug, Clone, Default)]
pub struct AcceptedEntries {
    ids: Arc<Vec<String>>,
    kinds: Arc<Vec<String>>,
    parents: Arc<Vec<Option<String>>>,
    pub count: usize,
}

impl AcceptedEntries {
    pub fn ids_before(&self, end: usize) -> &[String] {
        let limit = end.min(self.count).min(self.ids.len());
        &self.ids[..limit]
    }

    pub fn iter(&self) -> impl Iterator<Item = AcceptedEntry> + '_ {
        let limit = self
            .count
            .min(self.ids.len())
            .min(self.kinds.len())
            .min(self.parents.len());
        (0..limit).map(move |index| AcceptedEntry {
            index,
            id: self.ids[index].clone(),
            kind: self.kinds[index].clone(),
            parent_id: self.parents[index].clone(),
        })
    }
}

/// A cut names its base and accepted ids/ancestry. Its shared tail releases content only once the
/// captured file covers it; persistence may change the source of those exact entries, never the count.
#[derive(Debug, Clone)]
pub struct TranscriptRoot {
    pub history_version: u64,
    pub leaf_id: Option<String>,
    pub base: Option<PersistedBase>,
    pub entry_ids: Arc<Vec<String>>,
    pub accepted: AcceptedEntries,
    pub ancestry_bytes: usize,
    pub tail: Arc<Mutex<TranscriptTail>>,
    pub count: usize,
    pub unavailable: Option<TranscriptUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptReadError {
    Base(PersistedBaseError),
    Unavailable(TranscriptUnavailableReason),
    Tail(TailReadFailure),
}

impl From<TailReadFailure> for TranscriptReadError {
    fn from(value: TailReadFailure) -> Self {
        Self::Tail(value)
    }
}

fn lock_tail(tail: &Mutex<TranscriptTail>) -> std::sync::MutexGuard<'_, TranscriptTail> {
    tail.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug)]
pub struct AcceptedTranscript {
    version: u64,
    leaf_id: Option<String>,
    base: Option<PersistedBase>,
    entry_ids: Arc<Vec<String>>,
    kinds: Arc<Vec<String>>,
    parents: Arc<Vec<Option<String>>>,
    kind_names: HashMap<String, String>,
    ancestry_bytes: usize,
    budget: Arc<Mutex<TailBudget>>,
    tail: Arc<Mutex<TranscriptTail>>,
    unavailable: Option<TranscriptUnavailableReason>,
    missing_from: Option<usize>,
}

impl Default for AcceptedTranscript {
    fn default() -> Self {
        let budget = Arc::new(Mutex::new(TailBudget::with_default()));
        let tail = Arc::new(Mutex::new(TranscriptTail::new(None, Arc::clone(&budget))));
        Self {
            version: 0,
            leaf_id: None,
            base: None,
            entry_ids: Arc::new(Vec::new()),
            kinds: Arc::new(Vec::new()),
            parents: Arc::new(Vec::new()),
            kind_names: HashMap::new(),
            ancestry_bytes: 0,
            budget,
            tail,
            unavailable: None,
            missing_from: None,
        }
    }
}

fn extract_kind_and_parent(entry: Option<&str>) -> (String, Option<String>) {
    let Some(raw) = entry else {
        return ("entry".to_owned(), None);
    };
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return ("entry".to_owned(), None);
    };
    let kind = parsed
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("entry")
        .to_owned();
    let parent_id = parsed
        .get("parentId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    (kind, parent_id)
}

impl AcceptedTranscript {
    pub fn history_version(&self) -> u64 {
        self.version
    }

    pub fn available(&self) -> bool {
        self.unavailable.is_none()
    }

    /// Applies a bootstrap after pre-staging all entry texts against the tail budget; a refusal
    /// marks the transcript unavailable without keeping partial entries.
    pub fn bootstrap(&mut self, transitions: &[TranscriptTransition]) {
        let Some(staged) = stage_bootstrap(transitions) else {
            self.mark_unavailable(
                TranscriptUnavailableReason::TranscriptTailOverByteLimit,
                None,
            );
            return;
        };
        let mut by_pos: BTreeMap<usize, StagedTailEntry> = staged.into_iter().collect();
        for (pos, transition) in transitions.iter().enumerate() {
            self.apply_with_staged(transition, by_pos.remove(&pos));
        }
    }

    pub fn apply(&mut self, transition: &TranscriptTransition) {
        self.apply_with_staged(transition, None);
    }

    pub fn apply_with_staged(
        &mut self,
        transition: &TranscriptTransition,
        staged: Option<StagedTailEntry>,
    ) {
        self.version += 1;
        match transition {
            TranscriptTransition::Replace { base } => {
                lock_tail(&self.tail).supersede();
                self.base = base.clone();
                self.entry_ids = Arc::new(Vec::new());
                self.kinds = Arc::new(Vec::new());
                self.parents = Arc::new(Vec::new());
                self.kind_names.clear();
                self.ancestry_bytes = 0;
                let persistence = base.as_ref().map(|base| TranscriptPersistence {
                    file_identity: base.file_identity.clone(),
                    count: base.count,
                });
                self.tail = Arc::new(Mutex::new(TranscriptTail::new(
                    persistence,
                    Arc::clone(&self.budget),
                )));
                self.unavailable = None;
                self.missing_from = None;
            }
            TranscriptTransition::Unavailable { reason } => self.mark_unavailable(*reason, None),
            TranscriptTransition::Persisted { persistence } => self.persisted(persistence),
            _ if self.unavailable.is_some() && self.missing_from.is_none() => {}
            TranscriptTransition::Append {
                id,
                entry,
                persistence,
                ..
            } => {
                let index = self.entry_ids.len();
                let (kind, parent_id) = extract_kind_and_parent(entry.as_deref());
                if !self.retain_ancestry(id, &kind, parent_id) {
                    return;
                }
                if let Some(persistence) = persistence {
                    self.persisted(persistence);
                }
                let base_count = self.base.as_ref().map_or(0, |base| base.count);
                let mut tail = lock_tail(&self.tail);
                let persisted = tail.persistence.as_ref().map_or(0, |p| p.count);
                if self.missing_from.is_none()
                    && index + base_count >= persisted
                    && let Some(text) = entry.as_deref()
                {
                    if let Some(staged_entry) = staged.or_else(|| tail.stage(index, text)) {
                        tail.adopt(index, staged_entry);
                    } else {
                        drop(tail);
                        self.mark_unavailable(
                            TranscriptUnavailableReason::TranscriptTailOverByteLimit,
                            Some(index),
                        );
                    }
                }
                self.leaf_id = Some(id.clone());
            }
            TranscriptTransition::Leaf { leaf_id } => self.leaf_id = leaf_id.clone(),
        }
    }

    fn retain_ancestry(&mut self, id: &str, kind: &str, parent_id: Option<String>) -> bool {
        let previous = self.entry_ids.last().cloned();
        let shared = previous
            .as_deref()
            .zip(parent_id.as_deref())
            .is_some_and(|(prev, par)| prev == par);
        let interned = self.kind_names.get(kind).cloned();
        let bytes = ANCESTRY_RECORD_BYTES
            + string_bytes(id)
            + if interned.is_none() {
                KIND_NAME_BYTES + string_bytes(kind)
            } else {
                0
            }
            + if shared {
                0
            } else {
                string_bytes(parent_id.as_deref().unwrap_or(""))
            };
        if self.ancestry_bytes + bytes > DEFAULT_TRANSCRIPT_ANCESTRY_BYTES {
            self.mark_unavailable(
                TranscriptUnavailableReason::TranscriptAncestryOverByteLimit,
                None,
            );
            return false;
        }
        let kind_str = match interned {
            Some(existing) => existing,
            None => {
                self.kind_names.insert(kind.to_owned(), kind.to_owned());
                kind.to_owned()
            }
        };
        Arc::make_mut(&mut self.entry_ids).push(id.to_owned());
        Arc::make_mut(&mut self.kinds).push(kind_str);
        Arc::make_mut(&mut self.parents).push(if shared { previous } else { parent_id });
        self.ancestry_bytes += bytes;
        true
    }

    fn mark_unavailable(
        &mut self,
        reason: TranscriptUnavailableReason,
        missing_from: Option<usize>,
    ) {
        self.missing_from = if self.unavailable.is_none() {
            missing_from
        } else {
            None
        };
        self.unavailable.get_or_insert(reason);
        eprintln!("senpi rpc acceptance: transcript component unavailable: {reason:?}");
    }

    pub fn freeze(&self) -> TranscriptRoot {
        let count = self.entry_ids.len();
        TranscriptRoot {
            history_version: self.version,
            leaf_id: self.leaf_id.clone(),
            base: self.base.clone(),
            entry_ids: Arc::clone(&self.entry_ids),
            accepted: AcceptedEntries {
                ids: Arc::clone(&self.entry_ids),
                kinds: Arc::clone(&self.kinds),
                parents: Arc::clone(&self.parents),
                count,
            },
            ancestry_bytes: self.ancestry_bytes,
            tail: Arc::clone(&self.tail),
            count,
            unavailable: self.unavailable,
        }
    }

    fn persisted(&mut self, persistence: &TranscriptPersistence) {
        let base_count = self.base.as_ref().map_or(0, |base| base.count);
        lock_tail(&self.tail).persisted(persistence.clone(), base_count);
        // Every entry the version held no text for is now readable from the file by identity (RULING-80 (4)).
        if self.missing_from.is_some()
            && persistence.count.saturating_sub(base_count) >= self.entry_ids.len()
        {
            self.missing_from = None;
            self.unavailable = None;
        }
    }

    pub fn release(&mut self) {
        lock_tail(&self.tail).release_all();
    }
}

/// The root's entries: its base read only through the captured identity, then its own accepted values
/// held consistently across the read so a `Persisted` report mid-read cannot trim them.
pub fn read_transcript_root(root: &TranscriptRoot) -> Result<Vec<Value>, TranscriptReadError> {
    if let Some(reason) = root.unavailable {
        return Err(TranscriptReadError::Unavailable(reason));
    }
    let base_count = root.base.as_ref().map_or(0, |base| base.count);
    let mut hold: TailHold = lock_tail(&root.tail).hold(root.count, base_count);
    let result = read_with_hold(root, &hold);
    hold.release();
    result
}

fn read_with_hold(
    root: &TranscriptRoot,
    hold: &TailHold,
) -> Result<Vec<Value>, TranscriptReadError> {
    let base = root.base.clone().or_else(|| {
        hold.persistence
            .as_ref()
            .filter(|_| hold.file_count > 0)
            .map(|persistence| PersistedBase {
                file_identity: persistence.file_identity.clone(),
                count: 0,
                id_fingerprint: hex::encode(Sha256::digest(b"")),
            })
    });
    let mut entries = match base {
        Some(base) => read_persisted_base(&base, root.accepted.ids_before(hold.file_count))
            .map_err(TranscriptReadError::Base)?,
        None => Vec::new(),
    };
    for index in hold.file_count..root.count {
        let Some(text) = hold.get_text(index)? else {
            return Err(TranscriptReadError::Unavailable(
                TranscriptUnavailableReason::TranscriptTailTrimmed,
            ));
        };
        entries.push(serde_json::from_str(&text).unwrap_or(Value::Null));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn append(id: &str) -> TranscriptTransition {
        TranscriptTransition::Append {
            id: id.into(),
            entry: Some(format!("{{\"type\":\"message\",\"id\":\"{id}\"}}")),
            persistence: None,
            owning_message_id: None,
            recovery_epoch: None,
        }
    }

    #[test]
    fn versions_roots_and_keeps_earlier_cuts_stable() {
        let mut transcript = AcceptedTranscript::default();
        transcript.apply(&TranscriptTransition::Replace { base: None });
        transcript.apply(&append("a"));
        let early = transcript.freeze();
        transcript.apply(&append("b"));
        transcript.apply(&TranscriptTransition::Leaf {
            leaf_id: Some("a".into()),
        });
        let late = transcript.freeze();
        assert_eq!((early.history_version, early.count), (2, 1));
        assert_eq!((late.history_version, late.count), (4, 2));
        assert_eq!(late.leaf_id.as_deref(), Some("a"));
        let ids = |root: &TranscriptRoot| -> Vec<String> {
            read_transcript_root(root)
                .expect("read")
                .iter()
                .map(|entry| entry["id"].as_str().unwrap_or_default().to_owned())
                .collect()
        };
        assert_eq!(ids(&early), ["a"]);
        assert_eq!(ids(&late), ["a", "b"]);
        transcript.apply(&TranscriptTransition::Unavailable {
            reason: TranscriptUnavailableReason::TranscriptTailTrimmed,
        });
        transcript.apply(&append("c"));
        assert!(!transcript.available());
        assert_eq!(
            read_transcript_root(&transcript.freeze()),
            Err(TranscriptReadError::Unavailable(
                TranscriptUnavailableReason::TranscriptTailTrimmed
            ))
        );
    }

    #[test]
    fn ancestry_and_bootstrap_staging_are_bounded_and_frozen_in_o1() {
        let mut transcript = AcceptedTranscript::default();
        transcript.bootstrap(&[
            TranscriptTransition::Replace { base: None },
            append("m1"),
            TranscriptTransition::Append {
                id: "m2".into(),
                entry: Some("{\"type\":\"message\",\"id\":\"m2\",\"parentId\":\"m1\"}".into()),
                persistence: None,
                owning_message_id: None,
                recovery_epoch: None,
            },
        ]);
        let root = transcript.freeze();
        assert_eq!(root.count, 2);
        assert!(root.ancestry_bytes > 0);
        let collected: Vec<_> = root.accepted.iter().collect();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].kind, "message");
        assert_eq!(collected[1].parent_id.as_deref(), Some("m1"));
    }
}
