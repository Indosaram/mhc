//! Port of senpi packages/agent/src/harness/session/session.ts.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use maho_ai::types::BoxFuture;
use tokio::sync::Semaphore;

use crate::types::AgentMessage;

use super::commit::insert_entry;
use super::mutation_line::MutationLine;
use super::types::{
    Branch, BranchScan, CommitResult, Entry, EntryQuery, IdGenerator, JsonValue, NewEntry, Session,
    SessionCreateOptions, SessionMetadata, SessionMutation, SessionMutationCallback, SessionReader, SessionStats,
    Storage, StorageBranchScan, Write,
};
use super::values::{
    ListElement, ListReadOptions, StoredValue, Value, ValueList,
    append_list as append_list_write, branch_tip, delete_list as delete_list_write,
    delete_value as delete_value_write, entry_label, session_name, set_value as set_value_write,
};

pub type SessionResult<T> = Result<T, SessionError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionErrorKind {
    Invariant,
    InvalidBranch,
    BranchExists,
    PendingAssistantMessage,
    UnknownTarget,
    Closed,
    MutatorInactive,
    MutatorCommitAttempted,
    NonMonotonicSequence,
    DuplicateId,
    MissingParent,
    Io,
    CommitDiscarded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError {
    pub kind: SessionErrorKind,
    pub message: String,
    pub branch: Option<String>,
    pub reason: Option<String>,
    pub target_id: Option<String>,
}

impl SessionError {
    pub fn new(kind: SessionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            branch: None,
            reason: None,
            target_id: None,
        }
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(SessionErrorKind::Io, message)
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SessionError {}

fn quote(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
}

pub fn session_invariant_error(message: impl Into<String>) -> SessionError {
    SessionError::new(SessionErrorKind::Invariant, message)
}

pub fn session_invalid_branch_error(branch: &str, reason: &str) -> SessionError {
    let mut error = SessionError::new(
        SessionErrorKind::InvalidBranch,
        format!("Invalid branch {}: {reason}", quote(branch)),
    );
    error.branch = Some(branch.to_owned());
    error.reason = Some(reason.to_owned());
    error
}

pub fn session_branch_exists_error(branch: &str) -> SessionError {
    let mut error = SessionError::new(SessionErrorKind::BranchExists, format!("Branch already exists: {branch}"));
    error.branch = Some(branch.to_owned());
    error
}

pub fn session_pending_assistant_message_error() -> SessionError {
    SessionError::new(
        SessionErrorKind::PendingAssistantMessage,
        "Cannot persist a pending assistant message",
    )
}

pub fn session_unknown_target_error(target_id: &str) -> SessionError {
    let mut error = SessionError::new(SessionErrorKind::UnknownTarget, format!("Unknown target: {target_id}"));
    error.target_id = Some(target_id.to_owned());
    error
}

pub type SessionInvariantError = SessionError;
pub type SessionInvalidBranchError = SessionError;
pub type SessionBranchExistsError = SessionError;
pub type SessionPendingAssistantMessageError = SessionError;
pub type SessionUnknownTargetError = SessionError;

#[derive(Clone, Default)]
pub struct StorageBackedSessionOptions {
    pub mutation_line: Option<MutationLine>,
    pub id_generator: Option<IdGenerator>,
    pub on_close: Option<Arc<dyn Fn() + Send + Sync>>,
}

pub fn default_id_generator() -> IdGenerator {
    Arc::new(|timestamp_ms: Option<i64>| {
        maho_ai::utils::uuid::uuidv7(timestamp_ms.map(|t| t as f64)).unwrap_or_else(|_| uuid_like_fallback(timestamp_ms))
    })
}

fn uuid_like_fallback(timestamp_ms: Option<i64>) -> String {
    let stamp = timestamp_ms.unwrap_or(0).max(0) as u64;
    format!("{stamp:012x}-0000-7000-8000-000000000000")
}

fn is_pending_assistant(message: &AgentMessage) -> bool {
    match message.try_as_llm() {
        Some(maho_ai::types::Message::Assistant(assistant)) => {
            assistant.stop_reason == maho_ai::types::StopReason::Pending
        }
        _ => false,
    }
}

/// Wakeups are counted permits, not notifications: the TS design resolves promises that store
/// their settlement, so a signal raised before the waiter parks must not be lost.
struct MutationState {
    active: AtomicBool,
    commit_started: AtomicBool,
    commit_done: AtomicBool,
    settled: Arc<Semaphore>,
    release: Arc<Semaphore>,
    end_started: AtomicBool,
    end_done: AtomicBool,
    end_settled: Arc<Semaphore>,
}

pub struct StorageBackedSessionMutation {
    storage: Arc<dyn Storage>,
    state: Arc<MutationState>,
}

impl StorageBackedSessionMutation {
    fn new(storage: Arc<dyn Storage>, release: Arc<Semaphore>) -> Self {
        Self {
            storage,
            state: Arc::new(MutationState {
                active: AtomicBool::new(true),
                commit_started: AtomicBool::new(false),
                commit_done: AtomicBool::new(false),
                settled: Arc::new(Semaphore::new(0)),
                release,
                end_started: AtomicBool::new(false),
                end_done: AtomicBool::new(false),
                end_settled: Arc::new(Semaphore::new(0)),
            }),
        }
    }

    fn assert_active(&self) -> SessionResult<()> {
        if self.state.active.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(SessionError::new(
                SessionErrorKind::MutatorInactive,
                "SessionMutator cannot be used outside its mutation callback",
            ))
        }
    }

    async fn settle(&self) {
        while self.state.commit_started.load(Ordering::SeqCst) && !self.state.commit_done.load(Ordering::SeqCst) {
            if self.state.settled.acquire().await.is_err() {
                return;
            }
        }
    }
}

impl SessionReader for StorageBackedSessionMutation {
    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<std::collections::BTreeMap<String, Entry>>> {
        Box::pin(async move {
            self.assert_active()?;
            self.storage.get_entries(ids, context).await
        })
    }

    fn get_stats<'a>(
        &'a self,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<SessionStats>> {
        Box::pin(async move {
            self.assert_active()?;
            self.storage.get_stats(context).await
        })
    }

    fn get_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Option<StoredValue>>> {
        Box::pin(async move {
            self.assert_active()?;
            self.storage.get_value(address, context).await
        })
    }

    fn scan_values<'a>(
        &'a self,
        prefix: &'a Value,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Vec<StoredValue>>> {
        Box::pin(async move {
            self.assert_active()?;
            self.storage.scan_values(prefix, context).await
        })
    }

    fn read_list<'a>(
        &'a self,
        address: &'a ValueList,
        options: Option<ListReadOptions>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Vec<ListElement>>> {
        Box::pin(async move {
            self.assert_active()?;
            self.storage.read_list(address, options, context).await
        })
    }

    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Vec<Entry>>> {
        Box::pin(async move {
            self.assert_active()?;
            self.storage.scan_branch(query, context).await
        })
    }
}

