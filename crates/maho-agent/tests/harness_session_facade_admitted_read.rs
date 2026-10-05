//! Regression for the reusable admitted-read facade on the storage-backed (sqlite) session path.
//!
//! Pinned `packages/session-backends/sqlite-node/src/sqlite/session.ts` `SqliteOpenSession.close`
//! awaits every admitted operation before completing. The native equivalent is `MemorySessionFacade`;
//! this holds an admitted READ open and asserts `close` does not finish until the read settles.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use maho_ai::types::BoxFuture;
use tokio::sync::Notify;

use maho_agent::harness::context::{Context, BACKGROUND_CONTEXT};
use maho_agent::harness::session::testing::StorageDecorator;
use maho_agent::harness::session::types::{
    CommitResult, Entry, EntryScan, EntryStructure, SessionMetadata, SessionStats, Storage,
    StorageBranchScan, UsageRow, UsageScan, Write,
};
use maho_agent::harness::session::values::{ListElement, ListReadOptions, StoredValue, Value, ValueList};
use maho_agent::harness::session::{
    MemorySessionFacade, MemoryStorage, MemoryStorageOptions, Session, SessionReader,
    SessionResult, StorageBackedSession, StorageBackedSessionOptions,
};

const NOW: i64 = 1_700_000_000_000;

struct BlockingReadStorage {
    decorator: StorageDecorator,
    armed: Arc<AtomicBool>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl BlockingReadStorage {
    fn new(delegate: Arc<dyn Storage>) -> Self {
        Self {
            decorator: StorageDecorator::new(delegate),
            armed: Arc::new(AtomicBool::new(false)),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }
}

impl Storage for BlockingReadStorage {
    fn commit<'a>(&'a self, writes: Vec<Write>, context: &'a Context) -> BoxFuture<'a, SessionResult<CommitResult>> {
        self.decorator.commit(writes, context)
    }

    fn get_entries<'a>(&'a self, ids: Vec<String>, context: &'a Context) -> BoxFuture<'a, SessionResult<std::collections::BTreeMap<String, Entry>>> {
        self.decorator.get_entries(ids, context)
    }

    fn get_value<'a>(&'a self, address: &'a Value, context: &'a Context) -> BoxFuture<'a, SessionResult<Option<StoredValue>>> {
        self.decorator.get_value(address, context)
    }

    fn scan_values<'a>(&'a self, prefix: &'a Value, context: &'a Context) -> BoxFuture<'a, SessionResult<Vec<StoredValue>>> {
        self.decorator.scan_values(prefix, context)
    }

    fn read_list<'a>(&'a self, address: &'a ValueList, options: Option<ListReadOptions>, context: &'a Context) -> BoxFuture<'a, SessionResult<Vec<ListElement>>> {
        self.decorator.read_list(address, options, context)
    }

    fn scan_branch<'a>(&'a self, query: StorageBranchScan, context: &'a Context) -> BoxFuture<'a, SessionResult<Vec<Entry>>> {
        Box::pin(async move {
            if self.armed.load(Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.decorator.scan_branch(query, context).await
        })
    }

    fn scan_branch_structure<'a>(&'a self, query: StorageBranchScan, context: &'a Context) -> BoxFuture<'a, SessionResult<Vec<EntryStructure>>> {
        self.decorator.scan_branch_structure(query, context)
    }

    fn scan_entries<'a>(&'a self, query: EntryScan, context: &'a Context) -> BoxFuture<'a, SessionResult<Vec<Entry>>> {
        self.decorator.scan_entries(query, context)
    }

    fn scan_usage<'a>(&'a self, query: UsageScan, context: &'a Context) -> BoxFuture<'a, SessionResult<Vec<UsageRow>>> {
        self.decorator.scan_usage(query, context)
    }

    fn get_stats<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, SessionResult<SessionStats>> {
        self.decorator.get_stats(context)
    }

    fn close<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()> {
        self.decorator.close(context)
    }
}

fn metadata() -> SessionMetadata {
    SessionMetadata {
        id: "session".to_owned(),
        created_at: NOW,
        storage_version: 1,
        cwd: Some("/workspace".to_owned()),
        parent_session_id: None,
        legacy_parent_session_path: None,
    }
}

#[tokio::test]
async fn close_waits_for_an_admitted_read_on_the_storage_backed_session() {
    let storage = Arc::new(BlockingReadStorage::new(Arc::new(MemoryStorage::new(MemoryStorageOptions {
        now: Some(Arc::new(|| NOW)),
    }))));
    let session = Arc::new(StorageBackedSession::new(
        metadata(),
        storage.clone(),
        StorageBackedSessionOptions::default(),
    ));
    session.attach();
    let closed = Arc::new(AtomicBool::new(false));
    let on_close = {
        let closed = closed.clone();
        Arc::new(move || {
            closed.store(true, Ordering::SeqCst);
        })
    };
    let facade = Arc::new(MemorySessionFacade::new(session.clone(), on_close));
    facade.attach();

    let context = BACKGROUND_CONTEXT.clone();
    storage.armed.store(true, Ordering::SeqCst);
    let mut read = Box::pin(facade.scan_branch(StorageBranchScan::new("entry"), &context));
    tokio::time::timeout(std::time::Duration::from_secs(5), storage.entered.notified())
        .await
        .expect("the admitted read reaches storage");
    assert!(futures::poll!(read.as_mut()).is_pending(), "the admitted read is still in flight");

    let mut closing = Box::pin(facade.close(&context));
    assert!(futures::poll!(closing.as_mut()).is_pending(), "close must wait for the admitted read");
    assert!(!closed.load(Ordering::SeqCst));

    storage.release.notify_one();
    read.await.expect("read completes");
    closing.await;
    assert!(closed.load(Ordering::SeqCst));
}
