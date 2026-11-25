//! Port of senpi `packages/agent/src/durable/testing/store-cases.ts`.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::sync::Notify;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::store::{
    CommitResult, DurableStore, DurableStoreErrorCode, DurableStoreOptions, RecordScan, ScanOrder,
    StoreDocument, StoreOperation, StoreRecord, StoreSnapshot, StoreValue, StoreWrite,
    StoreWritePoint,
};
use crate::durable::store_admission::create_admission_line;

use super::store_harness::{
    StoreCaseAdapters, StoreConformanceAdapter, adopt_log, cut_once_at, expect_cut, expect_equal,
    expect_store_error, reopen_snapshot,
};

type StoreCase = CaseImplementation<StoreCaseAdapters>;

pub fn append(id: &str, kind: &str) -> StoreWrite {
    StoreWrite::Append {
        id: id.to_owned(),
        kind: kind.to_owned(),
        data: json!({ "id": id }),
    }
}

pub fn put(key: &str, data: StoreValue) -> StoreWrite {
    StoreWrite::Put {
        key: key.to_owned(),
        data,
    }
}

pub fn stored(seq: u64, id: &str, kind: &str) -> StoreRecord {
    StoreRecord {
        seq,
        id: id.to_owned(),
        kind: kind.to_owned(),
        data: json!({ "id": id }),
    }
}

