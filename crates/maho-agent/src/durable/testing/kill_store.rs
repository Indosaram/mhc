//! Port of senpi `packages/agent/src/durable/testing/kill-store.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ai::types::BoxFuture;
use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::store::{
    CommitResult, DurableStore, DurableStoreError, DurableStoreErrorCode, DurableStoreOptions,
    StoreDocument, StoreOperation, StoreSnapshot, StoreWrite,
};

use super::store_cases::{append, claimed_store, put, stored};
use super::store_harness::{
    StoreCaseAdapters, StoreConformanceAdapter, adopt_log, cut_once_at, cut_op_at, expect_cut,
    expect_equal, expect_store_error,
};

/// Where a KILL case applies; a host report logs it in kill[] with the registry's expected outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillDeclaration {
    pub id: &'static str,
    pub op: StoreOperation,
    pub at: &'static str,
    pub placement: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct KillPoint {
    op: StoreOperation,
    at: &'static str,
}

fn content(snap: &StoreSnapshot) -> serde_json::Value {
    json!({
        "head": snap.head,
        "records": snap.records,
        "documents": snap.documents,
    })
}

async fn seeded(adapter: &dyn StoreConformanceAdapter) -> Result<(String, StoreSnapshot), String> {
    let (medium, store) = claimed_store(adapter, 1).await?;
    store
        .commit(
            1,
            vec![append("r1", "entry"), put("doc", json!(1))],
            Box::new(|_| {}),
        )
        .await
        .map_err(|e| e.to_string())?;
    let baseline = store.export_snapshot().await.map_err(|e| e.to_string())?;
    Ok((medium, baseline))
}

async fn kill_then_reopen<F, Fut, T>(
    adapter: &dyn StoreConformanceAdapter,
    medium: &str,
    kill: KillPoint,
    operation: F,
) -> Result<Arc<dyn DurableStore>, String>
where
    F: FnOnce(Arc<dyn DurableStore>) -> Fut,
    Fut: std::future::Future<Output = Result<T, DurableStoreError>>,
{
    let store = adapter
        .open(medium, cut_once_at(cut_op_at(kill.op, kill.at)))
        .await
        .map_err(|e| e.to_string())?;
    expect_cut(
        operation(store).await,
        &format!("kill {} {:?}", kill.at, kill.op),
    )?;
    adapter
        .open(medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())
}

fn second_batch() -> Vec<StoreWrite> {
    vec![append("r2", "entry"), put("doc", json!(2))]
}

async fn kill_store_01(
    adapter: Arc<dyn StoreConformanceAdapter>,
    kill: KillPoint,
) -> ConformanceResult {
    let (medium, baseline) = seeded(adapter.as_ref()).await?;
    let log = adopt_log();
    let restarted = kill_then_reopen(adapter.as_ref(), &medium, kill, |s| async move {
        s.commit(1, second_batch(), log.callback()).await
    })
    .await?;
    expect_equal(
        &restarted
            .export_snapshot()
            .await
            .map_err(|e| e.to_string())?,
        &baseline,
        "nothing of the batch is durable",
    )?;
    let log2 = adopt_log();
    let res = restarted
        .commit(1, second_batch(), log2.callback())
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &res,
        &CommitResult {
            seqs: vec![2],
            head: 2,
        },
        "same IDs, next seq",
    )?;
    expect_equal(
        &log2.calls(),
        &vec!["adopt:2".to_owned()],
        "only the committed batch adopts",
    )
}

async fn kill_store_02(
    adapter: Arc<dyn StoreConformanceAdapter>,
    kill: KillPoint,
) -> ConformanceResult {
    let (medium, baseline) = seeded(adapter.as_ref()).await?;
    let log = adopt_log();
    let log_cb = log.callback();
    let restarted = kill_then_reopen(adapter.as_ref(), &medium, kill, |s| async move {
        s.commit(1, second_batch(), log_cb).await
    })
    .await?;
    let expected = StoreSnapshot {
        epoch: baseline.epoch,
        head: 2,
        records: vec![stored(1, "r1", "entry"), stored(2, "r2", "entry")],
        documents: vec![StoreDocument {
            key: "doc".to_owned(),
            data: json!(2),
        }],
    };
    expect_equal(
        &log.calls(),
        &Vec::<String>::new(),
        "the killed process never adopted",
    )?;
    expect_equal(
        &restarted
            .export_snapshot()
            .await
            .map_err(|e| e.to_string())?,
        &expected,
        "the whole batch is durable with its IDs and seqs",
    )?;
    expect_store_error(
        restarted.commit(1, second_batch(), log.callback()).await,
        DurableStoreErrorCode::DuplicateRecordId,
        "a retried batch",
    )?;
    expect_equal(
        &restarted
            .export_snapshot()
            .await
            .map_err(|e| e.to_string())?,
        &expected,
        "the retry applies nothing twice",
    )
}

async fn kill_store_03(
    adapter: Arc<dyn StoreConformanceAdapter>,
    kill: KillPoint,
) -> ConformanceResult {
    let (medium, baseline) = seeded(adapter.as_ref()).await?;
    let restarted = kill_then_reopen(adapter.as_ref(), &medium, kill, |s| async move {
        s.claim(1, 2).await
    })
    .await?;
    expect_equal(
        &restarted
            .export_snapshot()
            .await
            .map_err(|e| e.to_string())?,
        &baseline,
        "the epoch and log are unchanged",
    )?;
    restarted.claim(1, 2).await.map_err(|e| e.to_string())?;
    expect_equal(
        &restarted.read_epoch().await.map_err(|e| e.to_string())?,
        &2u64,
        "the claim succeeds after restart",
    )
}

