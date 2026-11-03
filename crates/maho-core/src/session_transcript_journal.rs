//! Port of senpi packages/coding-agent/src/core/session-transcript-journal.ts.
//!
//! The committed transcript changes of one session manager, recorded after the commit and never before
//! persistence, for a consumer that began the journal (senpi#4272, R3.C1.1).

use std::fs::File;
use std::io::{BufRead, BufReader};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub use crate::session_file_identity::FileIdentity;

/// The first `count` entries of a session file as one load or rewrite left them, named without holding them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedBase {
    pub file_identity: FileIdentity,
    pub count: usize,
    pub id_fingerprint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptUnavailableReason {
    TransitionOverWorkerFrameLimit,
    TranscriptFileIdentityUnavailable,
    TranscriptTailTrimmed,
    TranscriptTailOverByteLimit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPersistence {
    pub file_identity: FileIdentity,
    pub count: usize,
}

/// One committed change of a manager's transcript. Entry values are owned JSON text: the manager mutates
/// its entry objects later (mirror trimming re-parents them).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TranscriptTransition {
    Replace {
        base: Option<PersistedBase>,
    },
    Append {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        entry: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        persistence: Option<TranscriptPersistence>,
        #[serde(skip_serializing_if = "Option::is_none")]
        owning_message_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        recovery_epoch: Option<String>,
    },
    Persisted {
        persistence: TranscriptPersistence,
    },
    Leaf {
        leaf_id: Option<String>,
    },
    Unavailable {
        reason: TranscriptUnavailableReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistedBaseError {
    BaseFileMissing,
    BaseFileReplaced,
    BaseFileTruncated,
    BaseFingerprintMismatch,
}

/// The committed transcript changes a consumer has not taken yet; it exists only once a consumer began it.
#[derive(Clone, Debug, Default)]
pub struct TranscriptJournal {
    pending: Vec<TranscriptTransition>,
}

impl TranscriptJournal {
    pub fn record(&mut self, transitions: impl IntoIterator<Item = TranscriptTransition>) {
        self.pending.extend(transitions);
    }

    pub fn drain(&mut self) -> Vec<TranscriptTransition> {
        std::mem::take(&mut self.pending)
    }
}

fn entry_field<'a>(entry: &'a Value, key: &str) -> &'a str {
    entry
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("undefined")
}

fn fingerprint(entries: &[Value]) -> String {
    let mut hash = Sha256::new();
    for entry in entries {
        hash.update(format!(
            "{}\u{0}{}\n",
            entry_field(entry, "type"),
            entry_field(entry, "id")
        ));
    }
    hex::encode(hash.finalize())
}

/// The loaded/written entries described now; the owning descriptor supplies their identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedBase {
    pub count: usize,
    pub id_fingerprint: String,
    pub history: usize,
}

pub fn capture_transcript_base(entries: &[Value]) -> CapturedBase {
    CapturedBase {
        count: entries.len(),
        id_fingerprint: fingerprint(entries),
        history: entries
            .iter()
            .filter(|entry| entry.get("type").and_then(Value::as_str) != Some("session"))
            .count(),
    }
}

/// Reads `base` through its captured identity only. It stops at `count`, so a legal append after the base is
/// never read, and a replaced, truncated or rewritten file is a typed failure, never a full or current-file read.
pub fn read_persisted_base(
    base: &PersistedBase,
    accepted_ids: &[String],
) -> Result<Vec<Value>, PersistedBaseError> {
    let Ok(file) = File::open(&base.file_identity.path) else {
        return Err(PersistedBaseError::BaseFileMissing);
    };
    let metadata = file
        .metadata()
        .map_err(|_| PersistedBaseError::BaseFileMissing)?;
    if !crate::session_file_identity::same_file(&metadata, &base.file_identity) {
        return Err(PersistedBaseError::BaseFileReplaced);
    }
    let wanted = base.count + accepted_ids.len();
    let mut entries = Vec::with_capacity(wanted);
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    while entries.len() < wanted {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buffer);
                if let Some(entry) = crate::session_manager::parse_session_entry_line(
                    line.trim_end_matches(['\n', '\r']),
                ) {
                    entries.push(entry);
                }
            }
        }
    }
    if entries.len() < wanted {
        return Err(PersistedBaseError::BaseFileTruncated);
    }
    if fingerprint(&entries[..base.count]) != base.id_fingerprint
        || accepted_ids.iter().enumerate().any(|(index, id)| {
            entries[base.count + index]
                .get("id")
                .and_then(Value::as_str)
                != Some(id.as_str())
        })
    {
        return Err(PersistedBaseError::BaseFingerprintMismatch);
    }
    Ok(entries)
}