pub async fn claimed_store(
    adapter: &dyn StoreConformanceAdapter,
    epoch: u64,
) -> Result<(String, Arc<dyn DurableStore>), String> {
    let medium = adapter.create_medium().await.map_err(|e| e.to_string())?;
    let store = adapter
        .open(&medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    for next in 1..=epoch {
        store
            .claim(next - 1, next)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok((medium, store))
}

async fn commit_adopts_on_durable(adapters: StoreCaseAdapters) -> ConformanceResult {
    let adapter = adapters.store;
    let medium = adapter.create_medium().await.map_err(|e| e.to_string())?;
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let events_pt = Arc::clone(&events);
    let store = adapter
        .open(
            &medium,
            DurableStoreOptions {
                on_write_point: Some(Arc::new(move |point| {
                    if matches!(
                        point,
                        StoreWritePoint::Durable {
                            op: StoreOperation::Commit
                        }
                    ) {
                        events_pt
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push("durable".to_owned());
                    }
                    Ok(())
                })),
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    store.claim(0, 1).await.map_err(|e| e.to_string())?;

    let events_adopt = Arc::clone(&events);
    let first = store
        .commit(
            1,
            vec![
                append("r1", "entry"),
                append("r2", "entry"),
                put("doc", json!(1)),
            ],
            Box::new(move |result| {
                let joined: Vec<String> = result.seqs.iter().map(ToString::to_string).collect();
                events_adopt
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(format!("adopt:{}", joined.join(",")));
            }),
        )
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &first,
        &CommitResult {
            seqs: vec![1, 2],
            head: 2,
        },
        "batch",
    )?;
    let recorded = events.lock().unwrap_or_else(|p| p.into_inner()).clone();
    expect_equal(
        &recorded,
        &vec!["durable".to_owned(), "adopt:1,2".to_owned()],
        "adopt runs once, after the batch is durable",
    )?;

    let events_adopt2 = Arc::clone(&events);
    let second = store
        .commit(
            1,
            vec![put("doc", json!(2))],
            Box::new(move |result| {
                let joined: Vec<String> = result.seqs.iter().map(ToString::to_string).collect();
                events_adopt2
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(format!("adopt:{}", joined.join(",")));
            }),
        )
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &second,
        &CommitResult {
            seqs: vec![],
            head: 2,
        },
        "documents-only batch",
    )?;
    let snap = reopen_snapshot(adapter.as_ref(), &medium).await?;
    expect_equal(
        &snap,
        &StoreSnapshot {
            epoch: 1,
            head: 2,
            records: vec![stored(1, "r1", "entry"), stored(2, "r2", "entry")],
            documents: vec![StoreDocument {
                key: "doc".to_owned(),
                data: json!(2),
            }],
        },
        "another instance on the medium sees every committed batch whole",
    )
}

fn batch_writes() -> Vec<StoreWrite> {
    vec![
        append("r2", "entry"),
        put("a", json!(2)),
        StoreWrite::Delete {
            key: "b".to_owned(),
        },
        append("r3", "entry"),
        put("c", json!(1)),
    ]
}

async fn cut_at_every_write_point(adapters: StoreCaseAdapters) -> ConformanceResult {
    let adapter = adapters.store;
    let batch = batch_writes();
    let mut points = vec![StoreWritePoint::Before {
        op: StoreOperation::Commit,
    }];
    for index in 0..batch.len() {
        points.push(StoreWritePoint::Write {
            op: StoreOperation::Commit,
            index,
        });
    }
    for point in points {
        let what = format!(
            "cut at {}",
            serde_json::to_string(&point).unwrap_or_default()
        );
        let (medium, setup) = claimed_store(adapter.as_ref(), 1).await?;
        setup
            .commit(
                1,
                vec![
                    append("r1", "entry"),
                    put("a", json!(1)),
                    put("b", json!(1)),
                ],
                Box::new(|_| {}),
            )
            .await
            .map_err(|e| e.to_string())?;
        let baseline = setup.export_snapshot().await.map_err(|e| e.to_string())?;
        let store = adapter
            .open(&medium, cut_once_at(point))
            .await
            .map_err(|e| e.to_string())?;
        let log = adopt_log();
        expect_cut(store.commit(1, batch.clone(), log.callback()).await, &what)?;
        expect_equal(
            &log.calls(),
            &Vec::<String>::new(),
            &format!("{what}: no adopt"),
        )?;
        expect_equal(
            &reopen_snapshot(adapter.as_ref(), &medium).await?,
            &baseline,
            &format!("{what}: nothing of the batch is durable"),
        )?;
        let retried = store
            .commit(1, batch.clone(), log.callback())
            .await
            .map_err(|e| e.to_string())?;
        expect_equal(
            &retried,
            &CommitResult {
                seqs: vec![2, 3],
                head: 3,
            },
            &format!("{what}: no seq burned"),
        )?;
        expect_equal(
            &log.calls(),
            &vec!["adopt:2,3".to_owned()],
            &format!("{what}: the retried batch adopts once"),
        )?;
    }
    Ok(())
}

async fn concurrent_admissions(adapters: StoreCaseAdapters) -> ConformanceResult {
    let (_, store) = claimed_store(adapters.store.as_ref(), 1).await?;
    let line = create_admission_line();
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let gate = Arc::new(Notify::new());
    let gate_opened = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let run_admit = |name: &'static str, wait_gate: bool| {
        let line = line.clone();
        let store = Arc::clone(&store);
        let events = Arc::clone(&events);
        let gate = Arc::clone(&gate);
        let gate_opened = Arc::clone(&gate_opened);
        async move {
            line.admit(move || async move {
                events
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(format!("{name}:start"));
                if wait_gate {
                    while !gate_opened.load(std::sync::atomic::Ordering::SeqCst) {
                        gate.notified().await;
                    }
                }
                let events_adopt = Arc::clone(&events);
                store
                    .commit(
                        1,
                        vec![append(name, "entry")],
                        Box::new(move |result| {
                            let joined: Vec<String> =
                                result.seqs.iter().map(ToString::to_string).collect();
                            events_adopt
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .push(format!("{name}:adopt:{}", joined.join(",")));
                        }),
                    )
                    .await
                    .map_err(|e| e.to_string())
            })
            .await
        }
    };

    let fut_a = run_admit("a", true);
    let fut_b = run_admit("b", false);
    let line_c = line.clone();
    let events_c = Arc::clone(&events);
    let fut_c = async move {
        line_c
            .admit(move || async move {
                events_c
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push("c:start".to_owned());
                Err::<CommitResult, String>("mutation refused".to_owned())
            })
            .await
    };
    let fut_d = run_admit("d", false);

    let release = async {
        tokio::task::yield_now().await;
        gate_opened.store(true, std::sync::atomic::Ordering::SeqCst);
        gate.notify_waiters();
    };

    let (ra, rb, rc, rd, ()) = tokio::join!(fut_a, fut_b, fut_c, fut_d, release);
    let statuses = vec![
        if ra.is_ok() { "fulfilled" } else { "rejected" },
        if rb.is_ok() { "fulfilled" } else { "rejected" },
        if rc.is_ok() { "fulfilled" } else { "rejected" },
        if rd.is_ok() { "fulfilled" } else { "rejected" },
    ];
    expect_equal(
        &statuses,
        &vec!["fulfilled", "fulfilled", "rejected", "fulfilled"],
        "admission outcomes",
    )?;
    let recorded = events.lock().unwrap_or_else(|p| p.into_inner()).clone();
    expect_equal(
        &recorded,
        &vec![
            "a:start",
            "a:adopt:1",
            "b:start",
            "b:adopt:2",
            "c:start",
            "d:start",
            "d:adopt:3",
        ],
        "one admission at a time, in admission order; a refused mutation releases the line",
    )?;
    let recs: Vec<String> = store
        .scan_records(RecordScan::default())
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|r| format!("{}:{}", r.seq, r.id))
        .collect();
    expect_equal(&recs, &vec!["1:a", "2:b", "3:d"], "records")
}

