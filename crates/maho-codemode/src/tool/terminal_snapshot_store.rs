//! Port of `tool/terminal-snapshot-store.ts` (senpi 903b8336): a bounded LRU of settled-cell
//! snapshots, bounded by count and by estimated in-memory bytes, with image payloads spilled to
//! disk and re-read on `get` (#2259); the newest snapshot is always kept.

use super::detached_cell_contract::EvalDetachedCellSnapshot;
use super::settled_image_spill::{RetainedContentPart, SettledImageSpill};
use maho_ai::types::ContentBlock;
use std::collections::VecDeque;

pub const TERMINAL_SNAPSHOT_CAP: usize = 32;

struct StoredSnapshot {
    /// Served by `list()`: every part except spilled images, so listing never reads the disk.
    snapshot: EvalDetachedCellSnapshot,
    parts: Vec<RetainedContentPart>,
    bytes: u64,
}

pub struct TerminalSnapshotStore {
    snapshots: VecDeque<StoredSnapshot>,
    cap: usize,
    /// Upper bound on the estimated in-memory bytes of all retained snapshots; 0 keeps only the count cap.
    byte_budget: u64,
    /// Moves image payloads to disk; without it images stay in memory under the byte budget.
    spill: Option<SettledImageSpill>,
    bytes: u64,
}

impl Default for TerminalSnapshotStore {
    fn default() -> Self {
        Self::new(TERMINAL_SNAPSHOT_CAP)
    }
}

impl TerminalSnapshotStore {
    pub fn new(cap: usize) -> Self {
        Self::with_budget(cap, 0, None)
    }

    pub fn with_budget(cap: usize, byte_budget: u64, spill: Option<SettledImageSpill>) -> Self {
        Self {
            snapshots: VecDeque::new(),
            cap,
            byte_budget,
            spill,
            bytes: 0,
        }
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn remember(&mut self, snapshot: EvalDetachedCellSnapshot) {
        self.delete(&snapshot.cell_id);
        let content = &snapshot.result.content;
        let parts = match &mut self.spill {
            Some(spill) => spill.spill(&snapshot.cell_id, content),
            None => content
                .iter()
                .cloned()
                .map(RetainedContentPart::Inline)
                .collect(),
        };
        let in_memory: Vec<ContentBlock> = parts
            .iter()
            .filter_map(|part| match part {
                RetainedContentPart::Inline(block) => Some(block.clone()),
                RetainedContentPart::Spilled(_) => None,
            })
            .collect();
        let stored = if in_memory.len() == content.len() {
            snapshot
        } else {
            with_content(&snapshot, in_memory)
        };
        let bytes = estimate_snapshot_bytes(&stored);
        self.snapshots.push_back(StoredSnapshot {
            snapshot: stored,
            parts,
            bytes,
        });
        self.bytes += bytes;
        while self.snapshots.len() > 1 && (self.snapshots.len() > self.cap || self.over_budget()) {
            let Some(oldest) = self
                .snapshots
                .front()
                .map(|entry| entry.snapshot.cell_id.clone())
            else {
                break;
            };
            self.delete(&oldest);
        }
    }

    pub fn get(&self, cell_id: &str) -> Option<EvalDetachedCellSnapshot> {
        let entry = self
            .snapshots
            .iter()
            .find(|entry| entry.snapshot.cell_id == cell_id)?;
        match &self.spill {
            Some(spill) if entry.parts.len() != entry.snapshot.result.content.len() => {
                Some(with_content(&entry.snapshot, spill.hydrate(&entry.parts)))
            }
            _ => Some(entry.snapshot.clone()),
        }
    }

    pub fn delete(&mut self, cell_id: &str) {
        let Some(index) = self
            .snapshots
            .iter()
            .position(|entry| entry.snapshot.cell_id == cell_id)
        else {
            return;
        };
        let Some(entry) = self.snapshots.remove(index) else {
            return;
        };
        self.bytes -= entry.bytes;
        if let Some(spill) = &mut self.spill {
            spill.release(&entry.parts);
        }
    }

    pub fn list(&self) -> Vec<&EvalDetachedCellSnapshot> {
        self.snapshots.iter().map(|entry| &entry.snapshot).collect()
    }

    pub fn clear(&mut self) {
        self.snapshots.clear();
        self.bytes = 0;
        if let Some(spill) = &mut self.spill {
            spill.clear();
        }
    }

    fn over_budget(&self) -> bool {
        self.byte_budget > 0 && self.bytes > self.byte_budget
    }
}

fn with_content(
    snapshot: &EvalDetachedCellSnapshot,
    content: Vec<ContentBlock>,
) -> EvalDetachedCellSnapshot {
    let mut copy = snapshot.clone();
    copy.result.content = content;
    copy
}

/// Estimated in-memory bytes: UTF-16 text at two bytes a unit, image base64 at one, and the JSON length
/// of jsonOutputs.
pub fn estimate_snapshot_bytes(snapshot: &EvalDetachedCellSnapshot) -> u64 {
    let units = |text: &str| text.encode_utf16().count() as u64;
    let mut bytes = units(&snapshot.output_tail) * 2;
    for part in &snapshot.result.content {
        bytes += match part {
            ContentBlock::Text(text) => units(&text.text) * 2,
            ContentBlock::Image(image) => image.data.len() as u64,
            _ => 0,
        };
    }
    let details = &snapshot.result.details;
    if let Some(cells) = details["cells"].as_array() {
        for cell in cells {
            let text = |key: &str| cell[key].as_str().map_or(0, units);
            bytes += (text("output") + text("code")) * 2;
        }
    }
    if let Some(outputs) = details["jsonOutputs"]
        .as_array()
        .filter(|outputs| !outputs.is_empty())
    {
        bytes += units(&serde_json::Value::Array(outputs.clone()).to_string());
    }
    bytes
}