/// A manager's whole transcript as transitions: its persisted base, each entry not on that base as its own
/// append, then its leaf. `unpersisted` is `None` when a trimmed mirror no longer holds those entries.
pub fn snapshot_transcript(
    base: Option<PersistedBase>,
    unpersisted: Option<&[Value]>,
    leaf_id: Option<&str>,
    own: &mut dyn FnMut(&Value) -> String,
    persistence: Option<&TranscriptPersistence>,
) -> Vec<TranscriptTransition> {
    let Some(unpersisted) = unpersisted else {
        return vec![TranscriptTransition::Unavailable {
            reason: TranscriptUnavailableReason::TranscriptTailTrimmed,
        }];
    };
    let mut transitions = vec![TranscriptTransition::Replace { base }];
    for entry in unpersisted {
        transitions.push(TranscriptTransition::Append {
            id: entry
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            entry: persistence.is_none().then(|| own(entry)),
            persistence: persistence.cloned(),
            owning_message_id: None,
            recovery_epoch: None,
        });
    }
    transitions.push(TranscriptTransition::Leaf {
        leaf_id: leaf_id.map(str::to_owned),
    });
    transitions
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    fn entries() -> Vec<Value> {
        vec![
            json!({"type":"session","id":"s"}),
            json!({"type":"message","id":"a","parentId":null}),
        ]
    }

    #[test]
    fn the_journal_drains_what_was_recorded_once() {
        let mut journal = TranscriptJournal::default();
        journal.record([TranscriptTransition::Leaf { leaf_id: None }]);
        assert_eq!(journal.drain().len(), 1);
        assert!(journal.drain().is_empty());
    }

    #[test]
    fn transitions_serialize_with_the_wire_kind_and_camel_case_fields() {
        let leaf = serde_json::to_value(TranscriptTransition::Leaf {
            leaf_id: Some("a".into()),
        })
        .expect("json");
        assert_eq!(leaf, json!({"kind":"leaf","leafId":"a"}));
        let unavailable = serde_json::to_value(TranscriptTransition::Unavailable {
            reason: TranscriptUnavailableReason::TranscriptTailTrimmed,
        })
        .expect("json");
        assert_eq!(
            unavailable,
            json!({"kind":"unavailable","reason":"transcript_tail_trimmed"})
        );
    }

    #[test]
    fn a_snapshot_is_replace_then_appends_then_leaf() {
        let tail = vec![json!({"type":"message","id":"b","parentId":"a"})];
        let transitions = snapshot_transcript(
            None,
            Some(&tail),
            Some("b"),
            &mut |entry| entry.to_string(),
            None,
        );
        assert_eq!(transitions.len(), 3);
        assert!(
            matches!(&transitions[1], TranscriptTransition::Append { id, entry: Some(_), .. } if id == "b")
        );
        assert_eq!(
            snapshot_transcript(None, None, None, &mut |e| e.to_string(), None),
            vec![TranscriptTransition::Unavailable {
                reason: TranscriptUnavailableReason::TranscriptTailTrimmed
            }]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_persisted_base_is_read_only_through_its_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        let lines: String = entries().iter().map(|e| format!("{e}\n")).collect();
        std::fs::write(&path, &lines).expect("write");
        let file = File::open(&path).expect("open");
        let identity =
            crate::session_file_identity::capture_file_identity(&file, &path.to_string_lossy())
                .expect("identity");
        let captured = capture_transcript_base(&entries());
        assert_eq!(captured.history, 1);
        let base = PersistedBase {
            file_identity: identity,
            count: captured.count,
            id_fingerprint: captured.id_fingerprint,
        };
        assert_eq!(read_persisted_base(&base, &[]).expect("read").len(), 2);
        assert_eq!(
            read_persisted_base(&base, &["missing".into()]),
            Err(PersistedBaseError::BaseFileTruncated)
        );
        let replacement = dir.path().join("next");
        std::fs::write(&replacement, &lines).expect("write");
        std::fs::rename(&replacement, &path).expect("rename");
        assert_eq!(
            read_persisted_base(&base, &[]),
            Err(PersistedBaseError::BaseFileReplaced)
        );
        std::fs::remove_file(&path).expect("remove");
        assert_eq!(
            read_persisted_base(&base, &[]),
            Err(PersistedBaseError::BaseFileMissing)
        );
    }
}
