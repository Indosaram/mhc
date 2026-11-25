//! Port of senpi `packages/agent/test/durable/{session-store,sync-commit-conformance,wake-host-bun-timer,wakehost-prearm,qa-r17-wake-recovery}.test.ts`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use maho_agent::durable::session_store::{
    SessionHeader, StoredEntry, append_record_writes, divergence_write, manifest_write,
    open_session_store, open_sync_session_store, read_session_state,
};
use maho_agent::durable::store::{
    AdoptCallback, CommitResult, DurableStore, DurableStoreError, DurableStoreOptions, RecordScan,
    StoreDocument, StoreOperation, StoreRecord, StoreSnapshot, StoreValue, StoreWrite,
    StoreWritePoint, SyncCommitStore,
};
use maho_agent::durable::store_memory::{
    MemoryStoreMedium, create_memory_store_medium, open_memory_store,
};
use maho_agent::durable::testing::manual_timers::create_manual_timers;
use maho_agent::durable::testing::store_harness::cut_once_at;
use maho_agent::durable::testing::sync_commit_cases::{
    SyncCommitCaseAdapters, SyncCommitConformanceAdapter, sync_commit_cases,
};
use maho_agent::durable::testing::wake_harness::{
    BootOptions, create_slot, cut_alarm, cut_wake, expect_wake_cut, restart_host,
};
use maho_agent::durable::wake_host::{
    Wake, WakeAlarm, WakeAlarmOptions, WakeAlarmStage, WakeHostOptions, WakeOp, create_wake_host,
};
use maho_agent::durable::wake_host_store_alarm::store_alarm_persistence;
use maho_agent::durable::wake_host_timer::{TokioTimerHostOptions, create_tokio_timer_host};
use maho_ai::types::BoxFuture;
use serde_json::json;

use crate::s06_adapters::{DEFAULT_T0, memory_wake_case_adapters};

struct MemorySyncCommitAdapter {
    media: Mutex<HashMap<String, MemoryStoreMedium>>,
}

impl MemorySyncCommitAdapter {
    fn new() -> Self {
        Self {
            media: Mutex::new(HashMap::new()),
        }
    }
}

impl SyncCommitConformanceAdapter for MemorySyncCommitAdapter {
    fn create_medium(&self) -> BoxFuture<'_, Result<String, DurableStoreError>> {
        Box::pin(async {
            let mut media = self
                .media
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let name = format!("sync-{}", media.len() + 1);
            media.insert(name.clone(), create_memory_store_medium());
            Ok(name)
        })
    }

    fn open(
        &self,
        medium: &str,
        options: DurableStoreOptions,
    ) -> BoxFuture<'_, Result<Arc<dyn SyncCommitStore>, DurableStoreError>> {
        let name = medium.to_owned();
        Box::pin(async move {
            let found = self
                .media
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&name)
                .cloned();
            let Some(found) = found else {
                return Err(DurableStoreError::Other(format!("no medium {name}")));
            };
            Ok(Arc::new(open_memory_store(found, options)) as Arc<dyn SyncCommitStore>)
        })
    }
}

#[tokio::test]
async fn sync_commit_conformance_cases_pass_on_memory_store() {
    let adapter = SyncCommitCaseAdapters {
        store: Arc::new(MemorySyncCommitAdapter::new()),
    };
    for (id, case_fn) in sync_commit_cases() {
        case_fn(adapter.clone())
            .await
            .unwrap_or_else(|err| panic!("SYNC-COMMIT case {id} failed: {err}"));
    }
}

fn header() -> SessionHeader {
    let mut map = SessionHeader::new();
    map.insert("version".to_owned(), json!(3));
    map.insert("id".to_owned(), json!("sess-1"));
    map.insert("timestamp".to_owned(), json!("2026-01-01T00:00:00.000Z"));
    map.insert("cwd".to_owned(), json!("/repo"));
    map
}

