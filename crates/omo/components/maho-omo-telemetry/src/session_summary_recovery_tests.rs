use crate::{
    session_summary_recovery::*, session_summary_storage::*, session_summary_store::*,
    session_summary_window::open_window,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn dead_window(store: &SummaryStore, cwd: &std::path::Path, key: &str) -> Value {
    let mut record = rollup(key, 2, open_window(1000.0, &json!(1)), cwd, 1000.0, "main");
    record["owner"] = json!({"pid":2_147_483_647_i64,"bootId":"dead"});
    record["windowId"] = json!(new_summary_id().unwrap());
    store.write(&record).unwrap();
    record
}
fn capture(ack: bool) -> (SummaryPublish, Arc<Mutex<Vec<Value>>>) {
    let rows = Arc::new(Mutex::new(Vec::new()));
    let copy = rows.clone();
    (
        Arc::new(move |record| {
            copy.lock().unwrap().push(record);
            ack
        }),
        rows,
    )
}
#[test]
fn stale_dead_owner_is_recovered_once_under_original_window_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = SummaryStore::new(dir.path());
    let record = dead_window(&store, dir.path(), "session");
    let (publish, rows) = capture(true);
    for _ in 0..2 {
        SummaryRecovery::default()
            .sweep(&store, 301_001.0, &|_| true, &publish)
            .unwrap();
    }
    let rows = rows.lock().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["uuid"], record["windowId"]);
    assert_eq!(rows[0]["properties"]["summary_seq"], 2);
    assert_eq!(rows[0]["properties"]["ended_by"], "recovered");
    assert_eq!(store.read("session").unwrap().unwrap()["nextSeq"], 3);
    assert!(store.pending_uuids().unwrap().is_empty());
}
#[test]
fn fresh_heartbeat_or_live_owner_is_never_claimed() {
    let dir = tempfile::tempdir().unwrap();
    let store = SummaryStore::new(dir.path());
    dead_window(&store, dir.path(), "fresh");
    let (publish, rows) = capture(true);
    let mut recovery = SummaryRecovery::default();
    recovery
        .sweep(&store, 300_999.0, &|_| true, &publish)
        .unwrap();
    assert!(rows.lock().unwrap().is_empty());
    let mut live = dead_window(&store, dir.path(), "live");
    live["owner"] = store.owner.clone();
    store.write(&live).unwrap();
    store.remove("fresh").unwrap();
    recovery
        .sweep(&store, 1_000_000.0, &|_| true, &publish)
        .unwrap();
    assert!(rows.lock().unwrap().is_empty());
    assert!(store.read("live").unwrap().unwrap()["window"].is_object());
}
#[test]
fn pending_prevents_a_second_window_publication_after_crash() {
    let dir = tempfile::tempdir().unwrap();
    let store = SummaryStore::new(dir.path());
    let record = dead_window(&store, dir.path(), "session");
    let pending = json!({"v":2,"uuid":record["windowId"],"sessionHash":"session","owner":record["owner"],"consent":"enabled","scope":{"cwd":dir.path()},"createdAt":1000,"properties":{"turns":7}});
    store.write_pending(&pending).unwrap();
    let (publish, rows) = capture(false);
    SummaryRecovery::default()
        .sweep(&store, 301_001.0, &|_| true, &publish)
        .unwrap();
    let rows = rows.lock().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["uuid"], record["windowId"]);
    assert_eq!(rows[0]["properties"]["turns"], 7);
    assert!(store.read("session").unwrap().unwrap()["window"].is_null());
}
fn queue(store: &SummaryStore, cwd: &std::path::Path, count: usize) {
    for index in 0..count {
        store.write_pending(&json!({"v":2,"uuid":format!("00000000-0000-4000-8000-{index:012}"),"sessionHash":"s","owner":store.owner,"createdAt":1000+index,"properties":{"turns":1},"consent":"enabled","scope":{"cwd":cwd}})).unwrap();
    }
}
#[test]
fn spool_caps_drop_oldest_and_bound_sends() {
    let dir = tempfile::tempdir().unwrap();
    let store = SummaryStore::new(dir.path());
    queue(&store, dir.path(), PENDING_CAP + 30);
    let (publish, rows) = capture(false);
    let mut recovery = SummaryRecovery::default();
    recovery
        .sweep(&store, 301_001.0, &|_| true, &publish)
        .unwrap();
    assert_eq!(store.pending_uuids().unwrap().len(), PENDING_CAP);
    assert_eq!(rows.lock().unwrap().len(), RESENDS_PER_SWEEP);
    assert_eq!(recovery.dropped, 30);
    assert!(
        store
            .read_pending("00000000-0000-4000-8000-000000000000")
            .unwrap()
            .is_none()
    );
}
#[test]
fn resend_backoff_doubles_without_wall_clock_waits() {
    let dir = tempfile::tempdir().unwrap();
    let store = SummaryStore::new(dir.path());
    queue(&store, dir.path(), 1);
    let (publish, rows) = capture(false);
    let mut recovery = SummaryRecovery::default();
    for (now, count) in [
        (1000.0, 1),
        (60_999.0, 1),
        (61_000.0, 2),
        (121_000.0, 2),
        (181_000.0, 3),
    ] {
        recovery.sweep(&store, now, &|_| true, &publish).unwrap();
        assert_eq!(rows.lock().unwrap().len(), count);
    }
}
#[test]
fn dead_claims_are_restored_and_temps_removed() {
    let dir = tempfile::tempdir().unwrap();
    let store = SummaryStore::new(dir.path());
    dead_window(&store, dir.path(), "s");
    std::fs::rename(
        store.path("s"),
        store.dir.join("s.json.claimed-2147483647-dead"),
    )
    .unwrap();
    std::fs::write(store.dir.join("s.json.2147483647-dead.tmp"), "{}").unwrap();
    let (publish, rows) = capture(true);
    SummaryRecovery::default()
        .sweep(&store, 301_001.0, &|_| true, &publish)
        .unwrap();
    assert_eq!(rows.lock().unwrap().len(), 1);
    assert!(
        !list_names(&store.dir)
            .unwrap()
            .iter()
            .any(|name| name.contains("claimed-") || name.ends_with(".tmp"))
    );
}
#[test]
fn opt_out_and_expired_scope_do_not_leak_or_count() {
    let dir = tempfile::tempdir().unwrap();
    let store = SummaryStore::new(dir.path());
    dead_window(&store, dir.path(), "s");
    queue(&store, dir.path(), 1);
    let (publish, rows) = capture(true);
    let mut recovery = SummaryRecovery::default();
    recovery
        .sweep(&store, SUMMARY_TTL_MS + 2000.0, &|_| false, &publish)
        .unwrap();
    assert_eq!(recovery.dropped, 0);
    assert!(rows.lock().unwrap().is_empty());
    assert!(store.window_keys().unwrap().is_empty());
    assert!(store.pending_uuids().unwrap().is_empty());
}
#[test]
fn exclusive_continuation_does_not_replace_new_writer() {
    let dir = tempfile::tempdir().unwrap();
    let store = SummaryStore::new(dir.path());
    let first = rollup("s", 9, Value::Null, dir.path(), 1000.0, "main");
    store.write(&first).unwrap();
    let second = rollup("s", 1, Value::Null, dir.path(), 1000.0, "main");
    assert!(!store.write_window("s", &second, true).unwrap());
    assert_eq!(store.read("s").unwrap().unwrap()["nextSeq"], 9);
}