impl SessionMutation for StorageBackedSessionMutation {
    fn commit<'a>(
        &'a self,
        writes: Vec<Write>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<CommitResult>> {
        Box::pin(async move {
            self.assert_active()?;
            if self.state.commit_started.swap(true, Ordering::SeqCst) {
                return Err(SessionError::new(
                    SessionErrorKind::MutatorCommitAttempted,
                    "SessionMutator commit already attempted",
                ));
            }
            for write in &writes {
                if let Write::Entry(entry_write) = write
                    && let super::types::EntryKind::Message { message, .. } = &entry_write.entry.kind
                        && is_pending_assistant(message) {
                            self.state.commit_done.store(true, Ordering::SeqCst);
                            self.state.settled.add_permits(1);
                            return Err(session_pending_assistant_message_error());
                        }
            }
            let result = self.storage.commit(writes, context).await;
            self.state.commit_done.store(true, Ordering::SeqCst);
            self.state.settled.add_permits(1);
            result
        })
    }

    fn end<'a>(&'a self, _context: &'a crate::harness::context::Context) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if self.state.end_started.swap(true, Ordering::SeqCst) {
                while !self.state.end_done.load(Ordering::SeqCst) {
                    if self.state.end_settled.acquire().await.is_err() {
                        return;
                    }
                }
                return;
            }
            self.state.active.store(false, Ordering::SeqCst);
            self.settle().await;
            self.state.release.add_permits(1);
            self.state.end_done.store(true, Ordering::SeqCst);
            self.state.end_settled.add_permits(1);
        })
    }
}

