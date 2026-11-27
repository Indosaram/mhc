//! Port of senpi `packages/agent/src/durable/testing/sync-commit-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ai::types::BoxFuture;
use serde_json::json;

use crate::conformance::registry::{AdapterKind, RegisteredCase};
use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::store::{
    CommitResult, DurableStoreError, DurableStoreErrorCode, DurableStoreOptions, StoreDocument,
    StoreOperation, StoreSnapshot, StoreWrite, StoreWritePoint, SyncCommitStore,
};

use super::store_cases::{append, put, stored};
use super::store_harness::{adopt_log, cut_once_at, expect_equal, is_cut_signal};

/// What a backend that offers `commit_sync` provides to the SYNC-COMMIT cases.
pub trait SyncCommitConformanceAdapter: Send + Sync {
    fn create_medium(&self) -> BoxFuture<'_, Result<String, DurableStoreError>>;
    fn open(
        &self,
        medium: &str,
        options: DurableStoreOptions,
    ) -> BoxFuture<'_, Result<Arc<dyn SyncCommitStore>, DurableStoreError>>;
}

#[derive(Clone)]
pub struct SyncCommitCaseAdapters {
    pub store: Arc<dyn SyncCommitConformanceAdapter>,
}

type SyncCommitCase = CaseImplementation<SyncCommitCaseAdapters>;

