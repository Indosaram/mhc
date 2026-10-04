//! Port of senpi packages/agent/src/harness/session/memory.ts.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use maho_ai::types::BoxFuture;
use tokio::sync::Notify;

use crate::harness::context::Context;

use super::in_memory_storage_state::InMemoryStorageState;
use super::session::{
    SessionError, SessionErrorKind, StorageBackedSession, StorageBackedSessionOptions,
    session_invariant_error,
};
use super::types::{
    Branch, BranchScan, CommitResult, Entry, EntryQuery, EntryScan, EntryStructure, ForkOptions, IdGenerator,
    Session, SessionCreateOptions, SessionMetadata, SessionMutation, SessionMutationCallback, SessionReader,
    SessionRepo, SessionStats, Storage, StorageBranchScan, UsageRow, UsageScan, Write,
};
use super::values::{ListElement, ListReadOptions, StoredValue, Value, ValueList};

#[derive(Clone)]
#[derive(Default)]
pub struct MemoryStorageOptions {
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}


fn default_now() -> Arc<dyn Fn() -> i64 + Send + Sync> {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default()
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageLifecycle {
    Open,
    Closing,
    Closed,
}

/// In-memory Storage backend.
pub struct MemoryStorage {
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    state: Mutex<InMemoryStorageState>,
    lifecycle: Mutex<StorageLifecycle>,
    close_lock: tokio::sync::Mutex<()>,
}

impl MemoryStorage {
    pub fn new(options: MemoryStorageOptions) -> Self {
        Self {
            now: options.now.unwrap_or_else(default_now),
            state: Mutex::new(InMemoryStorageState::new()),
            lifecycle: Mutex::new(StorageLifecycle::Open),
            close_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn closed_error() -> SessionError {
        SessionError::new(SessionErrorKind::Closed, "MemoryStorage is closed")
    }

    fn is_open(&self) -> bool {
        *self.lifecycle.lock().expect("memory lifecycle") == StorageLifecycle::Open
    }

    /// Construct a destination storage at one serialized boundary between source commits.
    pub async fn fork(&self, options: &ForkOptions) -> Result<MemoryStorage, SessionError> {
        if !self.is_open() {
            return Err(Self::closed_error());
        }
        let destination_state = {
            let state = self.state.lock().expect("memory state");
            state.create_fork(options)?
        };
        let destination = MemoryStorage::new(MemoryStorageOptions {
            now: Some(self.now.clone()),
        });
        *destination.state.lock().expect("memory state") = destination_state;
        Ok(destination)
    }
}

impl Storage for MemoryStorage {
    fn commit<'a>(&'a self, writes: Vec<Write>, _context: &'a Context) -> BoxFuture<'a, Result<CommitResult, SessionError>> {
        Box::pin(async move {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            let timestamp = (self.now)();
            let mut state = self.state.lock().expect("memory state");
            let prepared = state.prepare_commit(writes, timestamp)?;
            let stats = state.apply_validated(&prepared.writes);
            Ok(CommitResult {
                first_seq: prepared.result.first_seq,
                seqs: prepared.result.seqs,
                timestamp: prepared.result.timestamp,
                stats,
            })
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
            Ok(self.state.lock().expect("memory state").get_entries(&ids))
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
            Ok(self.state.lock().expect("memory state").get_value(address))
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
            Ok(self.state.lock().expect("memory state").scan_values(prefix))
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
            self.state.lock().expect("memory state").read_list(address, options)
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
            self.state.lock().expect("memory state").scan_branch(&query)
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
            self.state.lock().expect("memory state").scan_branch_structure(&query)
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
            Ok(self.state.lock().expect("memory state").scan_entries(&query))
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
            Ok(self.state.lock().expect("memory state").scan_usage(&query))
        })
    }

    fn get_stats<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, Result<SessionStats, SessionError>> {
        Box::pin(async move {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            Ok(self.state.lock().expect("memory state").get_stats())
        })
    }

    fn close<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let _guard = self.close_lock.lock().await;
            if *self.lifecycle.lock().expect("memory lifecycle") == StorageLifecycle::Closed {
                return;
            }
            *self.lifecycle.lock().expect("memory lifecycle") = StorageLifecycle::Closing;
            drop(self.state.lock().expect("memory state"));
            *self.lifecycle.lock().expect("memory lifecycle") = StorageLifecycle::Closed;
        })
    }
}

const MEMORY_STORAGE_VERSION: u32 = 1;

struct Admission {
    counter: Arc<AtomicUsize>,
    notify: Arc<Notify>,
}

