//! The apply journal in `<gitDir>/omo-sync/` (sync-design §5). Incoming batches move
//! downloaded -> verified on their own record; the cycle that applies them moves
//! merged -> rendered -> committed -> checkpointed as one unit and lists the batches it covers.
//! The applied cursor moves only at `checkpointed`; the event cursor (newest advertised head) is
//! kept apart and never decides what is pulled. Outgoing ops are journaled as `pending` the moment
//! they are captured, before anything is rendered.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use super::atomic::{TEMP_MARK, read_json, remove_atomic, write_json};
use super::error::{EngineError, EngineResult};
use super::ports::{PulledEntry, RefusalCode};
use crate::fs::resilient;
use crate::support::random::random_id;
use crate::support::time::now_millis;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IncomingStage {
    Downloaded,
    Verified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CycleStage {
    Merged,
    Rendered,
    Committed,
    Checkpointed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireItem {
    pub key: String,
    /// base64 of the item's CRDT update
    pub ops: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingRecord {
    pub entry: PulledEntry,
    pub stage: IncomingStage,
    pub items: Vec<WireItem>,
    pub snapshot: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleRecord {
    pub id: String,
    pub stage: CycleStage,
    pub first: u64,
    pub last: u64,
    pub batches: Vec<PulledEntry>,
    pub keys: Vec<String>,
    pub tree_id: Option<String>,
    /// Pre-merge-build cycle records carried the render inline; read only by `legacy_cycle`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texts: Option<serde_json::Value>,
}

/// One render step of a cycle, appended to `renders/<cycle id>/` and never rewritten. The render is
/// staged at `staged_path(id, seq)` BEFORE this record is written and moved into place after it, so
/// whether it landed is decidable on any resume: a write landed iff its staged file is gone; a
/// delete landed iff its staged file exists. `frontier` is the item doc's version the text was
/// rendered at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderRecord {
    pub seq: u64,
    pub key: String,
    pub frontier: String,
    pub hash: String,
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paused {
    pub seq: u64,
    pub code: RefusalCode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct EngineState {
    /// The newest seq verified, applied and checkpointed on this device.
    pub applied: u64,
    /// The newest head the event stream advertised; a trigger only.
    pub event_head: u64,
    pub refused: u64,
    pub cycle: Option<CycleRecord>,
    /// The entry the pull stopped at; cleared once a later pull verifies past it.
    pub paused: Option<Paused>,
    pub last_synced_at: Option<i64>,
    /// Memories the last refusal-free cycle updated on this device.
    pub updated_while_offline: u64,
    /// The device peer this journal was created under.
    pub peer: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutgoingStage {
    Sealed,
    Reserved,
    Uploaded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutgoingBatch {
    pub batch_id: String,
    pub base_seq: u64,
    pub digest: String,
    pub bytes: String,
    pub upload_id: Option<String>,
    pub epoch: u64,
    pub link: String,
    pub record_sig: String,
    pub stage: OutgoingStage,
    pub items: Vec<WireItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PendingOps {
    #[serde(default)]
    pub items: Vec<WireItem>,
    #[serde(default)]
    pub outgoing: Option<OutgoingBatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedRef {
    pub seq: u64,
    pub digest: String,
}

pub struct SyncJournal {
    pub dir: PathBuf,
    pub tmp_dir: PathBuf,
}

fn list_names(dir: &Path) -> EngineResult<Vec<String>> {
    match resilient::read_dir_names(dir) {
        Ok(names) => Ok(names),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

impl SyncJournal {
    pub fn new(git_dir: &Path) -> Self {
        let dir = git_dir.join("omo-sync");
        let tmp_dir = dir.join("tmp");
        Self { dir, tmp_dir }
    }

    pub fn state(&self) -> EngineResult<EngineState> {
        read_json(&self.dir.join("state.json"), EngineState::default())
    }

    pub fn update_state(&self, patch: impl FnOnce(&mut EngineState)) -> EngineResult<EngineState> {
        let mut next = self.state()?;
        patch(&mut next);
        write_json(&self.dir.join("state.json"), &next)?;
        Ok(next)
    }

    /// Moves the whole journal aside (another device's, carried by a copied repo); a fresh one starts empty.
    pub fn retire(&self) -> EngineResult<()> {
        let suffix: String = random_id().chars().take(8).collect();
        let aside = PathBuf::from(format!(
            "{}.retired-{}-{suffix}",
            self.dir.display(),
            now_millis()
        ));
        match resilient::rename(&self.dir, &aside) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn renders_dir(&self, id: &str) -> PathBuf {
        self.dir.join("renders").join(id)
    }

    /// Where the render of record `seq` is staged (writes) or moved aside to (deletes).
    pub fn staged_path(&self, id: &str, seq: u64) -> PathBuf {
        self.renders_dir(id).join(format!("{seq:08}.staged"))
    }

    pub fn renders(&self, id: &str) -> EngineResult<Vec<RenderRecord>> {
        let dir = self.renders_dir(id);
        let mut records = Vec::new();
        for name in list_names(&dir)? {
            if !name.ends_with(".json") || name.starts_with(TEMP_MARK) {
                continue;
            }
            if let Some(record) = read_json::<Option<RenderRecord>>(&dir.join(&name), None)? {
                records.push(record);
            }
        }
        records.sort_by_key(|r| r.seq);
        Ok(records)
    }

    /// The seq the next render record of this cycle takes (records are append-only).
    pub fn next_render_seq(&self, id: &str) -> EngineResult<u64> {
        Ok(self.renders(id)?.last().map_or(0, |r| r.seq) + 1)
    }

    pub fn append_render(&self, id: &str, record: &RenderRecord) -> EngineResult<()> {
        write_json(
            &self.renders_dir(id).join(format!("{:08}.json", record.seq)),
            record,
        )
    }

    pub fn render_landed(&self, id: &str, record: &RenderRecord) -> bool {
        let staged = resilient::exists(&self.staged_path(id, record.seq));
        if record.text.is_none() {
            staged
        } else {
            !staged
        }
    }

    /// Drops every cycle's render journal except `keep` (the cycle still in flight, if any).
    pub fn clear_renders(&self, keep: Option<&str>) -> EngineResult<()> {
        let root = self.dir.join("renders");
        for name in list_names(&root)? {
            if Some(name.as_str()) != keep {
                resilient::remove_dir_all(&root.join(name))?;
            }
        }
        Ok(())
    }

    pub fn put_incoming(&self, record: &IncomingRecord) -> EngineResult<()> {
        write_json(&self.incoming_path(record.entry.seq), record)
    }

    pub fn incoming(&self) -> EngineResult<Vec<IncomingRecord>> {
        let dir = self.dir.join("incoming");
        let mut records = Vec::new();
        for name in list_names(&dir)? {
            if !name.ends_with(".json") || name.starts_with(TEMP_MARK) {
                continue;
            }
            if let Some(record) = read_json::<Option<IncomingRecord>>(&dir.join(&name), None)? {
                records.push(record);
            }
        }
        records.sort_by_key(|r| r.entry.seq);
        Ok(records)
    }

    pub fn drop_incoming(&self, seq: u64) -> EngineResult<()> {
        remove_atomic(&self.incoming_path(seq))
    }

    /// Applied batch ids with the seq and digest they were applied at (at-most-once apply).
    pub fn applied(&self) -> EngineResult<BTreeMap<String, AppliedRef>> {
        read_json(&self.dir.join("applied.json"), BTreeMap::new())
    }

    pub fn record_applied(&self, entries: &[PulledEntry]) -> EngineResult<()> {
        let mut index = self.applied()?;
        for entry in entries {
            if let Some(batch_id) = &entry.batch_id {
                index.insert(
                    batch_id.clone(),
                    AppliedRef {
                        seq: entry.seq,
                        digest: entry.digest.clone(),
                    },
                );
            }
        }
        write_json(&self.dir.join("applied.json"), &index)
    }

    pub fn pending(&self) -> EngineResult<PendingOps> {
        read_json(&self.dir.join("pending.json"), PendingOps::default())
    }

    pub fn set_pending(&self, pending: &PendingOps) -> EngineResult<()> {
        write_json(&self.dir.join("pending.json"), pending)
    }

    fn incoming_path(&self, seq: u64) -> PathBuf {
        self.dir.join("incoming").join(format!("{seq:012}.json"))
    }
}

pub fn to_base64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn from_base64(text: &str) -> EngineResult<Vec<u8>> {
    STANDARD
        .decode(text)
        .map_err(|e| EngineError::Other(format!("invalid base64 in journal: {e}")))
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
