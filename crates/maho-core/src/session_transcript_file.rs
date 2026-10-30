//! Port of senpi packages/coding-agent/src/core/session-transcript-file.ts.
//!
//! One lifecycle's identity and optional loaded base. Upstream captures the identity asynchronously and a
//! superseded capture cannot publish (generation guard). The Rust capture is synchronous (`fstat` on the
//! owning descriptor), so the identity is installed in the same call; the generation still guards a
//! capture handed in after a `reset` that superseded it.

use serde_json::Value;

use crate::session_transcript_journal::{
    FileIdentity, PersistedBase, TranscriptUnavailableReason, capture_transcript_base,
};

#[derive(Clone, Debug, Default)]
pub struct TranscriptFile {
    pub base: Option<PersistedBase>,
    pub history: usize,
    pub identity: Option<FileIdentity>,
    pub unavailable: Option<TranscriptUnavailableReason>,
    generation: u64,
}

impl TranscriptFile {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn reset(&mut self) {
        self.generation += 1;
        self.base = None;
        self.history = 0;
        self.identity = None;
        self.unavailable = None;
    }

    /// Install a capture taken by the generation `reset` returned; `entries` (the loaded or written
    /// entries) make it a persisted base. Returns false when a later reset superseded it.
    pub fn capture(&mut self, identity: Option<FileIdentity>, entries: Option<&[Value]>) -> bool {
        self.reset();
        self.install(self.generation, identity, entries)
    }

    pub fn install(
        &mut self,
        generation: u64,
        identity: Option<FileIdentity>,
        entries: Option<&[Value]>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let captured = entries.map(capture_transcript_base);
        match identity {
            None => {
                self.unavailable =
                    Some(TranscriptUnavailableReason::TranscriptFileIdentityUnavailable)
            }
            Some(file_identity) => {
                if let Some(captured) = captured {
                    self.base = Some(PersistedBase {
                        file_identity: file_identity.clone(),
                        count: captured.count,
                        id_fingerprint: captured.id_fingerprint,
                    });
                    self.history = captured.history;
                }
                self.identity = Some(file_identity);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity() -> FileIdentity {
        FileIdentity {
            path: "/s.jsonl".into(),
            dev: "1".into(),
            ino: "2".into(),
        }
    }

    #[test]
    fn a_capture_with_entries_is_a_base_and_a_superseded_one_cannot_publish() {
        let mut file = TranscriptFile::default();
        let entries = [
            json!({"type":"session","id":"s"}),
            json!({"type":"message","id":"a"}),
        ];
        assert!(file.capture(Some(identity()), Some(&entries)));
        assert_eq!(file.history, 1);
        assert_eq!(file.base.as_ref().map(|base| base.count), Some(2));
        let stale = file.generation();
        file.reset();
        assert!(!file.install(stale, Some(identity()), None));
        assert!(file.identity.is_none());
    }

    #[test]
    fn a_missing_identity_marks_the_transcript_unavailable() {
        let mut file = TranscriptFile::default();
        assert!(file.capture(None, None));
        assert_eq!(
            file.unavailable,
            Some(TranscriptUnavailableReason::TranscriptFileIdentityUnavailable)
        );
        assert!(file.base.is_none());
    }
}