impl Admission {
    fn finish(self) {
        if self.counter.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.notify.notify_waiters();
        }
    }
}

struct FacadeState {
    counter: Arc<AtomicUsize>,
    notify: Arc<Notify>,
}

impl FacadeState {
    fn new() -> Self {
        Self {
            counter: Arc::new(AtomicUsize::new(0)),
            notify: Arc::new(Notify::new()),
        }
    }

    fn admit(&self) -> Admission {
        self.counter.fetch_add(1, Ordering::SeqCst);
        Admission {
            counter: self.counter.clone(),
            notify: self.notify.clone(),
        }
    }

    async fn drain(&self) {
        while self.counter.load(Ordering::SeqCst) != 0 {
            let notified = self.notify.notified();
            if self.counter.load(Ordering::SeqCst) == 0 {
                break;
            }
            notified.await;
        }
    }
}

struct FacadeMutation {
    source: Box<dyn SessionMutation>,
    admission: Mutex<Option<Admission>>,
}

impl SessionReader for FacadeMutation {
    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Entry>, SessionError>> {
        self.source.get_entries(ids, context)
    }

    fn get_stats<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, Result<SessionStats, SessionError>> {
        self.source.get_stats(context)
    }

    fn get_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<StoredValue>, SessionError>> {
        self.source.get_value(address, context)
    }

    fn scan_values<'a>(
        &'a self,
        prefix: &'a Value,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, SessionError>> {
        self.source.scan_values(prefix, context)
    }

    fn read_list<'a>(
        &'a self,
        address: &'a ValueList,
        options: Option<ListReadOptions>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<ListElement>, SessionError>> {
        self.source.read_list(address, options, context)
    }

    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        self.source.scan_branch(query, context)
    }
}

impl SessionMutation for FacadeMutation {
    fn commit<'a>(
        &'a self,
        writes: Vec<Write>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<CommitResult, SessionError>> {
        self.source.commit(writes, context)
    }

    fn end<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.source.end(context).await;
            if let Some(admission) = self.admission.lock().expect("facade admission").take() {
                admission.finish();
            }
        })
    }
}

struct FacadeBranch {
    source: Box<dyn Branch>,
    facade: Arc<MemorySessionFacade>,
}

impl FacadeBranch {
    fn facade(&self) -> &MemorySessionFacade {
        &self.facade
    }
}

impl Branch for FacadeBranch {
    fn name(&self) -> &str {
        self.source.name()
    }

    fn get_tip_id<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, Result<Option<String>, SessionError>> {
        Box::pin(async move {
            let admission = self.facade().admit()?;
            let result = self.source.get_tip_id(context).await;
            admission.finish();
            result
        })
    }

    fn find_entries<'a>(
        &'a self,
        query: Option<BranchScan>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        Box::pin(async move {
            let admission = self.facade.admit()?;
            let result = self.source.find_entries(query, context).await;
            admission.finish();
            result
        })
    }

    fn find_entry<'a>(
        &'a self,
        query: Option<BranchScan>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<Entry>, SessionError>> {
        Box::pin(async move {
            let admission = self.facade.admit()?;
            let result = self.source.find_entry(query, context).await;
            admission.finish();
            result
        })
    }

    fn append_message<'a>(
        &'a self,
        message: crate::types::AgentMessage,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<String, SessionError>> {
        Box::pin(async move {
            let admission = self.facade.admit()?;
            let result = self.source.append_message(message, context).await;
            admission.finish();
            result
        })
    }

    fn append_custom_entry<'a>(
        &'a self,
        custom_type: String,
        data: Option<serde_json::Value>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<String, SessionError>> {
        Box::pin(async move {
            let admission = self.facade.admit()?;
            let result = self.source.append_custom_entry(custom_type, data, context).await;
            admission.finish();
            result
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FacadeLifecycle {
    Open,
    Closing,
    Closed,
}

/// Session facade that admits operations and closes once every admitted operation settles.
pub struct MemorySessionFacade {
    session: Arc<StorageBackedSession>,
    on_close: Arc<dyn Fn() + Send + Sync>,
    state: Mutex<FacadeLifecycle>,
    admissions: FacadeState,
    close_lock: tokio::sync::Mutex<()>,
    self_ref: Mutex<Weak<MemorySessionFacade>>,
}

impl MemorySessionFacade {
    /// Wrap a storage-backed session so every admitted operation is drained before `on_close` runs.
    pub fn new(session: Arc<StorageBackedSession>, on_close: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            session,
            on_close,
            state: Mutex::new(FacadeLifecycle::Open),
            admissions: FacadeState::new(),
            close_lock: tokio::sync::Mutex::new(()),
            self_ref: Mutex::new(Weak::new()),
        }
    }

    /// Register the facade's own weak handle so its Branch wrappers can admit through it.
    pub fn attach(self: &Arc<Self>) {
        *self.self_ref.lock().expect("facade self_ref") = Arc::downgrade(self);
    }

    fn closed_error() -> SessionError {
        SessionError::new(SessionErrorKind::Closed, "Session is closed")
    }

    fn lifecycle(&self) -> FacadeLifecycle {
        *self.state.lock().expect("facade state")
    }

    fn admit(&self) -> Result<Admission, SessionError> {
        if self.lifecycle() != FacadeLifecycle::Open {
            return Err(Self::closed_error());
        }
        Ok(self.admissions.admit())
    }
}

impl SessionReader for MemorySessionFacade {
    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Entry>, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.get_entries(ids, context).await;
            admission.finish();
            result
        })
    }

    fn get_stats<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, Result<SessionStats, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.get_stats(context).await;
            admission.finish();
            result
        })
    }

    fn get_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<StoredValue>, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.get_value(address, context).await;
            admission.finish();
            result
        })
    }

    fn scan_values<'a>(
        &'a self,
        prefix: &'a Value,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.scan_values(prefix, context).await;
            admission.finish();
            result
        })
    }

    fn read_list<'a>(
        &'a self,
        address: &'a ValueList,
        options: Option<ListReadOptions>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<ListElement>, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.read_list(address, options, context).await;
            admission.finish();
            result
        })
    }

    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.scan_branch(query, context).await;
            admission.finish();
            result
        })
    }
}