#[test]
fn recorder_is_lazy_and_persists_pending_before_advancing_and_sending() {
    use crate::{
        index::SenpiTelemetryOptions, omo_native_consent::NativeConsent,
        omo_native_session_summary::SessionSummaryRecorder,
    };
    let dir = tempfile::tempdir().unwrap();
    let consent = NativeConsent::new(
        &SenpiTelemetryOptions {
            env: Some(telemetry_core::TelemetryEnv::new()),
            ..Default::default()
        },
        Arc::new(|_| true),
    );
    consent.set_cwd(dir.path());
    let state = dir.path().to_owned();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let copy = observed.clone();
    let mut recorder = SessionSummaryRecorder::new_with_publish(
        dir.path(),
        consent,
        Arc::new(|_, _| {}),
        Arc::new(|| 1000.0),
        Arc::new(move |record| {
            let store = SummaryStore::new(&state);
            let id = record["uuid"].as_str().unwrap();
            assert!(store.read_pending(id).unwrap().is_some());
            let window = store.read("session").unwrap().unwrap();
            assert_eq!(window["nextSeq"], 1);
            assert!(window["window"].is_null());
            copy.lock().unwrap().push(record);
            true
        }),
    );
    assert!(!dir.path().join("rollups").exists());
    let event = json!({"turnIndex":1,"message":{"role":"assistant","provider":"openai","model":"gpt-5.6-sol","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":2,"cost":{"total":0.1}}}});
    for _ in 0..50 {
        recorder
            .record("session", &event, dir.path(), "main")
            .unwrap();
    }
    assert_eq!(observed.lock().unwrap().len(), 1);
    assert!(
        SummaryStore::new(dir.path())
            .pending_uuids()
            .unwrap()
            .is_empty()
    );
}