async fn kill_store_04(
    adapter: Arc<dyn StoreConformanceAdapter>,
    kill: KillPoint,
) -> ConformanceResult {
    let (medium, baseline) = seeded(adapter.as_ref()).await?;
    let restarted = kill_then_reopen(adapter.as_ref(), &medium, kill, |s| async move {
        s.claim(1, 2).await
    })
    .await?;
    let mut expected = baseline.clone();
    expected.epoch = 2;
    expect_equal(
        &restarted
            .export_snapshot()
            .await
            .map_err(|e| e.to_string())?,
        &expected,
        "the claim is durable; the log is intact",
    )?;
    expect_store_error(
        restarted.claim(1, 2).await,
        DurableStoreErrorCode::StaleEpoch,
        "the claim cannot be taken twice",
    )?;
    expect_store_error(
        restarted
            .commit(1, vec![append("late", "entry")], Box::new(|_| {}))
            .await,
        DurableStoreErrorCode::StaleEpoch,
        "the old epoch is fenced",
    )
}

async fn kill_store_05(
    adapter: Arc<dyn StoreConformanceAdapter>,
    kill: KillPoint,
) -> ConformanceResult {
    let (_, baseline) = seeded(adapter.as_ref()).await?;
    let (medium, _) = claimed_store(adapter.as_ref(), 2).await?;
    let base_clone = baseline.clone();
    let restarted = kill_then_reopen(adapter.as_ref(), &medium, kill, |s| async move {
        s.import_snapshot(2, base_clone).await
    })
    .await?;
    expect_equal(
        &restarted
            .export_snapshot()
            .await
            .map_err(|e| e.to_string())?,
        &StoreSnapshot {
            epoch: 2,
            head: 0,
            records: vec![],
            documents: vec![],
        },
        "nothing imported",
    )?;
    restarted
        .import_snapshot(2, baseline.clone())
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &content(
            &restarted
                .export_snapshot()
                .await
                .map_err(|e| e.to_string())?,
        ),
        &content(&baseline),
        "the import succeeds after restart",
    )
}

async fn kill_store_06(
    adapter: Arc<dyn StoreConformanceAdapter>,
    kill: KillPoint,
) -> ConformanceResult {
    let (_, baseline) = seeded(adapter.as_ref()).await?;
    let (medium, _) = claimed_store(adapter.as_ref(), 2).await?;
    let base_clone = baseline.clone();
    let restarted = kill_then_reopen(adapter.as_ref(), &medium, kill, |s| async move {
        s.import_snapshot(2, base_clone).await
    })
    .await?;
    expect_equal(
        &content(
            &restarted
                .export_snapshot()
                .await
                .map_err(|e| e.to_string())?,
        ),
        &content(&baseline),
        "the whole import is durable",
    )?;
    restarted
        .import_snapshot(2, baseline.clone())
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &content(
            &restarted
                .export_snapshot()
                .await
                .map_err(|e| e.to_string())?,
        ),
        &content(&baseline),
        "re-import adds nothing twice",
    )
}

pub fn kill_store_declarations() -> Vec<KillDeclaration> {
    vec![
        KillDeclaration {
            id: "KILL-STORE-01",
            op: StoreOperation::Commit,
            at: "before",
            placement: "everywhere",
        },
        KillDeclaration {
            id: "KILL-STORE-02",
            op: StoreOperation::Commit,
            at: "durable",
            placement: "everywhere",
        },
        KillDeclaration {
            id: "KILL-STORE-03",
            op: StoreOperation::Claim,
            at: "before",
            placement: "everywhere",
        },
        KillDeclaration {
            id: "KILL-STORE-04",
            op: StoreOperation::Claim,
            at: "durable",
            placement: "everywhere",
        },
        KillDeclaration {
            id: "KILL-STORE-05",
            op: StoreOperation::Import,
            at: "before",
            placement: "everywhere",
        },
        KillDeclaration {
            id: "KILL-STORE-06",
            op: StoreOperation::Import,
            at: "durable",
            placement: "everywhere",
        },
    ]
}

pub const KILL_STORE_DECLARATIONS: fn() -> Vec<KillDeclaration> = kill_store_declarations;

fn make_case<F>(
    op: StoreOperation,
    at: &'static str,
    f: fn(Arc<dyn StoreConformanceAdapter>, KillPoint) -> F,
) -> CaseImplementation<StoreCaseAdapters>
where
    F: std::future::Future<Output = ConformanceResult> + Send + 'static,
{
    Arc::new(move |adapters: StoreCaseAdapters| {
        let fut: BoxFuture<'static, ConformanceResult> =
            Box::pin(f(adapters.store, KillPoint { op, at }));
        fut
    })
}

pub fn kill_store_cases() -> BTreeMap<&'static str, CaseImplementation<StoreCaseAdapters>> {
    let mut map: BTreeMap<&'static str, CaseImplementation<StoreCaseAdapters>> = BTreeMap::new();
    map.insert(
        "KILL-STORE-01",
        make_case(StoreOperation::Commit, "before", kill_store_01),
    );
    map.insert(
        "KILL-STORE-02",
        make_case(StoreOperation::Commit, "durable", kill_store_02),
    );
    map.insert(
        "KILL-STORE-03",
        make_case(StoreOperation::Claim, "before", kill_store_03),
    );
    map.insert(
        "KILL-STORE-04",
        make_case(StoreOperation::Claim, "durable", kill_store_04),
    );
    map.insert(
        "KILL-STORE-05",
        make_case(StoreOperation::Import, "before", kill_store_05),
    );
    map.insert(
        "KILL-STORE-06",
        make_case(StoreOperation::Import, "durable", kill_store_06),
    );
    map
}

pub const KILL_STORE_CASES: fn() -> BTreeMap<&'static str, CaseImplementation<StoreCaseAdapters>> =
    kill_store_cases;