impl Session for MemorySessionFacade {
    fn metadata(&self) -> &SessionMetadata {
        self.session.metadata()
    }

    fn id_generator(&self) -> &IdGenerator {
        self.session.id_generator()
    }

    fn get_entry<'a>(
        &'a self,
        id: &'a str,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<Entry>, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.get_entry(id, context).await;
            admission.finish();
            result
        })
    }

    fn get_name<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, Result<Option<String>, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.get_name(context).await;
            admission.finish();
            result
        })
    }

    fn get_label<'a>(
        &'a self,
        target_id: &'a str,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<String>, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.get_label(target_id, context).await;
            admission.finish();
            result
        })
    }

    fn find_entries<'a>(
        &'a self,
        query: Option<EntryQuery>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.find_entries(query, context).await;
            admission.finish();
            result
        })
    }

    fn find_entry<'a>(
        &'a self,
        query: Option<EntryQuery>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<Entry>, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.find_entry(query, context).await;
            admission.finish();
            result
        })
    }

    fn branch<'a>(
        &'a self,
        name: &'a str,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<Box<dyn Branch>>, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.branch(name, context).await;
            admission.finish();
            let facade = self.handle()?;
            let branch = result?;
            Ok(branch.map(|branch| {
                Box::new(FacadeBranch {
                    source: branch,
                    facade,
                }) as Box<dyn Branch>
            }))
        })
    }

    fn create_branch<'a>(
        &'a self,
        name: &'a str,
        at: Option<String>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn Branch>, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.create_branch(name, at, context).await;
            admission.finish();
            let facade = self.handle()?;
            Ok(Box::new(FacadeBranch {
                source: result?,
                facade,
            }) as Box<dyn Branch>)
        })
    }

    fn begin_mutation<'a>(
        &'a self,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn SessionMutation>, SessionError>> {
        Box::pin(async move {
            let admission = self.admissions.admit();
            if self.lifecycle() != FacadeLifecycle::Open {
                admission.finish();
                return Err(Self::closed_error());
            }
            let source = match self.session.begin_mutation(context).await {
                Ok(source) => source,
                Err(error) => {
                    admission.finish();
                    return Err(error);
                }
            };
            if self.lifecycle() != FacadeLifecycle::Open {
                source.end(context).await;
                admission.finish();
                return Err(Self::closed_error());
            }
            Ok(Box::new(FacadeMutation {
                source,
                admission: Mutex::new(Some(admission)),
            }) as Box<dyn SessionMutation>)
        })
    }

    fn mutate<'a>(
        &'a self,
        mutation: SessionMutationCallback,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<serde_json::Value, SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self
                .session
                .mutate(
                    Arc::new(move |mutator, context| {
                        let mutation = mutation.clone();
                        Box::pin(async move { mutation(mutator, context).await })
                    }),
                    context,
                )
                .await;
            admission.finish();
            result
        })
    }

    fn set_value<'a>(
        &'a self,
        address: &'a Value,
        next: serde_json::Value,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.set_value(address, next, context).await;
            admission.finish();
            result
        })
    }

    fn delete_value<'a>(&'a self, address: &'a Value, context: &'a Context) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.delete_value(address, context).await;
            admission.finish();
            result
        })
    }

    fn append_list<'a>(
        &'a self,
        address: &'a ValueList,
        element: serde_json::Value,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.append_list(address, element, context).await;
            admission.finish();
            result
        })
    }

    fn delete_list<'a>(&'a self, address: &'a ValueList, context: &'a Context) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.delete_list(address, context).await;
            admission.finish();
            result
        })
    }

    fn set_name<'a>(&'a self, name: Option<String>, context: &'a Context) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.set_name(name, context).await;
            admission.finish();
            result
        })
    }

    fn set_label<'a>(
        &'a self,
        target_id: &'a str,
        label: Option<String>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            let admission = self.admit()?;
            let result = self.session.set_label(target_id, label, context).await;
            admission.finish();
            result
        })
    }

    fn close<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let _guard = self.close_lock.lock().await;
            if self.lifecycle() == FacadeLifecycle::Closed {
                return;
            }
            *self.state.lock().expect("facade state") = FacadeLifecycle::Closing;
            self.admissions.drain().await;
            *self.state.lock().expect("facade state") = FacadeLifecycle::Closed;
            (self.on_close)();
        })
    }
}

