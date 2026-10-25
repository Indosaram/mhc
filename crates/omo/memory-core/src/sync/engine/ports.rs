//! The sync engine's ports (sync-design §5-§8). The engine owns ordering, the journal and the
//! lock discipline; everything that talks to the service, verifies or seals bytes, holds item CRDT
//! state or touches git sits behind these traits, so the engine is tested against deterministic
//! fakes and the real implementations plug in without changing it.
//!
//! Network and crypto ports are async (boxed, non-`Send` futures: the engine is single-threaded and
//! must run inside a `tokio::task::LocalSet`); the item store, repo, lock and clock are blocking,
//! matching the blocking git/fs/lock layers they sit on.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use super::error::EngineResult;

pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = EngineResult<T>> + 'a>>;

/// Whether a pulled entry carries a batch object or only a keyring record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Batch,
    Keyring,
}

/// One pulled sequence entry: a batch the server ordered, never its plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PulledEntry {
    pub seq: u64,
    /// A keyring record carries no object: it is verified and advances the cursor.
    pub kind: EntryKind,
    /// The batch id when the transport can name it; `None` when only the sealed header does.
    pub batch_id: Option<String>,
    /// Hex SHA-256 of the object bytes, checked on download before anything else.
    pub digest: String,
    pub object_id: String,
    /// The principal that signed the record, for the commit's provenance trailer.
    pub author: String,
}

/// A compaction snapshot covering every entry up to and including `covers_seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRef {
    pub covers_seq: u64,
    /// Its chunk objects, in order.
    pub object_ids: Vec<String>,
    /// Hex SHA-256 of a single-chunk snapshot when the server states one.
    pub digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PullPage {
    /// Present when the cursor is older than retained history: load it before `entries`.
    pub snapshot: Option<SnapshotRef>,
    pub entries: Vec<PulledEntry>,
    /// The server's head when the page was served.
    pub head: u64,
    /// More entries follow after the last one in this page.
    pub more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendResult {
    Ok { seq: u64 },
    StaleBase { head: u64 },
}

#[derive(Debug, Clone)]
pub struct AppendRequest {
    pub upload_id: String,
    pub digest: String,
    pub base_seq: u64,
    pub batch_id: String,
    pub epoch: u64,
    pub link: String,
    pub record_sig: String,
}

pub struct StreamHandlers {
    /// `sync.advanced` / `sync.keyring.changed`: a trigger carrying the advertised head, never data.
    pub on_advanced: Rc<dyn Fn(u64)>,
    /// The stream connected (true) or dropped (false); while down the engine polls.
    pub on_connected: Rc<dyn Fn(bool)>,
}

pub type Unsubscribe = Box<dyn FnOnce()>;

pub trait Transport {
    /// Entries with `seq > after`, in sequence order, at most `limit` of them.
    fn pull(&self, after: u64, limit: usize) -> PortFuture<'_, PullPage>;
    /// Object bytes, fetched through the Worker after the account check.
    fn download<'a>(&'a self, object_id: &'a str) -> PortFuture<'a, Vec<u8>>;
    /// Reserve exactly `size` bytes with this SHA-256; returns the upload id.
    fn reserve<'a>(&'a self, size: usize, digest: &'a str) -> PortFuture<'a, String>;
    fn upload<'a>(&'a self, upload_id: &'a str, bytes: &'a [u8]) -> PortFuture<'a, ()>;
    /// Append the uploaded object at `base_seq + 1`. A repost of `batch_id` answers its original
    /// seq, and the server must check that BEFORE the base: a push resumed after a crash reposts
    /// with a base that its own landed append has already moved, and answering "stale" there would
    /// duplicate it.
    fn append(&self, request: AppendRequest) -> PortFuture<'_, AppendResult>;
    /// Subscribe to the account event stream; returns the unsubscribe.
    fn subscribe(&self, handlers: StreamHandlers) -> Unsubscribe;
}

/// One item's CRDT update inside a batch plaintext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadItem {
    pub key: String,
    pub ops: Vec<u8>,
}

/// A batch's plaintext: per-item CRDT updates, opaque to the engine.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BatchPayload {
    pub items: Vec<PayloadItem>,
}

/// Why an entry was refused, as a closed set: the status shows the code, never raw crypto detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    ContentHash,
    Signature,
    ChainLink,
    Epoch,
    AccountBinding,
    ObjectBinding,
    ReplayedContent,
    SequenceGap,
    Malformed,
}

impl RefusalCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContentHash => "content_hash",
            Self::Signature => "signature",
            Self::ChainLink => "chain_link",
            Self::Epoch => "epoch",
            Self::AccountBinding => "account_binding",
            Self::ObjectBinding => "object_binding",
            Self::ReplayedContent => "replayed_content",
            Self::SequenceGap => "sequence_gap",
            Self::Malformed => "malformed",
        }
    }
}