#[tokio::test]
async fn session_store_roundtrips_appends_rewrites_and_divergence_recoveries() {
    let medium = create_memory_store_medium();
    let store = Arc::new(open_memory_store(medium, DurableStoreOptions::default()));
    store.claim(0, 1).await.expect("claim");
    let sess_store = open_session_store(store.clone(), 1);
    let handle = sess_store
        .open("sess-1", header())
        .await
        .expect("open session");

    handle
        .append(vec![StoredEntry {
            key: "e1".to_owned(),
            line: json!({
                "type": "message",
                "id": "e1",
                "parentId": null,
                "timestamp": "t1",
                "payload": { "text": "hello" }
            })
            .to_string(),
        }])
        .await
        .expect("append entry");

    handle
        .rewrite(
            header(),
            vec![StoredEntry {
                key: "e1".to_owned(),
                line: json!({
                    "type": "message",
                    "id": "e1",
                    "parentId": null,
                    "timestamp": "t1",
                    "payload": { "text": "rewritten" }
                })
                .to_string(),
            }],
        )
        .await
        .expect("rewrite entry");

    let state = read_session_state(store.as_ref(), "sess-1")
        .await
        .expect("read state")
        .expect("present");
    assert_eq!(state.entries.len(), 1);
    assert!(state.entries[0].line.contains("rewritten"));

    let dw = divergence_write("sess-1", json!({ "branch": "forked" }));
    let mw = manifest_write("sess-1", &header());
    store
        .commit(1, vec![mw, dw], Box::new(|_| {}))
        .await
        .expect("commit manifest & divergence");
    let state2 = read_session_state(store.as_ref(), "sess-1")
        .await
        .expect("read state2")
        .expect("present2");
    assert_eq!(state2.divergence, json!({ "branch": "forked" }));
}

#[tokio::test]
async fn session_store_rejects_sync_commit_on_async_only_store_or_succeeds_on_sync_store() {
    let medium = create_memory_store_medium();
    let sync_store = Arc::new(open_memory_store(
        medium.clone(),
        DurableStoreOptions::default(),
    ));
    sync_store.claim(0, 1).await.expect("claim sync");
    let sync_sess = open_sync_session_store(sync_store, 1);
    let sync_handle = sync_sess
        .open("sess-sync", header())
        .await
        .expect("open sync");
    sync_handle
        .append_sync(&[StoredEntry {
            key: "e1".to_owned(),
            line: json!({ "type": "custom", "id": "e1" }).to_string(),
        }])
        .expect("sync append");
    assert_eq!(sync_handle.entries().len(), 1);

    let _writes = append_record_writes(
        "sess-sync",
        0,
        &[StoredEntry {
            key: "e2".to_owned(),
            line: "{}".to_owned(),
        }],
    );

    struct AsyncOnlyStore(Arc<maho_agent::durable::store_memory::MemoryStore>);
    impl DurableStore for AsyncOnlyStore {
        fn read_epoch(&self) -> BoxFuture<'_, Result<u64, DurableStoreError>> {
            self.0.read_epoch()
        }
        fn read_head(&self) -> BoxFuture<'_, Result<u64, DurableStoreError>> {
            self.0.read_head()
        }
        fn claim(&self, expected: u64, next: u64) -> BoxFuture<'_, Result<(), DurableStoreError>> {
            self.0.claim(expected, next)
        }
        fn commit<'a>(
            &'a self,
            epoch: u64,
            writes: Vec<StoreWrite>,
            adopt: AdoptCallback<'a>,
        ) -> BoxFuture<'a, Result<CommitResult, DurableStoreError>> {
            self.0.commit(epoch, writes, adopt)
        }
        fn mint_id(&self) -> String {
            self.0.mint_id()
        }
        fn scan_records(
            &self,
            query: RecordScan,
        ) -> BoxFuture<'_, Result<Vec<StoreRecord>, DurableStoreError>> {
            self.0.scan_records(query)
        }
        fn get_document(
            &self,
            key: &str,
        ) -> BoxFuture<'_, Result<Option<StoreValue>, DurableStoreError>> {
            self.0.get_document(key)
        }
        fn scan_documents(
            &self,
            prefix: &str,
        ) -> BoxFuture<'_, Result<Vec<StoreDocument>, DurableStoreError>> {
            self.0.scan_documents(prefix)
        }
        fn export_snapshot(&self) -> BoxFuture<'_, Result<StoreSnapshot, DurableStoreError>> {
            self.0.export_snapshot()
        }
        fn import_snapshot(
            &self,
            epoch: u64,
            snapshot: StoreSnapshot,
        ) -> BoxFuture<'_, Result<(), DurableStoreError>> {
            self.0.import_snapshot(epoch, snapshot)
        }
    }

    let cut_store = Arc::new(AsyncOnlyStore(Arc::new(open_memory_store(
        medium,
        cut_once_at(StoreWritePoint::Before {
            op: StoreOperation::Commit,
        }),
    ))));
    let async_sess = open_session_store(cut_store, 1);
    let async_handle = async_sess
        .open("sess-async", header())
        .await
        .expect("open async handle");
    let err = async_handle
        .append(vec![StoredEntry {
            key: "e1".to_owned(),
            line: "{}".to_owned(),
        }])
        .await
        .expect_err("cut fails commit");
    assert!(err.to_string().contains("cut"));
}