struct BranchState {
    name: String,
    session: Weak<StorageBackedSession>,
}

pub struct StorageBackedBranch {
    state: BranchState,
}

impl StorageBackedBranch {
    fn session(&self) -> SessionResult<Arc<StorageBackedSession>> {
        self.state
            .session
            .upgrade()
            .ok_or_else(|| SessionError::new(SessionErrorKind::Closed, "Session is closed"))
    }
}

impl Branch for StorageBackedBranch {
    fn name(&self) -> &str {
        &self.state.name
    }

    fn get_tip_id<'a>(
        &'a self,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Option<String>>> {
        Box::pin(async move {
            let session = self.session()?;
            session.get_branch_tip(&self.state.name, context).await
        })
    }

    fn find_entries<'a>(
        &'a self,
        query: Option<BranchScan>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Vec<Entry>>> {
        Box::pin(async move {
            let session = self.session()?;
            let query = query.unwrap_or_default();
            let start = match &query.start {
                Some(start) => Some(start.clone()),
                None => session.get_branch_tip(&self.state.name, context).await?,
            };
            let Some(start) = start else {
                return Ok(Vec::new());
            };
            session
                .scan_branch(
                    StorageBranchScan {
                        start,
                        stop_at_type: query.stop_at_type,
                        stop_at_id: query.stop_at_id,
                        entry_type: query.entry_type,
                        custom_type: query.custom_type,
                        order: Some(query.order.unwrap_or(super::types::BranchOrder::NewestFirst)),
                        limit: query.limit,
                        cursor: query.cursor,
                    },
                    context,
                )
                .await
        })
    }

    fn find_entry<'a>(
        &'a self,
        query: Option<BranchScan>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Option<Entry>>> {
        Box::pin(async move {
            let query = query.unwrap_or_default();
            let limit = query.limit.map_or(1, |limit| limit.min(1));
            let mut entries = self
                .find_entries(
                    Some(BranchScan {
                        limit: Some(limit),
                        ..query
                    }),
                    context,
                )
                .await?;
            Ok(entries.drain(..).next())
        })
    }

    fn append_message<'a>(
        &'a self,
        message: AgentMessage,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<String>> {
        Box::pin(async move {
            let session = self.session()?;
            session.append_to_branch(&self.state.name, NewEntry::message(String::new(), None, message), context).await
        })
    }

    fn append_custom_entry<'a>(
        &'a self,
        custom_type: String,
        data: Option<JsonValue>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<String>> {
        Box::pin(async move {
            let session = self.session()?;
            let entry = match data {
                Some(data) => NewEntry::custom_with_data(String::new(), None, custom_type, data),
                None => NewEntry::custom(String::new(), None, custom_type),
            };
            session.append_to_branch(&self.state.name, entry, context).await
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionLifecycle {
    Open,
    Closing,
    Closed,
}

pub struct StorageBackedSession {
    metadata: SessionMetadata,
    id_generator: IdGenerator,
    storage: Arc<dyn Storage>,
    mutation_line: MutationLine,
    on_close: Option<Arc<dyn Fn() + Send + Sync>>,
    branches: Mutex<HashMap<String, Arc<StorageBackedBranch>>>,
    state: Mutex<SessionLifecycle>,
    close_lock: tokio::sync::Mutex<()>,
    self_ref: Mutex<Weak<StorageBackedSession>>,
}

impl StorageBackedSession {
    pub fn new(metadata: SessionMetadata, storage: Arc<dyn Storage>, options: StorageBackedSessionOptions) -> Self {
        Self {
            metadata,
            id_generator: options.id_generator.unwrap_or_else(default_id_generator),
            storage,
            mutation_line: options.mutation_line.unwrap_or_default(),
            on_close: options.on_close,
            branches: Mutex::new(HashMap::new()),
            state: Mutex::new(SessionLifecycle::Open),
            close_lock: tokio::sync::Mutex::new(()),
            self_ref: Mutex::new(Weak::new()),
        }
    }

    /// Register the session's own weak handle so Branch objects can reach it.
    pub fn attach(self: &Arc<Self>) {
        *self.self_ref.lock().expect("session self_ref") = Arc::downgrade(self);
    }

    fn lifecycle(&self) -> SessionLifecycle {
        *self.state.lock().expect("session state")
    }

    fn closed_error() -> SessionError {
        SessionError::new(SessionErrorKind::Closed, "Session is closed")
    }

    fn assert_open(&self) -> SessionResult<()> {
        if self.lifecycle() == SessionLifecycle::Open {
            Ok(())
        } else {
            Err(Self::closed_error())
        }
    }

    pub async fn get_branch_tip(
        &self,
        name: &str,
        context: &crate::harness::context::Context,
    ) -> SessionResult<Option<String>> {
        let stored = self.get_value(&branch_tip(name), context).await?;
        match stored {
            Some(stored) => Ok(serde_json::from_value(stored.value).unwrap_or(None)),
            None => Err(session_invariant_error(format!("Unknown branch: {name}"))),
        }
    }

    pub async fn append_to_branch(
        &self,
        name: &str,
        entry: NewEntry,
        context: &crate::harness::context::Context,
    ) -> SessionResult<String> {
        self.assert_open()?;
        if let super::types::EntryKind::Message { message, .. } = &entry.kind
            && is_pending_assistant(message) {
                return Err(session_pending_assistant_message_error());
            }
        let id = (self.id_generator)(None);
        let mut entry = entry;
        entry.id = id.clone();
        let committed_id = id.clone();
        self.mutate(
            Arc::new({
                let name = name.to_owned();
                move |mutator, context| {
                    let name = name.clone();
                    let entry = entry.clone();
                    let id = committed_id.clone();
                    Box::pin(async move {
                        let tip = mutator.get_value(&branch_tip(&name), context).await?;
                        let Some(tip) = tip else {
                            return Err(session_invariant_error(format!("Unknown branch: {name}")));
                        };
                        let parent_id = serde_json::from_value::<Option<String>>(tip.value).unwrap_or(None);
                        let mut entry = entry;
                        entry.parent_id = parent_id;
                        mutator
                            .commit(
                                vec![
                                    insert_entry(entry),
                                    Write::Value(set_value_write(&branch_tip(&name), JsonValue::String(id.clone()))),
                                ],
                                context,
                            )
                            .await?;
                        Ok(JsonValue::String(id))
                    })
                }
            }),
            context,
        )
        .await?;
        Ok(id)
    }

    fn get_or_create_branch_object(self: &Arc<Self>, name: &str) -> Arc<StorageBackedBranch> {
        let mut branches = self.branches.lock().expect("session branches");
        branches
            .entry(name.to_owned())
            .or_insert_with(|| {
                Arc::new(StorageBackedBranch {
                    state: BranchState {
                        name: name.to_owned(),
                        session: Arc::downgrade(self),
                    },
                })
            })
            .clone()
    }

    fn assert_valid_branch_name(name: &str) -> SessionResult<()> {
        if name.is_empty() {
            return Err(session_invalid_branch_error(name, "branch name must not be empty"));
        }
        if name.contains(super::values::NUL) {
            return Err(session_invalid_branch_error(
                name,
                "branch name must not contain \\u0000",
            ));
        }
        Ok(())
    }

    fn self_handle(&self) -> SessionResult<Arc<StorageBackedSession>> {
        self.self_ref
            .lock()
            .expect("session self_ref")
            .upgrade()
            .ok_or_else(Self::closed_error)
    }
}

impl SessionReader for StorageBackedSession {
    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<std::collections::BTreeMap<String, Entry>>> {
        Box::pin(async move {
            self.assert_open()?;
            self.storage.get_entries(ids, context).await
        })
    }

    fn get_stats<'a>(
        &'a self,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<SessionStats>> {
        Box::pin(async move {
            self.assert_open()?;
            self.storage.get_stats(context).await
        })
    }

    fn get_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Option<StoredValue>>> {
        Box::pin(async move {
            self.assert_open()?;
            self.storage.get_value(address, context).await
        })
    }

    fn scan_values<'a>(
        &'a self,
        prefix: &'a Value,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Vec<StoredValue>>> {
        Box::pin(async move {
            self.assert_open()?;
            self.storage.scan_values(prefix, context).await
        })
    }

    fn read_list<'a>(
        &'a self,
        address: &'a ValueList,
        options: Option<ListReadOptions>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Vec<ListElement>>> {
        Box::pin(async move {
            self.assert_open()?;
            self.storage.read_list(address, options, context).await
        })
    }

    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Vec<Entry>>> {
        Box::pin(async move {
            self.assert_open()?;
            self.storage.scan_branch(query, context).await
        })
    }
}

impl Session for StorageBackedSession {
    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn id_generator(&self) -> &IdGenerator {
        &self.id_generator
    }

    fn get_entry<'a>(
        &'a self,
        id: &'a str,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Option<Entry>>> {
        Box::pin(async move { Ok(self.get_entries(vec![id.to_owned()], context).await?.remove(id)) })
    }

    fn get_name<'a>(
        &'a self,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Option<String>>> {
        Box::pin(async move {
            Ok(self
                .get_value(&session_name(), context)
                .await?
                .and_then(|stored| serde_json::from_value(stored.value).unwrap_or(None)))
        })
    }

    fn get_label<'a>(
        &'a self,
        target_id: &'a str,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Option<String>>> {
        Box::pin(async move {
            Ok(self
                .get_value(&entry_label(target_id), context)
                .await?
                .and_then(|stored| serde_json::from_value(stored.value).unwrap_or(None)))
        })
    }

    fn find_entries<'a>(
        &'a self,
        query: Option<EntryQuery>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Vec<Entry>>> {
        Box::pin(async move {
            self.assert_open()?;
            let query = query.unwrap_or_default();
            let order = query.order.unwrap_or(super::types::ScanOrder::Desc);
            if let Some(cursor) = query.cursor {
                if order == super::types::ScanOrder::Asc && cursor.seq == MAX_SAFE_INTEGER {
                    return Ok(Vec::new());
                }
                if order == super::types::ScanOrder::Desc && cursor.seq <= 1 {
                    return Ok(Vec::new());
                }
            }
            let (from_seq, to_seq) = match query.cursor {
                None => (None, None),
                Some(cursor) => {
                    if order == super::types::ScanOrder::Asc {
                        (Some(cursor.seq + 1), None)
                    } else {
                        (None, Some(cursor.seq - 1))
                    }
                }
            };
            self.storage
                .scan_entries(
                    super::types::EntryScan {
                        entry_type: query.entry_type,
                        custom_type: query.custom_type,
                        from_seq,
                        to_seq,
                        order: Some(order),
                        limit: query.limit,
                    },
                    context,
                )
                .await
        })
    }

    fn find_entry<'a>(
        &'a self,
        query: Option<EntryQuery>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Option<Entry>>> {
        Box::pin(async move {
            let query = query.unwrap_or_default();
            let limit = query.limit.map_or(1, |limit| limit.min(1));
            let mut entries = self
                .find_entries(
                    Some(EntryQuery {
                        limit: Some(limit),
                        ..query
                    }),
                    context,
                )
                .await?;
            Ok(entries.drain(..).next())
        })
    }

    fn branch<'a>(
        &'a self,
        name: &'a str,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Option<Box<dyn Branch>>>> {
        Box::pin(async move {
            Self::assert_valid_branch_name(name)?;
            let session = self.self_handle()?;
            if self.get_value(&branch_tip(name), context).await?.is_none() {
                return Ok(None);
            }
            Ok(Some(
                Box::new(BranchHandle(session.get_or_create_branch_object(name))) as Box<dyn Branch>
            ))
        })
    }

    fn create_branch<'a>(
        &'a self,
        name: &'a str,
        at: Option<String>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Box<dyn Branch>>> {
        Box::pin(async move {
            self.assert_open()?;
            Self::assert_valid_branch_name(name)?;
            let session = self.self_handle()?;
            let name_owned = name.to_owned();
            self.mutate(
                Arc::new(move |mutator, context| {
                    let name_owned = name_owned.clone();
                    let at = at.clone();
                    Box::pin(async move {
                        if mutator.get_value(&branch_tip(&name_owned), context).await?.is_some() {
                            return Err(session_branch_exists_error(&name_owned));
                        }
                        if let Some(at) = &at
                            && !mutator
                                .get_entries(vec![at.clone()], context)
                                .await?
                                .contains_key(at)
                            {
                                return Err(session_unknown_target_error(at));
                            }
                        mutator
                            .commit(
                                vec![Write::Value(set_value_write(
                                    &branch_tip(&name_owned),
                                    match at {
                                        Some(at) => JsonValue::String(at),
                                        None => JsonValue::Null,
                                    },
                                ))],
                                context,
                            )
                            .await?;
                        Ok(JsonValue::Null)
                    })
                }),
                context,
            )
            .await?;
            Ok(Box::new(BranchHandle(session.get_or_create_branch_object(name))) as Box<dyn Branch>)
        })
    }

    fn begin_mutation<'a>(
        &'a self,
        _context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Box<dyn SessionMutation>>> {
        Box::pin(async move {
            self.assert_open()?;
            let (sender, receiver) = tokio::sync::oneshot::channel::<SessionResult<Box<dyn SessionMutation>>>();
            let mut sender = Some(sender);
            let line = self.mutation_line.clone();
            let storage = self.storage.clone();
            tokio::spawn(async move {
                let mut granted = false;
                let result = line
                    .run(|| async {
                        let release = Arc::new(Semaphore::new(0));
                        let mutation = StorageBackedSessionMutation::new(storage.clone(), release.clone());
                        if let Some(sender) = sender.take() {
                            granted = true;
                            let _ = sender.send(Ok(Box::new(mutation) as Box<dyn SessionMutation>));
                        }
                        if let Ok(permit) = release.acquire().await {
                            permit.forget();
                        }
                        Ok(())
                    })
                    .await;
                if let Err(error) = result
                    && !granted
                        && let Some(sender) = sender.take() {
                            let _ = sender.send(Err(error));
                        }
            });
            match receiver.await {
                Ok(result) => result,
                Err(_) => Err(Self::closed_error()),
            }
        })
    }

    fn mutate<'a>(
        &'a self,
        mutation: SessionMutationCallback,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<JsonValue>> {
        Box::pin(async move {
            let mutator = self.begin_mutation(context).await?;
            let outcome = mutation(mutator.as_ref(), context).await;
            mutator.end(context).await;
            outcome
        })
    }

    fn set_value<'a>(
        &'a self,
        address: &'a Value,
        next: JsonValue,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<()>> {
        Box::pin(async move {
            let address = address.clone();
            self.mutate(
                Arc::new(move |mutator, context| {
                    let address = address.clone();
                    let next = next.clone();
                    Box::pin(async move {
                        mutator
                            .commit(vec![Write::Value(set_value_write(&address, next))], context)
                            .await?;
                        Ok(JsonValue::Null)
                    })
                }),
                context,
            )
            .await
            .map(|_| ())
        })
    }

    fn delete_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<()>> {
        Box::pin(async move {
            let address = address.clone();
            self.mutate(
                Arc::new(move |mutator, context| {
                    let address = address.clone();
                    Box::pin(async move {
                        mutator
                            .commit(vec![Write::Value(delete_value_write(&address))], context)
                            .await?;
                        Ok(JsonValue::Null)
                    })
                }),
                context,
            )
            .await
            .map(|_| ())
        })
    }

    fn append_list<'a>(
        &'a self,
        address: &'a ValueList,
        element: JsonValue,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<()>> {
        Box::pin(async move {
            let address = address.clone();
            self.mutate(
                Arc::new(move |mutator, context| {
                    let address = address.clone();
                    let element = element.clone();
                    Box::pin(async move {
                        mutator
                            .commit(vec![Write::List(append_list_write(&address, element))], context)
                            .await?;
                        Ok(JsonValue::Null)
                    })
                }),
                context,
            )
            .await
            .map(|_| ())
        })
    }

    fn delete_list<'a>(
        &'a self,
        address: &'a ValueList,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<()>> {
        Box::pin(async move {
            let address = address.clone();
            self.mutate(
                Arc::new(move |mutator, context| {
                    let address = address.clone();
                    Box::pin(async move {
                        mutator
                            .commit(vec![Write::List(delete_list_write(&address))], context)
                            .await?;
                        Ok(JsonValue::Null)
                    })
                }),
                context,
            )
            .await
            .map(|_| ())
        })
    }

    fn set_name<'a>(
        &'a self,
        name: Option<String>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<()>> {
        Box::pin(async move {
            match name {
                None => self.delete_value(&session_name(), context).await,
                Some(name) => self.set_value(&session_name(), JsonValue::String(name), context).await,
            }
        })
    }

    fn set_label<'a>(
        &'a self,
        target_id: &'a str,
        label: Option<String>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<()>> {
        Box::pin(async move {
            let address = entry_label(target_id);
            match label {
                None => self.delete_value(&address, context).await,
                Some(label) => self.set_value(&address, JsonValue::String(label), context).await,
            }
        })
    }

    fn close<'a>(&'a self, context: &'a crate::harness::context::Context) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let _guard = self.close_lock.lock().await;
            if self.lifecycle() == SessionLifecycle::Closed {
                return;
            }
            *self.state.lock().expect("session state") = SessionLifecycle::Closing;
            self.mutation_line.seal(Self::closed_error()).await;
            self.storage.close(context).await;
            *self.state.lock().expect("session state") = SessionLifecycle::Closed;
            if let Some(on_close) = &self.on_close {
                on_close();
            }
        })
    }
}

/// Branch handle that keeps the concrete session alive for the branch's lifetime.
pub struct BranchHandle(pub Arc<StorageBackedBranch>);

impl Branch for BranchHandle {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn get_tip_id<'a>(
        &'a self,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Option<String>>> {
        self.0.get_tip_id(context)
    }

    fn find_entries<'a>(
        &'a self,
        query: Option<BranchScan>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Vec<Entry>>> {
        self.0.find_entries(query, context)
    }

    fn find_entry<'a>(
        &'a self,
        query: Option<BranchScan>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<Option<Entry>>> {
        self.0.find_entry(query, context)
    }

    fn append_message<'a>(
        &'a self,
        message: AgentMessage,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<String>> {
        self.0.append_message(message, context)
    }

    fn append_custom_entry<'a>(
        &'a self,
        custom_type: String,
        data: Option<JsonValue>,
        context: &'a crate::harness::context::Context,
    ) -> BoxFuture<'a, SessionResult<String>> {
        self.0.append_custom_entry(custom_type, data, context)
    }
}

pub use super::types::{Session as _Session, SessionReader as _SessionReader};

pub type SessionBox = Box<dyn Session>;

pub type SessionCreateOptionsAlias = SessionCreateOptions;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
