//! Port of senpi packages/agent/test/harness/gating-storage.test.ts.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_agent::harness::context::{BACKGROUND_CONTEXT, Context};
use maho_agent::harness::session::session::{SessionError, SessionErrorKind, SessionResult};
use maho_agent::harness::session::testing::{GatingStorage, InstrumentedStorage};
use maho_agent::harness::session::types::{CommitResult, Entry, Storage, StorageBranchScan, Write};
use maho_agent::harness::session::values::{session_name, set_value};
use maho_agent::harness::session::{MemoryStorage, MemoryStorageOptions};

fn memory() -> Arc<dyn Storage> {
    Arc::new(MemoryStorage::new(MemoryStorageOptions {
        now: Some(Arc::new(|| 10)),
    }))
}

fn session_name_write(value: &str) -> Write {
    Write::Value(set_value(&session_name(), serde_json::json!(value)))
}

/// Storage whose commit parks until the test releases it, then delegates to memory.
struct ControlledLandingStorage {
    inner: MemoryStorage,
    started: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
}

impl ControlledLandingStorage {
    fn new() -> Self {
        Self {
            inner: MemoryStorage::new(MemoryStorageOptions {
                now: Some(Arc::new(|| 10)),
            }),
            started: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
}

impl Storage for ControlledLandingStorage {
    fn commit<'a>(
        &'a self,
        writes: Vec<Write>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<CommitResult>> {
        Box::pin(async move {
            self.started.add_permits(1);
            if let Ok(permit) = self.release.acquire().await {
                permit.forget();
            }
            self.inner.commit(writes, context).await
        })
    }

    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<BTreeMap<String, Entry>>> {
        self.inner.get_entries(ids, context)
    }

    fn get_value<'a>(
        &'a self,
        address: &'a maho_agent::harness::session::values::Value,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Option<maho_agent::harness::session::values::StoredValue>>> {
        self.inner.get_value(address, context)
    }

    fn scan_values<'a>(
        &'a self,
        prefix: &'a maho_agent::harness::session::values::Value,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Vec<maho_agent::harness::session::values::StoredValue>>> {
        self.inner.scan_values(prefix, context)
    }

    fn read_list<'a>(
        &'a self,
        address: &'a maho_agent::harness::session::values::ValueList,
        options: Option<maho_agent::harness::session::values::ListReadOptions>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Vec<maho_agent::harness::session::values::ListElement>>> {
        self.inner.read_list(address, options, context)
    }

    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Vec<Entry>>> {
        self.inner.scan_branch(query, context)
    }

    fn scan_branch_structure<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Vec<maho_agent::harness::session::types::EntryStructure>>> {
        self.inner.scan_branch_structure(query, context)
    }

    fn scan_entries<'a>(
        &'a self,
        query: maho_agent::harness::session::types::EntryScan,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Vec<Entry>>> {
        self.inner.scan_entries(query, context)
    }

    fn scan_usage<'a>(
        &'a self,
        query: maho_agent::harness::session::types::UsageScan,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Vec<maho_agent::harness::session::types::UsageRow>>> {
        self.inner.scan_usage(query, context)
    }

    fn get_stats<'a>(
        &'a self,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<maho_agent::harness::session::types::SessionStats>> {
        self.inner.get_stats(context)
    }

    fn close<'a>(&'a self, context: &'a Context) -> maho_ai::types::BoxFuture<'a, ()> {
        self.inner.close(context)
    }
}