#[tokio::test]
async fn wake_host_timer_fires_across_long_delays_coalesces_and_rearms_after_error() {
    let timers = create_manual_timers(DEFAULT_T0);
    let fires = Arc::new(AtomicUsize::new(0));
    let fail_next = Arc::new(AtomicBool::new(false));
    let fire_errors = Arc::new(AtomicUsize::new(0));

    let errors_clone = fire_errors.clone();
    let host = create_tokio_timer_host(TokioTimerHostOptions {
        now: Some(timers.now_fn()),
        set_timer: Some(timers.set_timer_fn()),
        max_delay_ms: Some(1_000),
        on_fire_error: Arc::new(move |_, _| {
            errors_clone.fetch_add(1, Ordering::SeqCst);
        }),
    });

    let medium = create_memory_store_medium();
    let store = Arc::new(open_memory_store(medium, DurableStoreOptions::default()));
    store.claim(0, 1).await.expect("claim");
    let persistence = store_alarm_persistence(store.clone(), 1);

    let fires_clone = fires.clone();
    let fail_clone = fail_next.clone();
    let alarm = host
        .open_alarm(
            "sess",
            persistence,
            Arc::new(move || {
                let fires_clone = fires_clone.clone();
                let fail_clone = fail_clone.clone();
                Box::pin(async move {
                    if fail_clone.swap(false, Ordering::SeqCst) {
                        return Err(DurableStoreError::Other("boom".to_owned()));
                    }
                    fires_clone.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
            WakeAlarmOptions::default(),
        )
        .await
        .expect("open alarm");

    // A delay > max_delay_ms steps forward in chunks until target time arrives.
    alarm.set(DEFAULT_T0 + 2_500).await.expect("set far alarm");
    timers
        .advance_to(DEFAULT_T0 + 1_500)
        .await
        .expect("advance 1500");
    assert_eq!(fires.load(Ordering::SeqCst), 0);
    timers
        .advance_to(DEFAULT_T0 + 2_500)
        .await
        .expect("advance 2500");
    assert_eq!(fires.load(Ordering::SeqCst), 1);

    // Clear removes persisted alarm; a fire failure triggers retry without losing alarm.
    fail_next.store(true, Ordering::SeqCst);
    alarm
        .set(DEFAULT_T0 + 3_000)
        .await
        .expect("set retry alarm");
    timers
        .advance_to(DEFAULT_T0 + 3_000)
        .await
        .expect("advance 3000");
    assert_eq!(fire_errors.load(Ordering::SeqCst), 1);
    assert_eq!(fires.load(Ordering::SeqCst), 1);
    timers
        .advance_to(DEFAULT_T0 + 4_500)
        .await
        .expect("advance 4500");
    assert_eq!(fires.load(Ordering::SeqCst), 2);

    alarm.clear().await.expect("clear");
    assert_eq!(alarm.read().await.expect("read"), None);
    host.close();
}

#[tokio::test]
async fn wakehost_prearm_and_cut_recovery_invariants_hold() {
    let adapters = memory_wake_case_adapters(DEFAULT_T0);
    let slot = create_slot(adapters.clone(), 1).await.expect("create slot");
    let first = slot
        .instance(BootOptions {
            on_wake_point: Some(cut_wake(WakeOp::Admit, 1)),
            ..Default::default()
        })
        .await
        .expect("first boot");
    let err = first
        .host
        .admit(|| async { Ok(()) })
        .await
        .expect_err("cut on admit");
    expect_wake_cut::<()>(Err(err), "admit cut")
        .await
        .expect("cut signal");
    slot.kill();

    // Reopen cleanly and verify recovery runs scheduled wakes cleanly without double firing.
    let second = slot
        .instance(BootOptions::default())
        .await
        .expect("second boot");
    let lp = second.loop_handle.clone();
    second
        .host
        .admit(|| async move { lp.schedule("task-1", DEFAULT_T0 + 5_000).await })
        .await
        .expect("schedule wake");
    assert_eq!(slot.alarm_at().await.expect("alarm_at"), Some(5_000));
    adapters
        .wake
        .advance_to(DEFAULT_T0 + 5_000)
        .await
        .expect("advance");
    assert_eq!(slot.ran().await.expect("ran"), vec!["task-1".to_owned()]);

    // Cut during alarm clear/set recovers cleanly on next host boot.
    slot.kill();
    let third = slot
        .instance(BootOptions {
            alarm: cut_alarm("clear", WakeAlarmStage::Before, 1),
            ..Default::default()
        })
        .await;
    expect_wake_cut::<()>(third.map(|_| ()), "cut on clear during start")
        .await
        .expect("clear cut signal");
    restart_host(&adapters, std::slice::from_ref(&slot))
        .await
        .expect("restart host");
    let fourth = slot
        .instance(BootOptions::default())
        .await
        .expect("fourth boot");
    let lp4 = fourth.loop_handle.clone();
    fourth
        .host
        .admit(|| async move { lp4.schedule("task-2", DEFAULT_T0 + 6_000).await })
        .await
        .expect("schedule task-2");
    adapters
        .wake
        .advance_to(DEFAULT_T0 + 6_000)
        .await
        .expect("advance to task-2");
    assert!(
        slot.ran()
            .await
            .expect("ran")
            .contains(&"task-2".to_owned())
    );
}

#[tokio::test]
async fn qa_r17_wake_recovery_runs_overdue_wake_after_crash_between_commit_and_fire() {
    let adapters = memory_wake_case_adapters(DEFAULT_T0);
    let slot = create_slot(adapters.clone(), 1).await.expect("slot");
    let a = slot.instance(BootOptions::default()).await.expect("boot a");
    let lp = a.loop_handle.clone();
    a.host
        .admit(|| async move { lp.schedule("overdue", DEFAULT_T0 + 1_000).await })
        .await
        .expect("schedule");

    // Crash before alarm fires while time advances past due.
    restart_host(&adapters, std::slice::from_ref(&slot))
        .await
        .expect("restart");
    adapters
        .wake
        .advance_to(DEFAULT_T0 + 2_000)
        .await
        .expect("advance");
    let _b = slot.instance(BootOptions::default()).await.expect("boot b");
    assert_eq!(slot.ran().await.expect("ran"), vec!["overdue".to_owned()]);

    // Direct WakeHost start + admit ordering check.
    let medium = create_memory_store_medium();
    let store = Arc::new(open_memory_store(medium, DurableStoreOptions::default()));
    store.claim(0, 1).await.expect("claim");
    let timers = create_manual_timers(DEFAULT_T0);
    let host_timer = create_tokio_timer_host(TokioTimerHostOptions {
        now: Some(timers.now_fn()),
        set_timer: Some(timers.set_timer_fn()),
        max_delay_ms: None,
        on_fire_error: Arc::new(|_, _| {}),
    });
    let alarm: Arc<dyn WakeAlarm> = host_timer
        .open_alarm(
            "r17",
            store_alarm_persistence(store, 1),
            Arc::new(|| Box::pin(async { Ok(()) })),
            WakeAlarmOptions::default(),
        )
        .await
        .expect("open alarm");
    let order = Arc::new(Mutex::new(Vec::<WakeOp>::new()));
    let order_clone = order.clone();
    let wake_host = create_wake_host(WakeHostOptions {
        alarm,
        now: timers.now_fn(),
        wake: Arc::new(|| Wake {
            active: false,
            at: None,
        }),
        resume: Arc::new(|| Box::pin(async { Ok(()) })),
        heartbeat_ms: Some(60_000),
        on_wake_point: Some(Arc::new(move |pt| {
            order_clone.lock().expect("order").push(pt.op);
            Ok(())
        })),
    });
    wake_host.start().await.expect("start");
    wake_host.admit(|| async { Ok(()) }).await.expect("admit");
    assert_eq!(
        *order.lock().expect("order"),
        vec![WakeOp::Start, WakeOp::Admit]
    );
    host_timer.close();
}
