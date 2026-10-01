//! Port of senpi packages/agent/test/harness/instrumented-storage.test.ts.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use maho_agent::harness::context::{BACKGROUND_CONTEXT, Context};
use maho_agent::harness::session::commit::{insert_entry, insert_usage};
use maho_agent::harness::session::session::{SessionError, SessionResult};
use maho_agent::harness::session::testing::InstrumentedStorage;
use maho_agent::harness::session::types::{
    CommitResult, Entry, NewEntry, NewUsageRow, SessionStats, Storage, StorageBranchScan, Write,
};
use maho_agent::harness::session::values::{append_list, list, session_name, set_value};
use maho_agent::harness::session::{MemoryStorage, MemoryStorageOptions};
use maho_agent::harness::utils::usage::empty_usage;
use maho_ai::types::Usage;

fn empty_stats() -> SessionStats {
    SessionStats {
        message_count: 0,
        usage: empty_usage(),
    }
}

fn memory() -> Arc<dyn Storage> {
    Arc::new(MemoryStorage::new(MemoryStorageOptions {
        now: Some(Arc::new(|| 100)),
    }))
}

/// Storage whose commits park until the test resolves them, mirroring `ControlledCommitStorage`.
struct ControlledCommitStorage {
    pending: Mutex<Vec<tokio::sync::oneshot::Sender<Result<CommitResult, SessionError>>>>,
}

impl ControlledCommitStorage {
    fn new() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
        }
    }

    fn admission_count(&self) -> usize {
        self.pending.lock().expect("pending").len()
    }

    fn resolve_next(&self, result: CommitResult) {
        let sender = self.pending.lock().expect("pending").remove(0);
        let _ = sender.send(Ok(result));
    }
}

impl Storage for ControlledCommitStorage {
    fn commit<'a>(&'a self, _writes: Vec<Write>, _context: &'a Context) -> maho_ai::types::BoxFuture<'a, SessionResult<CommitResult>> {
        Box::pin(async move {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            self.pending.lock().expect("pending").push(sender);
            receiver.await.unwrap_or_else(|_| Err(SessionError::io("commit cancelled")))
        })
    }

    fn get_entries<'a>(
        &'a self,
        _ids: Vec<String>,
        _context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<BTreeMap<String, Entry>>> {
        Box::pin(async move { Ok(BTreeMap::new()) })
    }

    fn get_value<'a>(
        &'a self,
        _address: &'a maho_agent::harness::session::values::Value,
        _context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Option<maho_agent::harness::session::values::StoredValue>>> {
        Box::pin(async move { Ok(None) })
    }

    fn scan_values<'a>(
        &'a self,
        _prefix: &'a maho_agent::harness::session::values::Value,
        _context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Vec<maho_agent::harness::session::values::StoredValue>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn read_list<'a>(
        &'a self,
        _address: &'a maho_agent::harness::session::values::ValueList,
        _options: Option<maho_agent::harness::session::values::ListReadOptions>,
        _context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Vec<maho_agent::harness::session::values::ListElement>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn scan_branch<'a>(
        &'a self,
        _query: StorageBranchScan,
        _context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Vec<Entry>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn scan_branch_structure<'a>(
        &'a self,
        _query: StorageBranchScan,
        _context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Vec<maho_agent::harness::session::types::EntryStructure>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn scan_entries<'a>(
        &'a self,
        _query: maho_agent::harness::session::types::EntryScan,
        _context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Vec<Entry>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn scan_usage<'a>(
        &'a self,
        _query: maho_agent::harness::session::types::UsageScan,
        _context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, SessionResult<Vec<maho_agent::harness::session::types::UsageRow>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn get_stats<'a>(&'a self, _context: &'a Context) -> maho_ai::types::BoxFuture<'a, SessionResult<SessionStats>> {
        Box::pin(async move { Ok(empty_stats()) })
    }

    fn close<'a>(&'a self, _context: &'a Context) -> maho_ai::types::BoxFuture<'a, ()> {
        Box::pin(async move {})
    }
}