impl std::fmt::Display for RefusalCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// `batch_id` is the one the sealed header names; the server's rows may carry none, so the
    /// verifier is the authority for at-most-once per (seq, batch id).
    Ok {
        payload: BatchPayload,
        batch_id: String,
    },
    Refused(RefusalCode),
}

/// Receiver checks of sync-design §6 in order, then the plaintext. Nothing reaches the item store
/// without passing here.
pub trait Verifier {
    fn verify_entry<'a>(
        &'a self,
        entry: &'a PulledEntry,
        bytes: &'a [u8],
    ) -> PortFuture<'a, Verdict>;
    fn verify_snapshot<'a>(
        &'a self,
        snapshot: &'a SnapshotRef,
        chunks: &'a [Vec<u8>],
    ) -> PortFuture<'a, Verdict>;
}

/// A sealed batch: the object bytes plus the chain fields the append carries (sync-design §6).
#[derive(Debug, Clone)]
pub struct SealedBatch {
    pub bytes: Vec<u8>,
    pub epoch: u64,
    pub link: String,
    pub record_sig: String,
}

/// The outgoing half of the crypto port: encrypt and sign a batch for upload.
pub trait Sealer {
    fn seal<'a>(
        &'a self,
        batch_id: &'a str,
        base_seq: u64,
        payload: &'a BatchPayload,
    ) -> PortFuture<'a, SealedBatch>;
}

/// The item model (device API) as the engine uses it.
pub trait ItemStore {
    /// Loads persisted CRDT state and baselines; the engine calls it once before a cycle.
    fn load(&self) -> EngineResult<()>;
    /// This device's peer id; a journal created under another peer belongs to another device.
    fn peer(&self) -> EngineResult<String>;
    /// Item keys the store knows, live or dead.
    fn keys(&self) -> Vec<String>;
    /// captureOnDevice: the working text against the item's materialised baseline; `None` = no edit.
    fn capture(&self, key: &str, working_text: Option<&str>) -> EngineResult<Option<Vec<u8>>>;
    /// Import a verified remote update (or this device's own captured ops).
    fn apply(&self, key: &str, ops: &[u8]) -> EngineResult<()>;
    /// renderForDisk: the item's merged text, or `None` for a dead item. `disk_text` is the file
    /// the render will replace (`None` when absent).
    fn render(&self, key: &str, disk_text: Option<&str>) -> EngineResult<Option<String>>;
    /// The item doc's version (a stable string of its frontier): a render is keyed by (key, version).
    fn version(&self, key: &str) -> EngineResult<String>;
    /// recordMaterialized: the text the engine wrote is the item's new baseline.
    fn mark_materialized(&self, key: &str, text: Option<&str>) -> EngineResult<()>;
    /// Import a compaction snapshot's state; local pending state is kept.
    fn apply_snapshot(&self, payload: &BatchPayload) -> EngineResult<()>;
    /// Make CRDT state and baselines durable (temp file + rename) before the journal advances.
    fn persist(&self) -> EngineResult<()>;
}

/// The memory writer lock: network work never runs while it is held.
pub trait WriterLock {
    fn with_lock(&self, f: &mut dyn FnMut() -> EngineResult<()>) -> EngineResult<()>;
}

pub type Cancel = Box<dyn FnOnce()>;

pub trait Clock {
    fn now(&self) -> i64;
    /// Schedule `f` after `ms`; returns the cancel.
    fn after(&self, ms: u64, f: Box<dyn FnOnce()>) -> Cancel;
}

/// The memory repository: working tree paths, the sync commit and its trailer.
pub trait SyncRepo {
    /// The working tree root; item key `k` lives at `<root>/<k>`.
    fn root(&self) -> &Path;
    /// The git directory; the journal lives in `<git_dir>/omo-sync/`.
    fn git_dir(&self) -> &Path;
    fn head(&self) -> EngineResult<Option<String>>;
    /// HEAD's tree id; a cycle whose staged tree equals it has nothing to commit.
    fn head_tree(&self) -> EngineResult<String>;
    /// The tree id a commit of HEAD plus `paths` (as they are in the working tree) would have,
    /// computed in a private index: the shared index is left untouched.
    fn stage_tree(&self, paths: &[String]) -> EngineResult<String>;
    /// Commit exactly `paths` as `OmO Sync` with `message` (trailers included).
    fn commit(&self, paths: &[String], message: &str) -> EngineResult<String>;
    /// Reconcile only committed paths in the shared index, including after crash recovery.
    fn sync_index(&self, paths: &[String]) -> EngineResult<()>;
    /// True when a commit reachable from HEAD carries `Sync-Seq: <range>`.
    fn has_sync_commit(&self, range: &str) -> EngineResult<bool>;
}
