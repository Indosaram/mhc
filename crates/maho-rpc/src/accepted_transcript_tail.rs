//! Port of senpi `packages/coding-agent/src/modes/rpc/accepted-transcript-tail.ts`.
//!
//! Bounded staging of unpersisted accepted transcript entries, consistent tail holds, and
//! persistence-driven release.

use maho_core::session_transcript_journal::{TranscriptPersistence, TranscriptTransition};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

/// Tail bytes one epoch's transcript may hold in its store (256 MiB in UTF-16 code units * 2).
pub const DEFAULT_TRANSCRIPT_TAIL_BYTES: usize = 256 * 1024 * 1024;
/// Tail bytes the staging store may hold in RAM, unspilled (32 MiB).
pub const TRANSCRIPT_TAIL_RESIDENT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailReadFailure {
    TranscriptSegmentMissing,
    TranscriptSegmentMismatch,
}

impl TailReadFailure {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TranscriptSegmentMissing => "transcript_segment_missing",
            Self::TranscriptSegmentMismatch => "transcript_segment_mismatch",
        }
    }
}

#[derive(Debug, Default)]
pub struct TailBudget {
    pub max: usize,
    pub held: usize,
}

impl TailBudget {
    pub fn new(max: usize) -> Self {
        Self { max, held: 0 }
    }

    pub fn with_default() -> Self {
        Self::new(DEFAULT_TRANSCRIPT_TAIL_BYTES)
    }
}

#[derive(Debug, Clone)]
pub struct StagedTailEntry {
    pub index: usize,
    pub text: Arc<String>,
    pub utf16_bytes: usize,
    pub sha256: String,
    pub pins: usize,
    pub spilled: bool,
}

impl StagedTailEntry {
    pub fn new(index: usize, text: String) -> Self {
        let utf16_bytes = text.encode_utf16().count() * 2;
        let sha256 = hex::encode(Sha256::digest(text.as_bytes()));
        Self {
            index,
            text: Arc::new(text),
            utf16_bytes,
            sha256,
            pins: 0,
            spilled: false,
        }
    }
}

pub struct TailHold {
    pub file_count: usize,
    pub persistence: Option<TranscriptPersistence>,
    held: BTreeMap<usize, Arc<Mutex<StagedTailEntry>>>,
    released: bool,
}

impl TailHold {
    pub fn has(&self, index: usize) -> bool {
        self.held.contains_key(&index)
    }

    pub fn get_text(&self, index: usize) -> Option<Arc<String>> {
        let entry = self.held.get(&index)?;
        let guard = entry.lock().unwrap_or_else(PoisonError::into_inner);
        Some(Arc::clone(&guard.text))
    }

    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        for entry in self.held.values() {
            let mut guard = entry.lock().unwrap_or_else(PoisonError::into_inner);
            guard.pins = guard.pins.saturating_sub(1);
        }
    }
}

impl Drop for TailHold {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Debug)]
pub struct TranscriptTail {
    pub entries: BTreeMap<usize, Arc<Mutex<StagedTailEntry>>>,
    pub kept: BTreeMap<usize, Arc<Mutex<StagedTailEntry>>>,
    pub retained_bytes: usize,
    pub persistence: Option<TranscriptPersistence>,
    budget: Arc<Mutex<TailBudget>>,
    cuts: BTreeMap<usize, usize>,
    superseded: bool,
}

impl TranscriptTail {
    pub fn new(persistence: Option<TranscriptPersistence>, budget: Arc<Mutex<TailBudget>>) -> Self {
        Self {
            entries: BTreeMap::new(),
            kept: BTreeMap::new(),
            retained_bytes: 0,
            persistence,
            budget,
            cuts: BTreeMap::new(),
            superseded: false,
        }
    }

    pub fn stage(&self, index: usize, text: &str) -> Option<StagedTailEntry> {
        let bytes = text.encode_utf16().count() * 2;
        let budget = self.budget.lock().unwrap_or_else(PoisonError::into_inner);
        if budget.held + bytes > budget.max
            || self.retained_bytes + bytes > TRANSCRIPT_TAIL_RESIDENT_BYTES
        {
            return None;
        }
        Some(StagedTailEntry::new(index, text.to_owned()))
    }

    pub fn adopt(&mut self, index: usize, entry: StagedTailEntry) {
        let bytes = entry.utf16_bytes;
        self.retained_bytes += bytes;
        self.budget
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .held += bytes;
        self.entries.insert(index, Arc::new(Mutex::new(entry)));
    }

    pub fn retain_cut(&mut self, count: usize) {
        *self.cuts.entry(count).or_insert(0) += 1;
    }

    pub fn supersede(&mut self) {
        self.superseded = true;
        if self.cuts.is_empty() {
            self.release_all();
            return;
        }
        for entry in self.entries.values() {
            entry.lock().unwrap_or_else(PoisonError::into_inner).spilled = true;
        }
    }