#[tokio::test]
async fn records_commit_attempts_synchronously_in_admission_order_before_settlement() {
    let delegate = Arc::new(ControlledCommitStorage::new());
    let storage = Arc::new(InstrumentedStorage::new(delegate.clone()));
    let first_transaction = vec![Write::Value(set_value(&session_name(), serde_json::json!("first")))];
    let second_transaction = vec![Write::Value(set_value(&session_name(), serde_json::json!("second")))];

    let first_commit = {
        let storage = storage.clone();
        let transaction = first_transaction.clone();
        tokio::spawn(async move { storage.commit(transaction, &BACKGROUND_CONTEXT).await })
    };
    tokio::task::yield_now().await;
    assert_eq!(storage.get_commit_attempts(), vec![first_transaction.clone()]);
    assert_eq!(delegate.admission_count(), 1);

    let second_commit = {
        let storage = storage.clone();
        let transaction = second_transaction.clone();
        tokio::spawn(async move { storage.commit(transaction, &BACKGROUND_CONTEXT).await })
    };
    tokio::task::yield_now().await;
    assert_eq!(
        storage.get_commit_attempts(),
        vec![first_transaction.clone(), second_transaction.clone()]
    );

    let first_result = CommitResult {
        first_seq: 1,
        seqs: vec![1],
        timestamp: 10,
        stats: empty_stats(),
    };
    delegate.resolve_next(first_result.clone());
    assert_eq!(first_commit.await.expect("first task").expect("first"), first_result);
    assert_eq!(
        storage.get_commit_attempts(),
        vec![first_transaction, second_transaction.clone()]
    );
    let second_result = CommitResult {
        first_seq: 2,
        seqs: vec![2],
        timestamp: 20,
        stats: empty_stats(),
    };
    delegate.resolve_next(second_result.clone());
    assert_eq!(second_commit.await.expect("second task").expect("second"), second_result);
    storage.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn clears_recorded_attempts_between_phases_without_affecting_the_delegate() {
    let delegate = memory();
    let storage = InstrumentedStorage::new(delegate.clone());
    storage
        .commit(
            vec![Write::Value(set_value(&session_name(), serde_json::json!("first")))],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("first");

    storage.clear_commit_attempts();
    assert!(storage.get_commit_attempts().is_empty());
    assert_eq!(
        storage
            .get_value(&session_name(), &BACKGROUND_CONTEXT)
            .await
            .expect("value")
            .expect("stored")
            .value,
        serde_json::json!("first")
    );

    let second_transaction = vec![Write::Value(set_value(&session_name(), serde_json::json!("second")))];
    storage
        .commit(second_transaction.clone(), &BACKGROUND_CONTEXT)
        .await
        .expect("second");
    assert_eq!(storage.get_commit_attempts(), vec![second_transaction]);
    storage.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn delegates_every_read_and_query_without_recording_synthetic_writes() {
    let delegate = memory();
    let storage = InstrumentedStorage::new(delegate.clone());
    let events = list("test.events", "").expect("address");
    let usage = Usage {
        input: 1,
        output: 2,
        total_tokens: 3,
        ..empty_usage()
    };
    storage
        .commit(
            vec![
                insert_entry(NewEntry::custom("root", None, "note")),
                Write::Value(set_value(&session_name(), serde_json::json!("session"))),
                Write::List(append_list(&events, serde_json::json!("event"))),
                insert_usage(NewUsageRow {
                    id: "usage".to_owned(),
                    usage,
                    entry_id: None,
                    adjustment: false,
                    details: None,
                }),
            ],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("commit");

    assert_eq!(
        storage.get_entries(vec!["root".to_owned()], &BACKGROUND_CONTEXT).await.expect("entries"),
        delegate.get_entries(vec!["root".to_owned()], &BACKGROUND_CONTEXT).await.expect("entries")
    );
    assert_eq!(
        storage.get_value(&session_name(), &BACKGROUND_CONTEXT).await.expect("value"),
        delegate.get_value(&session_name(), &BACKGROUND_CONTEXT).await.expect("value")
    );
    assert_eq!(
        storage.scan_values(&session_name(), &BACKGROUND_CONTEXT).await.expect("values"),
        delegate.scan_values(&session_name(), &BACKGROUND_CONTEXT).await.expect("values")
    );
    assert_eq!(
        storage.read_list(&events, None, &BACKGROUND_CONTEXT).await.expect("list"),
        delegate.read_list(&events, None, &BACKGROUND_CONTEXT).await.expect("list")
    );
    assert_eq!(
        storage
            .scan_branch(StorageBranchScan::new("root"), &BACKGROUND_CONTEXT)
            .await
            .expect("branch"),
        delegate
            .scan_branch(StorageBranchScan::new("root"), &BACKGROUND_CONTEXT)
            .await
            .expect("branch")
    );
    assert_eq!(
        storage
            .scan_branch_structure(StorageBranchScan::new("root"), &BACKGROUND_CONTEXT)
            .await
            .expect("structure"),
        delegate
            .scan_branch_structure(StorageBranchScan::new("root"), &BACKGROUND_CONTEXT)
            .await
            .expect("structure")
    );
    assert_eq!(
        storage
            .scan_entries(
                maho_agent::harness::session::types::EntryScan {
                    order: Some(maho_agent::harness::session::types::ScanOrder::Asc),
                    ..Default::default()
                },
                &BACKGROUND_CONTEXT
            )
            .await
            .expect("entries"),
        delegate
            .scan_entries(
                maho_agent::harness::session::types::EntryScan {
                    order: Some(maho_agent::harness::session::types::ScanOrder::Asc),
                    ..Default::default()
                },
                &BACKGROUND_CONTEXT
            )
            .await
            .expect("entries")
    );
    assert_eq!(
        storage
            .scan_usage(
                maho_agent::harness::session::types::UsageScan {
                    order: Some(maho_agent::harness::session::types::ScanOrder::Asc),
                    ..Default::default()
                },
                &BACKGROUND_CONTEXT
            )
            .await
            .expect("usage"),
        delegate
            .scan_usage(
                maho_agent::harness::session::types::UsageScan {
                    order: Some(maho_agent::harness::session::types::ScanOrder::Asc),
                    ..Default::default()
                },
                &BACKGROUND_CONTEXT
            )
            .await
            .expect("usage")
    );
    assert_eq!(
        storage.get_stats(&BACKGROUND_CONTEXT).await.expect("stats"),
        delegate.get_stats(&BACKGROUND_CONTEXT).await.expect("stats")
    );
    assert_eq!(storage.get_commit_attempts().len(), 1);
    storage.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn records_list_appends_without_reading_the_target_list() {
    let storage = InstrumentedStorage::new(memory());
    let events = list("test.events", "").expect("address");

    storage
        .commit(
            vec![Write::List(append_list(&events, serde_json::json!("event")))],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("commit");

    let attempts = storage.get_commit_attempts();
    assert_eq!(attempts.len(), 1);
    match &attempts[0][0] {
        Write::List(maho_agent::harness::session::values::ListWrite::Append(append)) => {
            assert_eq!(append.namespace, "test.events");
            assert_eq!(append.key, "");
            assert_eq!(append.value, serde_json::json!("event"));
        }
        other => panic!("expected list append, got {other:?}"),
    }
    storage.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn delegates_close_idempotence_and_admitted_commit_draining() {
    let delegate = memory();
    let storage = Arc::new(InstrumentedStorage::new(delegate));
    let admitted = {
        let storage = storage.clone();
        tokio::spawn(async move {
            storage
                .commit(
                    vec![Write::Value(set_value(&session_name(), serde_json::json!("admitted")))],
                    &BACKGROUND_CONTEXT,
                )
                .await
        })
    };

    let first_close = tokio::spawn({
        let storage = storage.clone();
        async move { storage.close(&BACKGROUND_CONTEXT).await }
    });
    let second_close = tokio::spawn({
        let storage = storage.clone();
        async move { storage.close(&BACKGROUND_CONTEXT).await }
    });
    admitted.await.expect("admitted").expect("admitted commit");
    first_close.await.expect("first close");
    second_close.await.expect("second close");
    assert!(storage.get_stats(&BACKGROUND_CONTEXT).await.is_err());
    assert_eq!(storage.get_commit_attempts().len(), 1);
}