impl MemorySessionFacade {
    fn handle(&self) -> Result<Arc<MemorySessionFacade>, SessionError> {
        self.self_ref
            .lock()
            .expect("facade self_ref")
            .upgrade()
            .ok_or_else(Self::closed_error)
    }
}

#[derive(Clone, Default)]
pub struct MemorySessionRepoOptions {
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

struct MemorySessionRecord {
    metadata: SessionMetadata,
    storage: Arc<MemoryStorage>,
    session: Arc<StorageBackedSession>,
    open: AtomicBool,
}

pub struct MemorySessionRepo {
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    sessions: Mutex<HashMap<String, Arc<MemorySessionRecord>>>,
    pending_ids: Mutex<HashSet<String>>,
    closed: AtomicBool,
}

impl MemorySessionRepo {
    pub fn new(options: MemorySessionRepoOptions) -> Self {
        Self {
            now: options.now.unwrap_or_else(default_now),
            sessions: Mutex::new(HashMap::new()),
            pending_ids: Mutex::new(HashSet::new()),
            closed: AtomicBool::new(false),
        }
    }

    fn assert_open(&self) -> Result<(), SessionError> {
        if self.closed.load(Ordering::SeqCst) {
            Err(SessionError::new(
                SessionErrorKind::Closed,
                "MemorySessionRepo is closed",
            ))
        } else {
            Ok(())
        }
    }

    fn reserve_id(&self, id: &str) -> Result<(), SessionError> {
        let mut pending = self.pending_ids.lock().expect("pending ids");
        let sessions = self.sessions.lock().expect("sessions");
        if sessions.contains_key(id) || pending.contains(id) {
            return Err(session_invariant_error(format!("Session already exists: {id}")));
        }
        pending.insert(id.to_owned());
        Ok(())
    }

    fn open_record(&self, record: &Arc<MemorySessionRecord>) -> Box<dyn Session> {
        Box::new(self.open_record_handle(record)) as Box<dyn Session>
    }

    fn open_record_handle(&self, record: &Arc<MemorySessionRecord>) -> Arc<MemorySessionFacade> {
        let handle = record.clone();
        let facade = Arc::new(MemorySessionFacade::new(
            record.session.clone(),
            Arc::new(move || {
                handle.open.store(false, Ordering::SeqCst);
            }),
        ));
        facade.attach();
        facade
    }