async fn claimed(
    adapter: &dyn SyncCommitConformanceAdapter,
    options: DurableStoreOptions,
) -> Result<(String, Arc<dyn SyncCommitStore>), String> {
    let medium = adapter.create_medium().await.map_err(|e| e.to_string())?;
    let setup = adapter
        .open(&medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    setup.claim(0, 1).await.map_err(|e| e.to_string())?;
    setup
        .commit(
            1,
            vec![append("r1", "entry"), put("a", json!(1))],
            Box::new(|_| {}),
        )
        .await
        .map_err(|e| e.to_string())?;
    let store = adapter
        .open(&medium, options)
        .await
        .map_err(|e| e.to_string())?;
    Ok((medium, store))
}

async fn reopen_sync_snapshot(
    adapter: &dyn SyncCommitConformanceAdapter,
    medium: &str,
) -> Result<StoreSnapshot, String> {
    let store = adapter
        .open(medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    store.export_snapshot().await.map_err(|e| e.to_string())
}

async fn commits_whole_on_return(adapters: SyncCommitCaseAdapters) -> ConformanceResult {
    let adapter = adapters.store;
    let (medium, store) = claimed(adapter.as_ref(), DurableStoreOptions::default()).await?;
    let log = adopt_log();
    let mut adopt = |result: &CommitResult| log.record(result);
    let outcome = store
        .commit_sync(
            1,
            vec![append("r2", "entry"), put("a", json!(2))],
            &mut adopt,
        )
        .map_err(|e| format!("a valid sync commit threw: {e}"))?;
    expect_equal(
        &outcome,
        &CommitResult {
            seqs: vec![2],
            head: 2,
        },
        "the result is the batch's seqs and the new head",
    )?;
    expect_equal(
        &log.calls(),
        &vec!["adopt:2".to_owned()],
        "adopt ran once, before commitSync returned",
    )?;
    expect_equal(
        &reopen_sync_snapshot(adapter.as_ref(), &medium).await?,
        &StoreSnapshot {
            epoch: 1,
            head: 2,
            records: vec![stored(1, "r1", "entry"), stored(2, "r2", "entry")],
            documents: vec![StoreDocument {
                key: "a".to_owned(),
                data: json!(2),
            }],
        },
        "another instance on the medium sees the batch as soon as commitSync returns",
    )
}

fn batch_writes() -> Vec<StoreWrite> {
    vec![
        append("r2", "entry"),
        put("a", json!(2)),
        StoreWrite::Delete {
            key: "a".to_owned(),
        },
        append("r3", "entry"),
    ]
}

async fn refused_throws_on_the_same_call(adapters: SyncCommitCaseAdapters) -> ConformanceResult {
    let adapter = adapters.store;
    let batch = batch_writes();
    struct Refusal {
        what: String,
        epoch: u64,
        writes: Vec<StoreWrite>,
        options: DurableStoreOptions,
        expected: &'static str,
    }
    let mut refusals = vec![
        Refusal {
            what: "stale epoch".to_owned(),
            epoch: 2,
            writes: batch.clone(),
            options: DurableStoreOptions::default(),
            expected: "stale-epoch",
        },
        Refusal {
            what: "duplicate record id".to_owned(),
            epoch: 1,
            writes: vec![append("r2", "entry"), append("r1", "entry")],
            options: DurableStoreOptions::default(),
            expected: "duplicate-record-id",
        },
        Refusal {
            what: "cut before".to_owned(),
            epoch: 1,
            writes: batch.clone(),
            options: cut_once_at(StoreWritePoint::Before {
                op: StoreOperation::Commit,
            }),
            expected: "cut",
        },
    ];
    for index in 0..batch.len() {
        refusals.push(Refusal {
            what: format!("cut at write {index}"),
            epoch: 1,
            writes: batch.clone(),
            options: cut_once_at(StoreWritePoint::Write {
                op: StoreOperation::Commit,
                index,
            }),
            expected: "cut",
        });
    }

    for refusal in refusals {
        let (medium, store) = claimed(adapter.as_ref(), refusal.options).await?;
        let before = reopen_sync_snapshot(adapter.as_ref(), &medium).await?;
        let log = adopt_log();
        let mut adopt = |result: &CommitResult| log.record(result);
        let outcome = store.commit_sync(refusal.epoch, refusal.writes, &mut adopt);
        let Err(err) = outcome else {
            return Err(format!(
                "{}: the refused sync commit did not throw on the same call",
                refusal.what
            ));
        };
        let actual = if is_cut_signal(&err) {
            "cut".to_owned()
        } else {
            match err.code() {
                Some(DurableStoreErrorCode::StaleEpoch) => "stale-epoch".to_owned(),
                Some(DurableStoreErrorCode::DuplicateRecordId) => "duplicate-record-id".to_owned(),
                Some(DurableStoreErrorCode::Unclaimed) => "unclaimed".to_owned(),
                Some(DurableStoreErrorCode::InvalidClaim) => "invalid-claim".to_owned(),
                Some(DurableStoreErrorCode::ImportConflict) => "import-conflict".to_owned(),
                None => format!("unexpected {err}"),
            }
        };
        expect_equal(
            &actual,
            &refusal.expected,
            &format!("{}: the refusal", refusal.what),
        )?;
        expect_equal(
            &log.calls(),
            &Vec::<String>::new(),
            &format!("{}: no adopt", refusal.what),
        )?;
        expect_equal(
            &reopen_sync_snapshot(adapter.as_ref(), &medium).await?,
            &before,
            &format!("{}: the store is unchanged", refusal.what),
        )?;
    }
    Ok(())
}

pub fn sync_commit_cases() -> BTreeMap<&'static str, SyncCommitCase> {
    let mut map: BTreeMap<&'static str, SyncCommitCase> = BTreeMap::new();
    map.insert(
        "SYNC-COMMIT-01",
        Arc::new(|a| Box::pin(commits_whole_on_return(a))),
    );
    map.insert(
        "SYNC-COMMIT-02",
        Arc::new(|a| Box::pin(refused_throws_on_the_same_call(a))),
    );
    map
}

pub const SYNC_COMMIT_CASES: fn() -> BTreeMap<&'static str, SyncCommitCase> = sync_commit_cases;

const SYNC_COMMIT_IMPL: &str = "packages/agent/src/durable/testing/sync-commit-cases.ts";

pub fn sync_commit_registry() -> Vec<RegisteredCase> {
    vec![
        RegisteredCase {
            id: "SYNC-COMMIT-01",
            impl_path: SYNC_COMMIT_IMPL,
            intro: "#5827",
            adapters: &[AdapterKind::Store],
            expected: "a sync commit is durable, adopted once and visible to another instance when it returns",
        },
        RegisteredCase {
            id: "SYNC-COMMIT-02",
            impl_path: SYNC_COMMIT_IMPL,
            intro: "#5827",
            adapters: &[AdapterKind::Store],
            expected: "a refused sync commit (stale epoch, duplicate id, cut) throws on the same call and changes nothing",
        },
    ]
}

pub const SYNC_COMMIT_REGISTRY: fn() -> Vec<RegisteredCase> = sync_commit_registry;