    pub fn release_cut(&mut self, count: usize) {
        if let Some(left) = self.cuts.get_mut(&count) {
            if *left <= 1 {
                self.cuts.remove(&count);
            } else {
                *left -= 1;
            }
        }
        if self.superseded && self.cuts.is_empty() {
            self.release_all();
            return;
        }
        let reach = self.reach();
        let drop_keys: Vec<usize> = self
            .kept
            .keys()
            .copied()
            .filter(|&idx| idx >= reach)
            .collect();
        for idx in drop_keys {
            if let Some(entry) = self.kept.remove(&idx) {
                self.free(&entry);
            }
        }
    }

    pub fn persisted(&mut self, persistence: TranscriptPersistence, base_count: usize) {
        let persisted_count = persistence.count;
        self.persistence = Some(persistence);
        let reach = self.reach();
        let to_move: Vec<usize> = self
            .entries
            .keys()
            .copied()
            .take_while(|&idx| idx + base_count < persisted_count)
            .collect();
        for idx in to_move {
            if let Some(entry) = self.entries.remove(&idx) {
                let bytes = entry
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .utf16_bytes;
                self.retained_bytes = self.retained_bytes.saturating_sub(bytes);
                if idx >= reach {
                    self.free(&entry);
                } else {
                    entry.lock().unwrap_or_else(PoisonError::into_inner).spilled = true;
                    self.kept.insert(idx, entry);
                }
            }
        }
    }

    pub fn release_all(&mut self) {
        let entries: Vec<_> = std::mem::take(&mut self.entries).into_values().collect();
        let kept: Vec<_> = std::mem::take(&mut self.kept).into_values().collect();
        for entry in entries.into_iter().chain(kept) {
            self.free(&entry);
        }
        self.retained_bytes = 0;
    }

    pub fn hold(&self, count: usize, base_count: usize) -> TailHold {
        let own = |idx: usize| self.entries.get(&idx).or_else(|| self.kept.get(&idx));
        let mut file_count = count.min(
            self.persistence
                .as_ref()
                .map_or(0, |p| p.count)
                .saturating_sub(base_count),
        );
        while file_count > 0 && own(file_count - 1).is_some() {
            file_count -= 1;
        }
        let mut held = BTreeMap::new();
        for idx in file_count..count {
            if let Some(entry) = own(idx) {
                entry.lock().unwrap_or_else(PoisonError::into_inner).pins += 1;
                held.insert(idx, Arc::clone(entry));
            }
        }
        TailHold {
            file_count,
            persistence: self.persistence.clone(),
            held,
            released: false,
        }
    }

    fn free(&self, entry: &Mutex<StagedTailEntry>) {
        let bytes = entry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .utf16_bytes;
        let mut budget = self.budget.lock().unwrap_or_else(PoisonError::into_inner);
        budget.held = budget.held.saturating_sub(bytes);
    }

    fn reach(&self) -> usize {
        self.cuts.keys().copied().max().unwrap_or(0)
    }
}

pub fn stage_bootstrap(
    transitions: &[TranscriptTransition],
) -> Option<HashMap<usize, StagedTailEntry>> {
    let mut out = HashMap::new();
    let mut index = 0usize;
    let mut held = 0usize;
    for (pos, t) in transitions.iter().enumerate() {
        match t {
            TranscriptTransition::Replace { .. } => index = 0,
            TranscriptTransition::Append { entry, .. } => {
                if let Some(text) = entry {
                    let staged = StagedTailEntry::new(index, text.clone());
                    if held + staged.utf16_bytes > DEFAULT_TRANSCRIPT_TAIL_BYTES
                        || held + staged.utf16_bytes > TRANSCRIPT_TAIL_RESIDENT_BYTES
                    {
                        return None;
                    }
                    held += staged.utf16_bytes;
                    out.insert(pos, staged);
                }
                index += 1;
            }
            _ => {}
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_pins_kept_entries_across_persisted_until_cut_released() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("session.jsonl");
        std::fs::write(&file_path, "{\"id\":\"0\"}\n").expect("write");
        let file = std::fs::File::open(&file_path).expect("open");
        let file_identity = maho_core::session_file_identity::capture_file_identity(
            &file,
            file_path.to_string_lossy().as_ref(),
        )
        .expect("identity");
        let budget = Arc::new(Mutex::new(TailBudget::with_default()));
        let mut tail = TranscriptTail::new(None, Arc::clone(&budget));
        let e0 = tail.stage(0, "{\"id\":\"0\"}").expect("stage");
        tail.adopt(0, e0);
        tail.retain_cut(1);
        tail.persisted(
            TranscriptPersistence {
                file_identity,
                count: 1,
            },
            0,
        );
        assert!(tail.entries.is_empty());
        assert!(tail.kept.contains_key(&0));
        let hold = tail.hold(1, 0);
        assert_eq!(hold.file_count, 0);
        assert!(hold.has(0));
        drop(hold);
        tail.release_cut(1);
        assert!(tail.kept.is_empty());
        assert_eq!(
            budget.lock().unwrap_or_else(PoisonError::into_inner).held,
            0
        );
    }
}