    async fn build_record(
        &self,
        id: String,
        created_at: i64,
        parent_session_id: Option<String>,
        storage: Arc<MemoryStorage>,
    ) -> Result<Arc<MemorySessionRecord>, SessionError> {
        let metadata = SessionMetadata {
            id,
            created_at,
            storage_version: MEMORY_STORAGE_VERSION,
            cwd: None,
            parent_session_id,
            legacy_parent_session_path: None,
        };
        let session = Arc::new(StorageBackedSession::new(
            metadata.clone(),
            storage.clone(),
            StorageBackedSessionOptions::default(),
        ));
        session.attach();
        Ok(Arc::new(MemorySessionRecord {
            metadata,
            storage,
            session,
            open: AtomicBool::new(true),
        }))
    }
}

impl SessionRepo for MemorySessionRepo {
    fn create<'a>(
        &'a self,
        options: SessionCreateOptions,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, SessionError>> {
        Box::pin(async move {
            self.assert_open()?;
            let created_at = (self.now)();
            let id = options
                .id
                .clone()
                .unwrap_or_else(|| maho_ai::utils::uuid::uuidv7(Some(created_at as f64)).unwrap_or_default());
            self.reserve_id(&id)?;
            let storage = Arc::new(MemoryStorage::new(MemoryStorageOptions {
                now: Some(self.now.clone()),
            }));
            let result = self
                .build_record(id.clone(), created_at, options.parent_session_id.clone(), storage)
                .await;
            self.pending_ids.lock().expect("pending ids").remove(&id);
            let record = result?;
            self.sessions
                .lock()
                .expect("sessions")
                .insert(id, record.clone());
            Ok(self.open_record(&record))
        })
    }

    fn open<'a>(
        &'a self,
        metadata: SessionMetadata,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, SessionError>> {
        Box::pin(async move {
            self.assert_open()?;
            let record = self
                .sessions
                .lock()
                .expect("sessions")
                .get(&metadata.id)
                .cloned();
            let Some(record) = record else {
                return Err(session_invariant_error(format!(
                    "Unknown session: {}",
                    metadata.id
                )));
            };
            if record.open.load(Ordering::SeqCst) {
                return Err(session_invariant_error(format!(
                    "Session is already open: {}",
                    metadata.id
                )));
            }
            record.open.store(true, Ordering::SeqCst);
            Ok(self.open_record(&record))
        })
    }

    fn list<'a>(
        &'a self,
        _options: Option<serde_json::Value>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<SessionMetadata>, SessionError>> {
        Box::pin(async move {
            self.assert_open()?;
            Ok(self
                .sessions
                .lock()
                .expect("sessions")
                .values()
                .map(|record| record.metadata.clone())
                .collect())
        })
    }

    fn delete<'a>(
        &'a self,
        metadata: SessionMetadata,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            self.assert_open()?;
            let record = self
                .sessions
                .lock()
                .expect("sessions")
                .get(&metadata.id)
                .cloned();
            let Some(record) = record else {
                return Err(session_invariant_error(format!(
                    "Unknown session: {}",
                    metadata.id
                )));
            };
            if record.open.load(Ordering::SeqCst) {
                return Err(session_invariant_error(format!("Session is open: {}", metadata.id)));
            }
            record.session.close(context).await;
            self.sessions.lock().expect("sessions").remove(&metadata.id);
            Ok(())
        })
    }

    fn fork<'a>(
        &'a self,
        source: SessionMetadata,
        options: ForkOptions,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, SessionError>> {
        Box::pin(async move {
            self.assert_open()?;
            let source_record = self
                .sessions
                .lock()
                .expect("sessions")
                .get(&source.id)
                .cloned();
            let Some(source_record) = source_record else {
                return Err(session_invariant_error(format!("Unknown session: {}", source.id)));
            };
            let created_at = (self.now)();
            let id = options
                .id()
                .map(str::to_owned)
                .unwrap_or_else(|| maho_ai::utils::uuid::uuidv7(Some(created_at as f64)).unwrap_or_default());
            self.reserve_id(&id)?;
            let storage = source_record.storage.fork(&options).await;
            let result = match storage {
                Ok(storage) => {
                    self.build_record(
                        id.clone(),
                        created_at,
                        Some(source_record.metadata.id.clone()),
                        Arc::new(storage),
                    )
                    .await
                }
                Err(error) => Err(error),
            };
            self.pending_ids.lock().expect("pending ids").remove(&id);
            let record = result?;
            self.sessions
                .lock()
                .expect("sessions")
                .insert(id, record.clone());
            Ok(self.open_record(&record))
        })
    }
}

impl MemorySessionRepo {
    pub async fn close(&self, context: &Context) {
        self.closed.store(true, Ordering::SeqCst);
        let sessions: Vec<Arc<MemorySessionRecord>> =
            self.sessions.lock().expect("sessions").values().cloned().collect();
        for record in sessions {
            record.session.close(context).await;
        }
    }
}

pub type MemorySessionRepoHandle = Arc<MemorySessionRepo>;
