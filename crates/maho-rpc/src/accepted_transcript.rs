//! senpi `modes/rpc/accepted-transcript.ts`: the transcript component of one epoch, changed only by
//! accepted transitions (R3.C1.1).
use maho_core::session_transcript_journal::{
    PersistedBase, PersistedBaseError, TranscriptPersistence, TranscriptTransition,
    TranscriptUnavailableReason, read_persisted_base,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// One worker frame's worth of unwritten entries; persisted history consumes no content budget.
pub const DEFAULT_TRANSCRIPT_TAIL_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct TranscriptTail {
    entries: BTreeMap<usize, String>,
    retained_bytes: usize,
    persistence: Option<TranscriptPersistence>,
}

/// A cut names its base and accepted ids. Its shared tail releases content only once the captured file
/// covers it; persistence may change the source of those exact entries, never the accepted count.
#[derive(Debug, Clone)]
pub struct TranscriptRoot {
    pub history_version: u64,
    pub leaf_id: Option<String>,
    pub base: Option<PersistedBase>,
    pub entry_ids: Arc<Vec<String>>,
    pub tail: Arc<Mutex<TranscriptTail>>,
    pub count: usize,
    pub unavailable: Option<TranscriptUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptReadError {
    Base(PersistedBaseError),
    Unavailable(TranscriptUnavailableReason),
}

fn lock(tail: &Mutex<TranscriptTail>) -> std::sync::MutexGuard<'_, TranscriptTail> {
    tail.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Default)]
pub struct AcceptedTranscript {
    version: u64,
    leaf_id: Option<String>,
    base: Option<PersistedBase>,
    entry_ids: Arc<Vec<String>>,
    tail: Arc<Mutex<TranscriptTail>>,
    unavailable: Option<TranscriptUnavailableReason>,
}

impl AcceptedTranscript {
    pub fn history_version(&self) -> u64 {
        self.version
    }

    pub fn available(&self) -> bool {
        self.unavailable.is_none()
    }

    pub fn apply(&mut self, transition: &TranscriptTransition) {
        self.version += 1;
        match transition {
            TranscriptTransition::Replace { base } => {
                self.base = base.clone();
                self.entry_ids = Arc::new(Vec::new());
                self.tail = Arc::new(Mutex::new(TranscriptTail {
                    persistence: base.as_ref().map(|base| TranscriptPersistence {
                        file_identity: base.file_identity.clone(),
                        count: base.count,
                    }),
                    ..TranscriptTail::default()
                }));
                self.unavailable = None;
            }
            TranscriptTransition::Unavailable { reason } => self.mark_unavailable(*reason),
            TranscriptTransition::Persisted { persistence } => self.persisted(persistence),
            _ if self.unavailable.is_some() => {}
            TranscriptTransition::Append {
                id,
                entry,
                persistence,
                ..
            } => {
                let index = self.entry_ids.len();
                Arc::make_mut(&mut self.entry_ids).push(id.clone());
                if let Some(persistence) = persistence {
                    self.persisted(persistence);
                }
                let base_count = self.base.as_ref().map_or(0, |base| base.count);
                let mut tail = lock(&self.tail);
                let persisted = tail.persistence.as_ref().map_or(0, |p| p.count);
                if index + base_count >= persisted {
                    let bytes = entry.as_ref().map_or(0, String::len);
                    if tail.retained_bytes + bytes > DEFAULT_TRANSCRIPT_TAIL_BYTES {
                        drop(tail);
                        self.mark_unavailable(
                            TranscriptUnavailableReason::TranscriptTailOverByteLimit,
                        );
                        return;
                    }
                    if let Some(entry) = entry {
                        tail.entries.insert(index, entry.clone());
                        tail.retained_bytes += bytes;
                    }
                }
                drop(tail);
                self.leaf_id = Some(id.clone());
            }
            TranscriptTransition::Leaf { leaf_id } => self.leaf_id = leaf_id.clone(),
        }
    }

    fn mark_unavailable(&mut self, reason: TranscriptUnavailableReason) {
        self.unavailable.get_or_insert(reason);
        eprintln!("senpi rpc acceptance: transcript component unavailable: {reason:?}");
    }

    pub fn freeze(&self) -> TranscriptRoot {
        TranscriptRoot {
            history_version: self.version,
            leaf_id: self.leaf_id.clone(),
            base: self.base.clone(),
            entry_ids: self.entry_ids.clone(),
            tail: self.tail.clone(),
            count: self.entry_ids.len(),
            unavailable: self.unavailable,
        }
    }

    fn persisted(&mut self, persistence: &TranscriptPersistence) {
        let base_count = self.base.as_ref().map_or(0, |base| base.count);
        let mut tail = lock(&self.tail);
        tail.persistence = Some(persistence.clone());
        let covered: Vec<usize> = tail
            .entries
            .keys()
            .copied()
            .take_while(|index| index + base_count < persistence.count)
            .collect();
        for index in covered {
            if let Some(entry) = tail.entries.remove(&index) {
                tail.retained_bytes -= entry.len();
            }
        }
    }
}

/// The root's entries: its base read only through the captured identity, then its own accepted values.
pub fn read_transcript_root(root: &TranscriptRoot) -> Result<Vec<Value>, TranscriptReadError> {
    if let Some(reason) = root.unavailable {
        return Err(TranscriptReadError::Unavailable(reason));
    }
    let tail = lock(&root.tail);
    let persistence = tail.persistence.clone();
    let base_count = root.base.as_ref().map_or(0, |base| base.count);
    let persisted_count = root.count.min(
        persistence
            .as_ref()
            .map_or(0, |p| p.count)
            .saturating_sub(base_count),
    );
    let base = root.base.clone().or_else(|| {
        persistence.as_ref().map(|persistence| PersistedBase {
            file_identity: persistence.file_identity.clone(),
            count: 0,
            id_fingerprint: hex::encode(Sha256::digest(b"")),
        })
    });
    let mut entries = match base {
        Some(base) => read_persisted_base(&base, &root.entry_ids[..persisted_count])
            .map_err(TranscriptReadError::Base)?,
        None => Vec::new(),
    };
    for index in persisted_count..root.count {
        let Some(entry) = tail.entries.get(&index) else {
            return Err(TranscriptReadError::Unavailable(
                TranscriptUnavailableReason::TranscriptTailTrimmed,
            ));
        };
        entries.push(serde_json::from_str(entry).unwrap_or(Value::Null));
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

    // senpi 4272-transcript-root-versions: each accepted transition is one root version; cuts are stable.
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
}