async fn delayed_old_owner(adapters: StoreCaseAdapters) -> ConformanceResult {
    let adapter = adapters.store;
    let (medium, old_owner) = claimed_store(adapter.as_ref(), 1).await?;
    old_owner
        .commit(1, vec![append("old-1", "entry")], Box::new(|_| {}))
        .await
        .map_err(|e| e.to_string())?;
    let new_owner = adapter
        .open(&medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    new_owner.claim(1, 2).await.map_err(|e| e.to_string())?;
    new_owner
        .commit(2, vec![append("new-1", "entry")], Box::new(|_| {}))
        .await
        .map_err(|e| e.to_string())?;
    let late = adopt_log();

    expect_store_error(
        old_owner
            .commit(1, vec![append("old-2", "entry")], late.callback())
            .await,
        DurableStoreErrorCode::StaleEpoch,
        "old owner's delayed commit",
    )?;
    expect_store_error(
        old_owner.claim(1, 2).await,
        DurableStoreErrorCode::StaleEpoch,
        "old owner's delayed claim",
    )?;
    expect_store_error(
        new_owner
            .commit(3, vec![append("x", "entry")], late.callback())
            .await,
        DurableStoreErrorCode::StaleEpoch,
        "a commit from a future epoch",
    )?;
    expect_store_error(
        new_owner.claim(2, 2).await,
        DurableStoreErrorCode::InvalidClaim,
        "a claim that does not raise the epoch",
    )?;
    let fresh_medium = adapter.create_medium().await.map_err(|e| e.to_string())?;
    let fresh = adapter
        .open(&fresh_medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    expect_store_error(
        fresh
            .commit(0, vec![append("x", "entry")], late.callback())
            .await,
        DurableStoreErrorCode::Unclaimed,
        "a commit to an unclaimed store",
    )?;
    expect_equal(
        &late.calls(),
        &Vec::<String>::new(),
        "no refused commit adopts",
    )?;
    expect_equal(
        &reopen_snapshot(adapter.as_ref(), &medium).await?,
        &StoreSnapshot {
            epoch: 2,
            head: 2,
            records: vec![stored(1, "old-1", "entry"), stored(2, "new-1", "entry")],
            documents: vec![],
        },
        "the new owner's state",
    )
}

async fn claim_and_import(adapters: StoreCaseAdapters) -> ConformanceResult {
    let adapter = adapters.store;
    let (_, source) = claimed_store(adapter.as_ref(), 1).await?;
    source
        .commit(
            1,
            vec![
                append("r1", "entry"),
                append("r2", "entry"),
                put("doc", json!("v1")),
            ],
            Box::new(|_| {}),
        )
        .await
        .map_err(|e| e.to_string())?;
    let first = source.export_snapshot().await.map_err(|e| e.to_string())?;
    let (medium, target) = claimed_store(adapter.as_ref(), 2).await?;

    expect_store_error(
        target.import_snapshot(1, first.clone()).await,
        DurableStoreErrorCode::StaleEpoch,
        "import under a non-current epoch",
    )?;
    target
        .import_snapshot(2, first.clone())
        .await
        .map_err(|e| e.to_string())?;
    target
        .import_snapshot(2, first.clone())
        .await
        .map_err(|e| e.to_string())?;
    let mut expected_first = first.clone();
    expected_first.epoch = 2;
    expect_equal(
        &reopen_snapshot(adapter.as_ref(), &medium).await?,
        &expected_first,
        "import is whole and idempotent",
    )?;

    source
        .commit(
            1,
            vec![append("r3", "entry"), put("doc", json!("v2"))],
            Box::new(|_| {}),
        )
        .await
        .map_err(|e| e.to_string())?;
    let tail = source.export_snapshot().await.map_err(|e| e.to_string())?;
    target
        .import_snapshot(2, tail.clone())
        .await
        .map_err(|e| e.to_string())?;
    let mut expected_tail = tail.clone();
    expected_tail.epoch = 2;
    expect_equal(
        &reopen_snapshot(adapter.as_ref(), &medium).await?,
        &expected_tail,
        "the tail past the head is imported once",
    )?;

    let mut conflicting = tail.clone();
    conflicting.records = vec![
        stored(1, "r1", "entry"),
        stored(2, "other", "entry"),
        stored(3, "r3", "entry"),
    ];
    expect_store_error(
        target.import_snapshot(2, conflicting).await,
        DurableStoreErrorCode::ImportConflict,
        "a diverged log",
    )?;
    let mut not_above = tail.clone();
    not_above.epoch = 2;
    expect_store_error(
        target.import_snapshot(2, not_above).await,
        DurableStoreErrorCode::InvalidClaim,
        "an import not above the source",
    )?;
    expect_equal(
        &reopen_snapshot(adapter.as_ref(), &medium).await?,
        &expected_tail,
        "refused imports change nothing",
    )?;
    let next = target
        .commit(2, vec![append("r4", "entry")], Box::new(|_| {}))
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &next,
        &CommitResult {
            seqs: vec![4],
            head: 4,
        },
        "the new owner continues the log",
    )
}

async fn claim_raises_epoch_by_one(adapters: StoreCaseAdapters) -> ConformanceResult {
    let adapter = adapters.store;
    let (medium, store) = claimed_store(adapter.as_ref(), 1).await?;
    store
        .commit(1, vec![append("r1", "entry")], Box::new(|_| {}))
        .await
        .map_err(|e| e.to_string())?;
    let baseline = store.export_snapshot().await.map_err(|e| e.to_string())?;
    let fresh_medium = adapter.create_medium().await.map_err(|e| e.to_string())?;
    let fresh = adapter
        .open(&fresh_medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?;

    expect_store_error(
        store.claim(1, 3).await,
        DurableStoreErrorCode::InvalidClaim,
        "a claim that skips an epoch",
    )?;
    expect_store_error(
        fresh.claim(0, 2).await,
        DurableStoreErrorCode::InvalidClaim,
        "a first claim that skips epoch 1",
    )?;
    expect_equal(
        &fresh.read_epoch().await.map_err(|e| e.to_string())?,
        &0u64,
        "a refused first claim leaves the store unclaimed",
    )?;
    expect_equal(
        &reopen_snapshot(adapter.as_ref(), &medium).await?,
        &baseline,
        "a refused claim leaves the epoch and log unchanged",
    )?;
    store.claim(1, 2).await.map_err(|e| e.to_string())?;
    expect_equal(
        &store.read_epoch().await.map_err(|e| e.to_string())?,
        &2u64,
        "the next epoch is still claimable",
    )
}

async fn ids_scans_and_documents(adapters: StoreCaseAdapters) -> ConformanceResult {
    let adapter = adapters.store;
    let (medium, store) = claimed_store(adapter.as_ref(), 1).await?;
    let reopened = adapter
        .open(&medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    let elsewhere_medium = adapter.create_medium().await.map_err(|e| e.to_string())?;
    let elsewhere = adapter
        .open(&elsewhere_medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?;

    let mut ids = Vec::new();
    for s in [&store, &reopened, &elsewhere] {
        for _ in 0..100 {
            ids.push(s.mint_id());
        }
    }
    let unique: HashSet<&String> = ids.iter().collect();
    expect_equal(
        &unique.len(),
        &ids.len(),
        "minted IDs are unique across instances, reopens and hosts",
    )?;

    let kinds = ["a", "b", "a", "b", "a"];
    let mut writes: Vec<StoreWrite> = kinds
        .iter()
        .enumerate()
        .map(|(i, kind)| append(&format!("r{}", i + 1), kind))
        .collect();
    writes.push(put("s/2", json!(2)));
    writes.push(put("s/1", json!(1)));
    writes.push(put("t/1", json!(3)));
    store
        .commit(1, writes, Box::new(|_| {}))
        .await
        .map_err(|e| e.to_string())?;

    let seqs_of = |query: RecordScan| {
        let store = Arc::clone(&store);
        async move {
            Ok::<Vec<u64>, String>(
                store
                    .scan_records(query)
                    .await
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|r| r.seq)
                    .collect(),
            )
        }
    };
    expect_equal(
        &seqs_of(RecordScan::default()).await?,
        &vec![1, 2, 3, 4, 5],
        "full scan",
    )?;
    expect_equal(
        &seqs_of(RecordScan {
            from_seq: Some(2),
            to_seq: Some(4),
            ..Default::default()
        })
        .await?,
        &vec![2, 3, 4],
        "range scan",
    )?;
    expect_equal(
        &seqs_of(RecordScan {
            kind: Some("b".to_owned()),
            ..Default::default()
        })
        .await?,
        &vec![2, 4],
        "kind scan",
    )?;
    expect_equal(
        &seqs_of(RecordScan {
            order: Some(ScanOrder::Desc),
            limit: Some(2),
            ..Default::default()
        })
        .await?,
        &vec![5, 4],
        "descending, limited",
    )?;
    expect_equal(
        &seqs_of(RecordScan {
            kind: Some("a".to_owned()),
            order: Some(ScanOrder::Desc),
            limit: Some(1),
            ..Default::default()
        })
        .await?,
        &vec![5],
        "kind, descending, limited",
    )?;

    expect_equal(
        &store
            .scan_documents("s/")
            .await
            .map_err(|e| e.to_string())?,
        &vec![
            StoreDocument {
                key: "s/1".to_owned(),
                data: json!(1),
            },
            StoreDocument {
                key: "s/2".to_owned(),
                data: json!(2),
            },
        ],
        "prefix scan",
    )?;
    store
        .commit(
            1,
            vec![StoreWrite::Delete {
                key: "s/1".to_owned(),
            }],
            Box::new(|_| {}),
        )
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &store.get_document("s/1").await.map_err(|e| e.to_string())?,
        &Option::<StoreValue>::None,
        "a deleted document",
    )?;
    let doc_keys: Vec<String> = store
        .scan_documents("")
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|d| d.key)
        .collect();
    expect_equal(&doc_keys, &vec!["s/2", "t/1"], "all documents by key")?;

    expect_store_error(
        store
            .commit(1, vec![append("r1", "entry")], Box::new(|_| {}))
            .await,
        DurableStoreErrorCode::DuplicateRecordId,
        "an ID already in the log",
    )?;
    expect_store_error(
        store
            .commit(
                1,
                vec![append("x", "entry"), append("x", "entry")],
                Box::new(|_| {}),
            )
            .await,
        DurableStoreErrorCode::DuplicateRecordId,
        "a repeated ID",
    )?;
    expect_equal(
        &store.read_head().await.map_err(|e| e.to_string())?,
        &5u64,
        "refused batches change nothing",
    )?;

    let mut value = json!({ "nested": { "n": 1 } });
    store
        .commit(1, vec![put("v", value.clone())], Box::new(|_| {}))
        .await
        .map_err(|e| e.to_string())?;
    value["nested"]["n"] = json!(2);
    let read = store.get_document("v").await.map_err(|e| e.to_string())?;
    expect_equal(
        &read,
        &Some(json!({ "nested": { "n": 1 } })),
        "the store keeps its own copy of written values",
    )
}

pub fn store_cases() -> BTreeMap<&'static str, StoreCase> {
    let mut map: BTreeMap<&'static str, StoreCase> = BTreeMap::new();
    map.insert(
        "STORE-01",
        Arc::new(|a| Box::pin(commit_adopts_on_durable(a))),
    );
    map.insert(
        "STORE-02",
        Arc::new(|a| Box::pin(cut_at_every_write_point(a))),
    );
    map.insert("STORE-03", Arc::new(|a| Box::pin(concurrent_admissions(a))));
    map.insert("STORE-04", Arc::new(|a| Box::pin(delayed_old_owner(a))));
    map.insert("STORE-05", Arc::new(|a| Box::pin(claim_and_import(a))));
    map.insert(
        "STORE-06",
        Arc::new(|a| Box::pin(ids_scans_and_documents(a))),
    );
    map.insert(
        "STORE-07",
        Arc::new(|a| Box::pin(claim_raises_epoch_by_one(a))),
    );
    map
}

pub const STORE_CASES: fn() -> BTreeMap<&'static str, StoreCase> = store_cases;