#[tokio::test]
async fn bypasses_setup_writes_until_armed_and_waits_for_commits_parked_after_wait_pending() {
    let storage = Arc::new(GatingStorage::new(memory()));
    storage
        .commit(vec![session_name_write("setup")], &BACKGROUND_CONTEXT)
        .await
        .expect("setup write");
    assert_eq!(storage.pending(), 0);

    storage.arm();
    let mut waiting = Box::pin(storage.wait_pending(1));
    assert!(
        futures::poll!(waiting.as_mut()).is_pending(),
        "waitPending must park until a commit arrives"
    );

    let commit = tokio::spawn({
        let storage = storage.clone();
        async move {
            storage
                .commit(vec![session_name_write("parked")], &BACKGROUND_CONTEXT)
                .await
        }
    });
    waiting.await.expect("waitPending");
    assert_eq!(storage.pending(), 1);
    assert_eq!(
        storage
            .get_value(&session_name(), &BACKGROUND_CONTEXT)
            .await
            .expect("value")
            .expect("setup value")
            .value,
        serde_json::json!("setup")
    );

    storage.next(1).await.expect("release");
    commit.await.expect("commit task").expect("commit");
    assert_eq!(storage.pending(), 0);
    assert_eq!(
        storage
            .get_value(&session_name(), &BACKGROUND_CONTEXT)
            .await
            .expect("value")
            .expect("parked value")
            .value,
        serde_json::json!("parked")
    );
    storage.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn releases_in_fifo_order_and_next_resolves_only_after_the_backend_write_lands() {
    let delegate = Arc::new(ControlledLandingStorage::new());
    let storage = Arc::new(GatingStorage::new(delegate.clone()));
    storage.arm();
    let first = tokio::spawn({
        let storage = storage.clone();
        async move {
            storage
                .commit(vec![session_name_write("first")], &BACKGROUND_CONTEXT)
                .await
        }
    });
    let second = tokio::spawn({
        let storage = storage.clone();
        async move {
            storage
                .commit(vec![session_name_write("second")], &BACKGROUND_CONTEXT)
                .await
        }
    });
    storage.wait_pending(2).await.expect("pending");
    assert_eq!(storage.pending(), 2);

    let mut next = Box::pin(storage.next(1));
    assert!(futures::poll!(next.as_mut()).is_pending(), "next must await the backend landing");
    let started = tokio::time::timeout(std::time::Duration::from_secs(5), delegate.started.acquire())
        .await
        .expect("the first commit reached the backend")
        .expect("started permit");
    started.forget();
    assert!(
        futures::poll!(next.as_mut()).is_pending(),
        "next stays pending until the backend write lands"
    );
    assert_eq!(storage.pending(), 1);
    delegate.release.add_permits(1);
    next.await.expect("next");
    first.await.expect("first task").expect("first");
    assert_eq!(
        storage
            .get_value(&session_name(), &BACKGROUND_CONTEXT)
            .await
            .expect("value")
            .expect("first value")
            .value,
        serde_json::json!("first")
    );

    let mut next = Box::pin(storage.next(1));
    assert!(futures::poll!(next.as_mut()).is_pending(), "next must await the second landing");
    let started = tokio::time::timeout(std::time::Duration::from_secs(5), delegate.started.acquire())
        .await
        .expect("the second commit reached the backend")
        .expect("started permit");
    started.forget();
    assert!(
        futures::poll!(next.as_mut()).is_pending(),
        "next stays pending until the second backend write lands"
    );
    delegate.release.add_permits(1);
    next.await.expect("next");
    second.await.expect("second task").expect("second");
    assert_eq!(
        storage
            .get_value(&session_name(), &BACKGROUND_CONTEXT)
            .await
            .expect("value")
            .expect("second value")
            .value,
        serde_json::json!("second")
    );
    storage.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn permanently_rejects_parked_waiting_and_later_commits_after_discard() {
    let storage = Arc::new(GatingStorage::new(memory()));
    storage.arm();
    let mut waiting = Box::pin(storage.wait_pending(2));
    assert!(futures::poll!(waiting.as_mut()).is_pending());
    let parked = tokio::spawn({
        let storage = storage.clone();
        async move {
            storage
                .commit(vec![session_name_write("lost")], &BACKGROUND_CONTEXT)
                .await
        }
    });
    storage.wait_pending(1).await.expect("pending");
    assert_eq!(storage.pending(), 1);

    storage.discard();
    assert_eq!(storage.pending(), 0);
    assert_eq!(
        parked.await.expect("parked task").expect_err("parked discarded").kind,
        SessionErrorKind::CommitDiscarded
    );
    assert_eq!(
        waiting.await.expect_err("waiting discarded").kind,
        SessionErrorKind::CommitDiscarded
    );
    assert_eq!(
        storage.next(1).await.expect_err("next discarded").kind,
        SessionErrorKind::CommitDiscarded
    );
    assert_eq!(
        storage
            .commit(Vec::new(), &BACKGROUND_CONTEXT)
            .await
            .expect_err("later commit discarded")
            .kind,
        SessionErrorKind::CommitDiscarded
    );
    storage.close(&BACKGROUND_CONTEXT).await;

    let unarmed = Arc::new(GatingStorage::new(memory()));
    unarmed.discard();
    assert_eq!(
        unarmed
            .commit(Vec::new(), &BACKGROUND_CONTEXT)
            .await
            .expect_err("unarmed discarded")
            .kind,
        SessionErrorKind::CommitDiscarded
    );
    unarmed.close(&BACKGROUND_CONTEXT).await;
    let _ = SessionError::io("unused");
}

#[tokio::test]
async fn records_attempts_before_gating_parks_them() {
    let gating = Arc::new(GatingStorage::new(memory()));
    let storage = Arc::new(InstrumentedStorage::new(gating.clone()));
    gating.arm();
    let writes = vec![session_name_write("recorded")];

    let commit = tokio::spawn({
        let storage = storage.clone();
        let writes = writes.clone();
        async move { storage.commit(writes, &BACKGROUND_CONTEXT).await }
    });
    tokio::task::yield_now().await;
    assert_eq!(storage.get_commit_attempts(), vec![writes]);
    gating.wait_pending(1).await.expect("pending");
    assert_eq!(gating.pending(), 1);
    gating.next(1).await.expect("release");
    commit.await.expect("commit task").expect("commit");
    storage.close(&BACKGROUND_CONTEXT).await;
}
