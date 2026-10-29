//! Ports of the process-level fixtures `otlp-kill-child.ts` and `otlp-silent-server.ts`, expressed as
//! Rust test support: another process's active spool file and a collector that never answers.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use futures::FutureExt;
use serde_json::Map;
use serde_json::Value;
use telemetry_core::AbortSignal;
use telemetry_core::DEAD_OWNER_IDLE_MS;
use telemetry_core::DropReason;
use telemetry_core::PostDeps;
use telemetry_core::PostOutcome;
use telemetry_core::Signal;
use telemetry_core::SpoolContext;
use telemetry_core::SpoolEntry;
use telemetry_core::SpoolWriter;
use telemetry_core::SpoolWriterInput;
use telemetry_core::WalLogRecord;
use telemetry_core::WalLogRecordProcessor;
use telemetry_core::create_reqwest_fetch;
use telemetry_core::encode_header;
use telemetry_core::encode_resource;
use telemetry_core::encode_scope;
use telemetry_core::otlp_proto::common::encode_instrumentation_scope;
use telemetry_core::otlp_proto::resource::encode_resource_message;
use telemetry_core::post_with_retry;
use telemetry_core::sweep_spool;

use crate::otlp_support::create_spool_root;
use crate::otlp_support::spool_names;
use crate::otlp_support::test_resource;
use crate::otlp_support::test_scope;

/// The events `otlp-kill-child.ts` emits before it is killed.
fn spawn_crashing_writer(dir: &Path, pid: u32, boot_id: &str) -> io::Result<()> {
    let resource = encode_resource_message(&encode_resource(&test_resource("1.1.0")));
    let scope = encode_instrumentation_scope(&encode_scope(&test_scope()));
    let writer = SpoolWriter::new(SpoolWriterInput {
        dir: dir.to_path_buf(),
        signal: Signal::Logs,
        pid,
        boot_id: boot_id.to_string(),
        header: encode_header(Signal::Logs, &resource, &scope),
        consent: Arc::new(|| true),
        on_error: Arc::new(|_error: &io::Error, _records: usize| {}),
        write: None,
        truncate: None,
    });
    let mut processor = WalLogRecordProcessor::new(writer, Box::new(|_writer: &mut SpoolWriter| {}));
    for index in 1..=7u32 {
        let mut attributes = Map::new();
        attributes.insert("log.record.uid".into(), Value::from(index));
        attributes.insert("omo.seq".into(), Value::from(index));
        processor.on_emit(&WalLogRecord {
            event_name: Some("omo.feature_used".to_string()),
            time_unix_nano: u64::from(index),
            observed_time_unix_nano: u64::from(index),
            severity_number: 0,
            attributes,
            dropped_attributes_count: 0,
            trace_id: Vec::new(),
            span_id: Vec::new(),
            trace_flags: 0,
        });
    }
    processor.shutdown();
    Ok(())
}

fn context(dir: &Path, now_ms: u64, alive: bool) -> SpoolContext {
    SpoolContext {
        dir: dir.to_path_buf(),
        boot_id: "d".repeat(32),
        pid: std::process::id(),
        now_ms: Arc::new(move || now_ms),
        is_pid_alive: Arc::new(move |_pid| alive),
        cap_bytes: 5 * 1024 * 1024,
        max_age_ms: 14 * 24 * 60 * 60 * 1000,
        on_drop: Arc::new(|_reason: DropReason, _records: usize| {}),
        on_lock_left: None,
    }
}

#[test]
fn a_dead_writers_idle_active_file_is_sealed_by_the_sweep() {
    let root = create_spool_root();
    spawn_crashing_writer(&root.dir, 4_242, &"e".repeat(32)).expect("crashing writer");
    let active = spool_names(&root.dir);
    assert_eq!(active.len(), 1);
    assert!(active[0].starts_with("active-logs-4242-"), "got {active:?}");

    // The writer died and its file has been idle past the dead-owner threshold.
    let old = std::time::SystemTime::now()
        - std::time::Duration::from_millis(DEAD_OWNER_IDLE_MS + 1_000);
    filetime::set_file_mtime(
        root.dir.join(&active[0]),
        filetime::FileTime::from_system_time(old),
    )
    .expect("age the active file");

    sweep_spool(&context(
        &root.dir,
        u64::try_from(chrono::Utc::now().timestamp_millis()).expect("clock"),
        false,
    ))
    .expect("sweep");

    let names = spool_names(&root.dir);
    assert_eq!(names.len(), 1);
    assert!(names[0].starts_with("batch-logs-"), "got {names:?}");
}

#[tokio::test]
async fn a_collector_that_never_answers_ends_the_attempt_at_the_request_timeout() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind silent collector");
    let port = listener.local_addr().expect("addr").port();
    let requests = Arc::new(Mutex::new(0usize));
    let seen = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        loop {
            let Ok((stream, _peer)) = listener.accept().await else {
                return;
            };
            *seen.lock().unwrap() += 1;
            // Accept the request and never answer it.
            std::mem::forget(stream);
        }
    });

    let deps = PostDeps {
        fetch: create_reqwest_fetch(),
        sleep: Arc::new(|_ms: u64, _abort: AbortSignal| {
            futures::future::ready(()).boxed()
        }),
        random: Arc::new(|| 0.0),
        endpoint: format!("http://127.0.0.1:{port}"),
        ingest_key: "test-ingest-key".to_string(),
        abort: AbortSignal::new(),
        request_timeout_ms: 50,
        before_attempt: None,
    };

    let outcome = post_with_retry(&deps, Signal::Logs, b"body").await;

    assert_eq!(outcome, PostOutcome::RetryLater);
    assert!(*requests.lock().unwrap() >= 1);
    server.abort();
}

#[test]
fn the_spool_entry_names_stay_parseable_for_the_purge() {
    let root = create_spool_root();
    spawn_crashing_writer(&root.dir, 7, &"f".repeat(32)).expect("crashing writer");
    let names = spool_names(&root.dir);
    assert_eq!(names.len(), 1);
    assert!(matches!(
        telemetry_core::parse_spool_name(&names[0]),
        SpoolEntry::Active { .. }
    ));
    assert!(telemetry_core::is_spool_file_name(&names[0]));
}
