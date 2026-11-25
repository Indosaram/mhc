use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};

pub type RevokeFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

pub trait SessionKernelControl: Send + Sync {
    fn kernel_id(&self) -> &str;
    fn revoke(&self);
    fn retire(&self) -> RevokeFuture;
}

#[derive(Clone, Debug)]
pub struct InvocationSessionSnapshot {
    pub cwd: String,
    pub epoch: u32,
    pub active: bool,
    pub rules: Vec<CellRule>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CellRule {
    pub permission: String,
    pub pattern: String,
    pub layer: String,
}

type SnapshotReader = Arc<dyn Fn() -> InvocationSessionSnapshot + Send + Sync>;

#[derive(Default)]
struct SessionRecord {
    dead: bool,
    pending: bool,
    kernels: HashMap<String, Arc<dyn SessionKernelControl>>,
}

#[derive(Default)]
struct AuthorityTables {
    readers: HashMap<String, Vec<(u64, SnapshotReader)>>,
    next_reader_id: u64,
    sessions: HashMap<String, Arc<Mutex<SessionRecord>>>,
}

static AUTHORITY: LazyLock<Mutex<AuthorityTables>> =
    LazyLock::new(|| Mutex::new(AuthorityTables::default()));

#[derive(Clone)]
pub struct PermissionSessionHandle {
    record: Arc<Mutex<SessionRecord>>,
}

impl PermissionSessionHandle {
    pub fn dead(&self) -> bool {
        self.record.lock().expect("permission session lock").dead
    }

    pub fn mark_dead(&self) {
        self.record.lock().expect("permission session lock").dead = true;
    }

    pub fn pending(&self) -> bool {
        self.record.lock().expect("permission session lock").pending
    }
}

pub struct InvocationRegistration {
    session_id: String,
    reader_id: u64,
}

impl Drop for InvocationRegistration {
    fn drop(&mut self) {
        let mut tables = AUTHORITY.lock().expect("cell authority lock");
        if let Some(list) = tables.readers.get_mut(&self.session_id) {
            list.retain(|(id, _)| *id != self.reader_id);
            if list.is_empty() {
                tables.readers.remove(&self.session_id);
            }
        }
    }
}

pub fn register_invocation(session_id: &str, read: SnapshotReader) -> InvocationRegistration {
    let mut tables = AUTHORITY.lock().expect("cell authority lock");
    tables.next_reader_id += 1;
    let reader_id = tables.next_reader_id;
    tables
        .readers
        .entry(session_id.to_owned())
        .or_default()
        .push((reader_id, read));
    InvocationRegistration {
        session_id: session_id.to_owned(),
        reader_id,
    }
}

pub fn invocations(session_id: &str) -> Vec<InvocationSessionSnapshot> {
    let readers: Vec<SnapshotReader> = {
        let tables = AUTHORITY.lock().expect("cell authority lock");
        tables
            .readers
            .get(session_id)
            .map(|list| list.iter().map(|(_, f)| Arc::clone(f)).collect())
            .unwrap_or_default()
    };
    readers.into_iter().map(|read| read()).collect()
}

pub fn register_kernel(
    session_id: &str,
    kernel: Arc<dyn SessionKernelControl>,
) -> PermissionSessionHandle {
    let record = {
        let mut tables = AUTHORITY.lock().expect("cell authority lock");
        Arc::clone(
            tables
                .sessions
                .entry(session_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(SessionRecord::default()))),
        )
    };
    record
        .lock()
        .expect("permission session lock")
        .kernels
        .insert(kernel.kernel_id().to_owned(), kernel);
    PermissionSessionHandle { record }
}

pub fn unregister_kernel(session_id: &str, kernel_id: &str) {
    let record = {
        let tables = AUTHORITY.lock().expect("cell authority lock");
        tables.sessions.get(session_id).cloned()
    };
    if let Some(record) = record {
        let mut guard = record.lock().expect("permission session lock");
        guard.kernels.remove(kernel_id);
        let should_prune = !guard.dead && !guard.pending && guard.kernels.is_empty();
        drop(guard);
        if should_prune {
            let mut tables = AUTHORITY.lock().expect("cell authority lock");
            if let Some(existing) = tables.sessions.get(session_id)
                && Arc::ptr_eq(existing, &record)
            {
                let g = existing.lock().expect("permission session lock");
                if !g.dead && !g.pending && g.kernels.is_empty() {
                    drop(g);
                    tables.sessions.remove(session_id);
                }
            }
        }
    }
}

/// All words go DEAD synchronously, before retirement or another admission can run.
pub async fn revoke(session_id: &str) {
    let record = {
        let mut tables = AUTHORITY.lock().expect("cell authority lock");
        Arc::clone(
            tables
                .sessions
                .entry(session_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(SessionRecord::default()))),
        )
    };
    let kernels: Vec<Arc<dyn SessionKernelControl>> = {
        let mut guard = record.lock().expect("permission session lock");
        guard.dead = true;
        guard.pending = true;
        guard.kernels.values().cloned().collect()
    };
    for kernel in &kernels {
        kernel.revoke();
    }
    for kernel in kernels {
        kernel.retire().await;
    }
    record.lock().expect("permission session lock").pending = false;
}
