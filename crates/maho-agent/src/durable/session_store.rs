//! Port of senpi `packages/agent/src/durable/session-store.ts`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, json};
use tokio::sync::Mutex as AsyncMutex;

use super::store::{
    DurableStore, DurableStoreError, DurableStoreErrorCode, RecordScan, ScanOrder, StoreValue,
    StoreWrite, SyncCommitStore,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEntry {
    pub key: String,
    pub line: String,
}

pub type SessionHeader = Map<String, StoreValue>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredSession {
    pub id: String,
    pub header: SessionHeader,
    pub entries: Vec<StoredEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStoreErrorCode {
    DuplicateEntry,
    CorruptSession,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionStoreError {
    #[error("{message}")]
    Session {
        code: SessionStoreErrorCode,
        message: String,
    },
    #[error(transparent)]
    Store(#[from] DurableStoreError),
}

impl SessionStoreError {
    pub fn new(code: SessionStoreErrorCode, message: impl Into<String>) -> Self {
        Self::Session {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> Option<SessionStoreErrorCode> {
        match self {
            Self::Session { code, .. } => Some(*code),
            Self::Store(_) => None,
        }
    }
}

const SESSION: &str = "session-import/session/";
const DIVERGENCE: &str = "session-import/divergence/";

fn encode_uri_component(input: &str) -> String {
    // Matches JS encodeURIComponent unreserved set: A-Z a-z 0-9 - _ . ! ~ * ' ( )
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let unreserved = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            );
        if unreserved {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

fn session_manifest_key(id: &str) -> String {
    format!("{SESSION}{}", encode_uri_component(id))
}

fn divergence_key(id: &str) -> String {
    format!("{DIVERGENCE}{}", encode_uri_component(id))
}

fn session_record_kind(id: &str) -> String {
    format!("session/{}", encode_uri_component(id))
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionState {
    pub id: String,
    pub header: SessionHeader,
    pub entries: Vec<StoredEntry>,
    pub divergence: StoreValue,
    /// How many rewrite records the session holds; an append record's id is scoped to it.
    pub rewrites: usize,
}

impl SessionState {
    pub fn into_stored(self) -> StoredSession {
        StoredSession {
            id: self.id,
            header: self.header,
            entries: self.entries,
        }
    }
}

struct Plan {
    writes: Vec<StoreWrite>,
    next: SessionState,
}

fn read_entry(id: &str, value: &StoreValue) -> Result<StoredEntry, SessionStoreError> {
    let Some(obj) = value.as_object() else {
        return Err(SessionStoreError::new(
            SessionStoreErrorCode::CorruptSession,
            format!("session {id} holds an unreadable entry record"),
        ));
    };
    let (Some(key), Some(line)) = (
        obj.get("key").and_then(StoreValue::as_str),
        obj.get("line").and_then(StoreValue::as_str),
    ) else {
        return Err(SessionStoreError::new(
            SessionStoreErrorCode::CorruptSession,
            format!("session {id} holds an unreadable entry record"),
        ));
    };
    Ok(StoredEntry {
        key: key.to_owned(),
        line: line.to_owned(),
    })
}

/// The session as committed: the last rewrite, then every later append; a key seen twice keeps its first record.
pub async fn read_session_state(
    store: &dyn DurableStore,
    id: &str,
) -> Result<Option<SessionState>, SessionStoreError> {
    let Some(manifest) = store.get_document(&session_manifest_key(id)).await? else {
        return Ok(None);
    };
    let Some(manifest_obj) = manifest.as_object() else {
        return Err(SessionStoreError::new(
            SessionStoreErrorCode::CorruptSession,
            format!("session {id} has no readable manifest"),
        ));
    };
    let Some(header_obj) = manifest_obj.get("header").and_then(StoreValue::as_object) else {
        return Err(SessionStoreError::new(
            SessionStoreErrorCode::CorruptSession,
            format!("session {id} has no readable manifest"),
        ));
    };
    if manifest_obj.contains_key("entryIds") {
        return Err(SessionStoreError::new(
            SessionStoreErrorCode::CorruptSession,
            format!(
                "session {id} is stored in the layout before the record log; re-import it from its JSONL file"
            ),
        ));
    }
    let mut header = header_obj.clone();
    let mut entries = Vec::new();
    let mut held = HashSet::new();
    let mut rewrites = 0usize;
    let records = store
        .scan_records(RecordScan {
            kind: Some(session_record_kind(id)),
            order: Some(ScanOrder::Asc),
            ..Default::default()
        })
        .await?;
    for record in records {
        let Some(data) = record.data.as_object() else {
            return Err(SessionStoreError::new(
                SessionStoreErrorCode::CorruptSession,
                format!("session {id} holds an unreadable record {}", record.seq),
            ));
        };
        let op = data.get("op").and_then(StoreValue::as_str);
        if op == Some("rewrite")
            && let Some(new_header) = data.get("header").and_then(StoreValue::as_object)
            && let Some(raw_entries) = data.get("entries").and_then(StoreValue::as_array)
        {
            header = new_header.clone();
            entries = raw_entries
                .iter()
                .map(|entry| read_entry(id, entry))
                .collect::<Result<Vec<_>, _>>()?;
            held = entries.iter().map(|entry| entry.key.clone()).collect();
            rewrites += 1;
        } else if op == Some("append") {
            let entry = read_entry(id, &record.data)?;
            if held.contains(&entry.key) {
                continue;
            }
            held.insert(entry.key.clone());
            entries.push(entry);
        } else {
            return Err(SessionStoreError::new(
                SessionStoreErrorCode::CorruptSession,
                format!("session {id} holds an unreadable record {}", record.seq),
            ));
        }
    }
    let divergence = store
        .get_document(&divergence_key(id))
        .await?
        .unwrap_or(StoreValue::Null);
    Ok(Some(SessionState {
        id: id.to_owned(),
        header,
        entries,
        divergence,
        rewrites,
    }))
}

pub fn manifest_write(id: &str, header: &SessionHeader) -> StoreWrite {
    StoreWrite::Put {
        key: session_manifest_key(id),
        data: json!({
            "id": id,
            "header": header,
        }),
    }
}

pub fn divergence_write(id: &str, divergence: StoreValue) -> StoreWrite {
    StoreWrite::Put {
        key: divergence_key(id),
        data: divergence,
    }
}

pub fn append_record_writes(id: &str, rewrites: usize, added: &[StoredEntry]) -> Vec<StoreWrite> {
    let kind = session_record_kind(id);
    added
        .iter()
        .map(|entry| StoreWrite::Append {
            id: format!("{kind}/{rewrites}/{}", encode_uri_component(&entry.key)),
            kind: kind.clone(),
            data: json!({
                "op": "append",
                "key": entry.key,
                "line": entry.line,
            }),
        })
        .collect()
}

pub fn rewrite_record_write(
    store: &dyn DurableStore,
    id: &str,
    header: &SessionHeader,
    entries: &[StoredEntry],
) -> StoreWrite {
    let serialized_entries: Vec<StoreValue> = entries
        .iter()
        .map(|entry| json!({ "key": entry.key, "line": entry.line }))
        .collect();
    StoreWrite::Append {
        id: store.mint_id(),
        kind: session_record_kind(id),
        data: json!({
            "op": "rewrite",
            "header": header,
            "entries": serialized_entries,
        }),
    }
}

fn refuse_repeats<'a>(
    id: &str,
    entries: &[StoredEntry],
    held: impl IntoIterator<Item = &'a str>,
    what: &str,
) -> Result<(), SessionStoreError> {
    let mut seen: HashSet<&str> = held.into_iter().collect();
    for entry in entries {
        if !seen.insert(entry.key.as_str()) {
            return Err(SessionStoreError::new(
                SessionStoreErrorCode::DuplicateEntry,
                format!("{what} {id} repeats {}", entry.key),
            ));
        }
    }
    Ok(())
}

fn plan_append(
    state: &SessionState,
    held: bool,
    added: &[StoredEntry],
) -> Result<Plan, SessionStoreError> {
    refuse_repeats(
        &state.id,
        added,
        state.entries.iter().map(|entry| entry.key.as_str()),
        "session",
    )?;
    let mut next = state.clone();
    next.entries.extend(added.iter().cloned());
    let mut writes = Vec::new();
    if !held {
        writes.push(manifest_write(&state.id, &state.header));
    }
    writes.extend(append_record_writes(&state.id, state.rewrites, added));
    Ok(Plan { writes, next })
}

fn plan_rewrite(
    store: &dyn DurableStore,
    state: &SessionState,
    held: bool,
    header: SessionHeader,
    entries: Vec<StoredEntry>,
) -> Result<Plan, SessionStoreError> {
    refuse_repeats(&state.id, &entries, std::iter::empty(), "rewrite of")?;
    let mut next = state.clone();
    next.header = header.clone();
    next.entries = entries.clone();
    next.rewrites += 1;
    let mut writes = Vec::new();
    if !held {
        writes.push(manifest_write(&state.id, &header));
    }
    writes.push(rewrite_record_write(store, &state.id, &header, &entries));
    Ok(Plan { writes, next })
}

fn as_duplicate_entry(id: &str, error: DurableStoreError) -> SessionStoreError {
    if error.code() == Some(DurableStoreErrorCode::DuplicateRecordId) {
        SessionStoreError::new(
            SessionStoreErrorCode::DuplicateEntry,
            format!("session {id} already holds an entry this write repeats"),
        )
    } else {
        SessionStoreError::Store(error)
    }
}

async fn open_state(
    store: &dyn DurableStore,
    id: &str,
    header: SessionHeader,
) -> Result<(SessionState, bool), SessionStoreError> {
    match read_session_state(store, id).await? {
        Some(read) => Ok((read, true)),
        None => Ok((
            SessionState {
                id: id.to_owned(),
                header,
                entries: Vec::new(),
                divergence: StoreValue::Null,
                rewrites: 0,
            },
            false,
        )),
    }
}

struct HandleInner {
    state: SessionState,
    held: bool,
}

#[derive(Clone)]
pub struct SessionHandle {
    store: Arc<dyn DurableStore>,
    epoch: u64,
    id: String,
    inner: Arc<Mutex<HandleInner>>,
    queue: Arc<AsyncMutex<()>>,
}

impl SessionHandle {
    pub fn header(&self) -> SessionHeader {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .header
            .clone()
    }

    pub fn entries(&self) -> Vec<StoredEntry> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .entries
            .clone()
    }

    pub async fn append(&self, entries: Vec<StoredEntry>) -> Result<(), SessionStoreError> {
        let _guard = self.queue.lock().await;
        let plan = {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            plan_append(&inner.state, inner.held, &entries)?
        };
        self.store
            .commit(self.epoch, plan.writes, Box::new(|_| {}))
            .await
            .map_err(|error| as_duplicate_entry(&self.id, error))?;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.state = plan.next;
        inner.held = true;
        Ok(())
    }

    pub async fn rewrite(
        &self,
        header: SessionHeader,
        entries: Vec<StoredEntry>,
    ) -> Result<(), SessionStoreError> {
        let _guard = self.queue.lock().await;
        let plan = {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            plan_rewrite(
                self.store.as_ref(),
                &inner.state,
                inner.held,
                header,
                entries,
            )?
        };
        self.store
            .commit(self.epoch, plan.writes, Box::new(|_| {}))
            .await
            .map_err(|error| as_duplicate_entry(&self.id, error))?;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.state = plan.next;
        inner.held = true;
        Ok(())
    }
}

#[derive(Clone)]
pub struct SessionStore {
    store: Arc<dyn DurableStore>,
    epoch: u64,
}

impl SessionStore {
    pub async fn load(&self, id: &str) -> Result<Option<StoredSession>, SessionStoreError> {
        Ok(read_session_state(self.store.as_ref(), id)
            .await?
            .map(SessionState::into_stored))
    }

    pub async fn open(
        &self,
        id: &str,
        header: SessionHeader,
    ) -> Result<SessionHandle, SessionStoreError> {
        let (state, held) = open_state(self.store.as_ref(), id, header).await?;
        Ok(SessionHandle {
            store: Arc::clone(&self.store),
            epoch: self.epoch,
            id: id.to_owned(),
            inner: Arc::new(Mutex::new(HandleInner { state, held })),
            queue: Arc::new(AsyncMutex::new(())),
        })
    }
}

pub fn open_session_store(store: Arc<dyn DurableStore>, epoch: u64) -> SessionStore {
    SessionStore { store, epoch }
}

#[derive(Clone)]
pub struct SyncSessionHandle {
    store: Arc<dyn SyncCommitStore>,
    epoch: u64,
    id: String,
    inner: Arc<Mutex<HandleInner>>,
}

impl SyncSessionHandle {
    pub fn header(&self) -> SessionHeader {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .header
            .clone()
    }

    pub fn entries(&self) -> Vec<StoredEntry> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .entries
            .clone()
    }

    pub fn append_sync(&self, entries: &[StoredEntry]) -> Result<(), SessionStoreError> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let plan = plan_append(&inner.state, inner.held, entries)?;
        let mut noop = |_: &super::store::CommitResult| {};
        self.store
            .commit_sync(self.epoch, plan.writes, &mut noop)
            .map_err(|error| as_duplicate_entry(&self.id, error))?;
        inner.state = plan.next;
        inner.held = true;
        Ok(())
    }

    pub fn rewrite_sync(
        &self,
        header: SessionHeader,
        entries: Vec<StoredEntry>,
    ) -> Result<(), SessionStoreError> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let plan = plan_rewrite(
            self.store.as_ref(),
            &inner.state,
            inner.held,
            header,
            entries,
        )?;
        let mut noop = |_: &super::store::CommitResult| {};
        self.store
            .commit_sync(self.epoch, plan.writes, &mut noop)
            .map_err(|error| as_duplicate_entry(&self.id, error))?;
        inner.state = plan.next;
        inner.held = true;
        Ok(())
    }
}

#[derive(Clone)]
pub struct SyncSessionStore {
    store: Arc<dyn SyncCommitStore>,
    epoch: u64,
}

impl SyncSessionStore {
    pub async fn load(&self, id: &str) -> Result<Option<StoredSession>, SessionStoreError> {
        Ok(read_session_state(self.store.as_ref(), id)
            .await?
            .map(SessionState::into_stored))
    }

    pub async fn open(
        &self,
        id: &str,
        header: SessionHeader,
    ) -> Result<SyncSessionHandle, SessionStoreError> {
        let (state, held) = open_state(self.store.as_ref(), id, header).await?;
        Ok(SyncSessionHandle {
            store: Arc::clone(&self.store),
            epoch: self.epoch,
            id: id.to_owned(),
            inner: Arc::new(Mutex::new(HandleInner { state, held })),
        })
    }
}

pub fn open_sync_session_store(store: Arc<dyn SyncCommitStore>, epoch: u64) -> SyncSessionStore {
    SyncSessionStore { store, epoch }
}
