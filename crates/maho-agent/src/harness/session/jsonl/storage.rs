//! Port of senpi packages/agent/src/harness/session/jsonl/storage.ts.

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ai::types::BoxFuture;

use crate::harness::context::Context;
use crate::harness::session::in_memory_storage_state::InMemoryStorageState;
use crate::harness::session::session::{SessionError, SessionErrorKind};
use crate::harness::session::types::{
    CommitResult, Entry, EntryScan, EntryStructure, NewUsageRow, SessionStats, Storage, StorageBranchScan, UsageRow,
    UsageScan, Write,
};
use crate::harness::session::values::{ListElement, ListReadOptions, StoredValue, Value, ValueList};
use crate::harness::types::FileSystem;

use super::io::{file_value, parse_jsonl_transaction, publish_file_atomically, publish_jsonl, read_jsonl_header, serialize_jsonl_transaction};
use super::types::{JSONL_STORAGE_VERSION, JsonlStorageHeader, JsonlStorageOptions};

fn split_complete_lines(content: &str) -> (Vec<String>, bool) {
    if let Some(body) = content.strip_suffix('\n') {
        return (body.split('\n').map(str::to_owned).collect(), false);
    }
    match content.rfind('\n') {
        None => (Vec::new(), true),
        Some(last_newline) => (
            content[..last_newline].split('\n').map(str::to_owned).collect(),
            true,
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonlBacking {
    V4,
    V3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageLifecycle {
    Open,
    Closing,
    Closed,
}

/// JSONL storage backed by an injected filesystem capability.
pub struct JsonlStorage {
    file_system: Arc<dyn FileSystem>,
    path: String,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    header: JsonlStorageHeader,
    backing: std::sync::Mutex<JsonlBacking>,
    state: std::sync::Mutex<InMemoryStorageState>,
    lifecycle: std::sync::Mutex<StorageLifecycle>,
    close_lock: tokio::sync::Mutex<()>,
}

impl JsonlStorage {
    fn now(&self) -> i64 {
        (self.now)()
    }

    fn is_open(&self) -> bool {
        *self.lifecycle.lock().expect("jsonl lifecycle") == StorageLifecycle::Open
    }

    fn closed_error() -> SessionError {
        SessionError::new(SessionErrorKind::Closed, "JsonlStorage is closed")
    }

    pub fn header(&self) -> &JsonlStorageHeader {
        &self.header
    }

    pub fn is_legacy_v3(&self) -> bool {
        *self.backing.lock().expect("jsonl backing") == JsonlBacking::V3
    }

    pub async fn create(
        options: JsonlStorageOptions,
        header: JsonlStorageHeader,
        initial_writes: Vec<Write>,
        context: &Context,
    ) -> Result<JsonlStorage, SessionError> {
        let storage = JsonlStorage {
            file_system: options.file_system.clone(),
            path: options.path.clone(),
            now: options.now.clone().unwrap_or_else(default_now),
            header: header.clone(),
            backing: std::sync::Mutex::new(JsonlBacking::V4),
            state: std::sync::Mutex::new(InMemoryStorageState::new()),
            lifecycle: std::sync::Mutex::new(StorageLifecycle::Open),
            close_lock: tokio::sync::Mutex::new(()),
        };
        let prepared = {
            let state = storage.state.lock().expect("jsonl state");
            state.prepare_commit(initial_writes, storage.now())?
        };
        let writes = prepared.writes.clone();
        publish_jsonl(
            options.file_system.clone(),
            options.path.clone(),
            &header,
            context,
            Arc::new(move |append| {
                let writes = writes.clone();
                Box::pin(async move {
                    if !writes.is_empty() {
                        append(writes).await?;
                    }
                    Ok(())
                })
            }),
        )
        .await?;
        storage
            .state
            .lock()
            .expect("jsonl state")
            .apply_validated(&prepared.writes);
        Ok(storage)
    }

    pub async fn open(options: JsonlStorageOptions, context: &Context) -> Result<JsonlStorage, SessionError> {
        let reader = file_value(
            options
                .file_system
                .open_text_line_reader(&options.path, context)
                .await,
            &format!("Failed to read JSONL storage {}", options.path),
        )?;
        let parsed = read_jsonl_header(reader.as_ref(), &options.path, context).await;
        reader.close(context).await;
        let parsed = parsed?;
        match parsed {
            super::codec::JsonlParsedSessionHeader::V4(header) => Self::open_v4(options, header, context).await,
            super::codec::JsonlParsedSessionHeader::V3Legacy(_) => Err(SessionError::new(
                SessionErrorKind::Io,
                format!(
                    "Invalid JSONL storage {}: legacy v3 upgrade is not ported",
                    options.path
                ),
            )),
        }
    }

    async fn open_v4(
        options: JsonlStorageOptions,
        header: JsonlStorageHeader,
        context: &Context,
    ) -> Result<JsonlStorage, SessionError> {
        let content = file_value(
            options.file_system.read_text_file(&options.path, context).await,
            &format!("Failed to read JSONL storage {}", options.path),
        )?;
        let (lines, torn) = split_complete_lines(&content);
        if header.storage_version != JSONL_STORAGE_VERSION {
            return Err(SessionError::new(
                SessionErrorKind::Io,
                format!(
                    "Session {} uses unsupported storage version {}",
                    header.id, header.storage_version
                ),
            ));
        }
        let storage = JsonlStorage {
            file_system: options.file_system.clone(),
            path: options.path.clone(),
            now: options.now.clone().unwrap_or_else(default_now),
            header: header.clone(),
            backing: std::sync::Mutex::new(JsonlBacking::V4),
            state: std::sync::Mutex::new(InMemoryStorageState::new()),
            lifecycle: std::sync::Mutex::new(StorageLifecycle::Open),
            close_lock: tokio::sync::Mutex::new(()),
        };
        for (index, line) in lines.iter().enumerate().skip(1) {
            let writes = parse_jsonl_transaction(line).map_err(|_| {
                SessionError::new(
                    SessionErrorKind::Io,
                    format!("Invalid JSONL storage {}: line {}", options.path, index + 1),
                )
            })?;
            storage.replay_committed(&writes)?;
        }
        if let Some(next_seq) = header.next_seq {
            storage
                .state
                .lock()
                .expect("jsonl state")
                .advance_next_seq(next_seq)?;
        }
        if torn {
            let content = format!("{}\n", lines.join("\n"));
            let file_system = options.file_system.clone();
            let path = options.path.clone();
            publish_file_atomically(
                file_system,
                path,
                context,
                Arc::new(move |append| {
                    let content = content.clone();
                    Box::pin(async move { append(content).await })
                }),
            )
            .await?;
        }
        Ok(storage)
    }

    fn replay_committed(&self, writes: &[crate::harness::session::commit::CommittedWrite]) -> Result<(), SessionError> {
        let mut state = self.state.lock().expect("jsonl state");
        state.validate_committed(writes)?;
        state.apply_validated(writes);
        Ok(())
    }

    async fn apply_commit(&self, writes: Vec<Write>, context: &Context) -> Result<CommitResult, SessionError> {
        if self.is_legacy_v3() && !writes.is_empty() {
            return Err(SessionError::new(
                SessionErrorKind::Io,
                "Cannot commit to a legacy v3 JSONL storage: the v3 upgrade is not ported",
            ));
        }
        let prepared = {
            let state = self.state.lock().expect("jsonl state");
            state.prepare_commit(writes, self.now())?
        };
        if !prepared.writes.is_empty() {
            let line = format!("{}\n", serialize_jsonl_transaction(&prepared.writes));
            file_value(
                self.file_system.append_file(&self.path, line.as_bytes(), context).await,
                &format!("Failed to append JSONL storage {}", self.path),
            )?;
        }
        let stats = self
            .state
            .lock()
            .expect("jsonl state")
            .apply_validated(&prepared.writes);
        Ok(CommitResult {
            first_seq: prepared.result.first_seq,
            seqs: prepared.result.seqs,
            timestamp: prepared.result.timestamp,
            stats,
        })
    }

    /// Capture the first sequence a later source commit would use.
    pub async fn capture_fork_next_seq(&self, _context: &Context) -> Result<i64, SessionError> {
        if !self.is_open() {
            return Err(Self::closed_error());
        }
        Ok(self.state.lock().expect("jsonl state").get_next_seq())
    }
}

fn default_now() -> Arc<dyn Fn() -> i64 + Send + Sync> {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default()
    })
}

impl Storage for JsonlStorage {
    fn commit<'a>(&'a self, writes: Vec<Write>, context: &'a Context) -> BoxFuture<'a, Result<CommitResult, SessionError>> {
        Box::pin(async move {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            self.apply_commit(writes, context).await
        })
    }

    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Entry>, SessionError>> {
        Box::pin(async move {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            Ok(self.state.lock().expect("jsonl state").get_entries(&ids))
        })
    }

    fn get_value<'a>(
        &'a self,
        address: &'a Value,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<StoredValue>, SessionError>> {
        Box::pin(async move {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            Ok(self.state.lock().expect("jsonl state").get_value(address))
        })
    }

    fn scan_values<'a>(
        &'a self,
        prefix: &'a Value,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, SessionError>> {
        Box::pin(async move {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            Ok(self.state.lock().expect("jsonl state").scan_values(prefix))
        })
    }

    fn read_list<'a>(
        &'a self,
        address: &'a ValueList,
        options: Option<ListReadOptions>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<ListElement>, SessionError>> {
        Box::pin(async move {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            self.state.lock().expect("jsonl state").read_list(address, options)
        })
    }

    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        Box::pin(async move {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            self.state.lock().expect("jsonl state").scan_branch(&query)
        })
    }

    fn scan_branch_structure<'a>(
        &'a self,
        query: StorageBranchScan,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<EntryStructure>, SessionError>> {
        Box::pin(async move {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            self.state.lock().expect("jsonl state").scan_branch_structure(&query)
        })
    }

    fn scan_entries<'a>(
        &'a self,
        query: EntryScan,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        Box::pin(async move {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            Ok(self.state.lock().expect("jsonl state").scan_entries(&query))
        })
    }

    fn scan_usage<'a>(
        &'a self,
        query: UsageScan,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<UsageRow>, SessionError>> {
        Box::pin(async move {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            Ok(self.state.lock().expect("jsonl state").scan_usage(&query))
        })
    }

    fn get_stats<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, Result<SessionStats, SessionError>> {
        Box::pin(async move {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            Ok(self.state.lock().expect("jsonl state").get_stats())
        })
    }

    fn close<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let _guard = self.close_lock.lock().await;
            if *self.lifecycle.lock().expect("jsonl lifecycle") == StorageLifecycle::Closed {
                return;
            }
            *self.lifecycle.lock().expect("jsonl lifecycle") = StorageLifecycle::Closing;
            *self.lifecycle.lock().expect("jsonl lifecycle") = StorageLifecycle::Closed;
        })
    }
}

pub type JsonlStorageHandle = Arc<JsonlStorage>;

pub type JsonlUsageRow = UsageRow;

pub type JsonlNewUsageRow = NewUsageRow;

