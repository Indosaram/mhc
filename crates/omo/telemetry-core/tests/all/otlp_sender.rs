//! Ports of `otlp-sender.test.ts` and the sender half of `otlp-entry.test.ts`.

use std::sync::Arc;
use std::sync::Mutex;

use serde_json::json;
use telemetry_core::CreateOtlpTelemetryTransportInput;
use telemetry_core::OtlpResponse;
use telemetry_core::TelemetryDiagnosticEvent;
use telemetry_core::TelemetryDiagnosticInput;
use telemetry_core::create_otlp_telemetry_transport;
use telemetry_core::otlp_proto::write_message;
use telemetry_core::otlp_proto::write_string;
use telemetry_core::otlp_proto::write_uint;

use crate::otlp_support::TransportOverrides;
use crate::otlp_support::attribute_map;
use crate::otlp_support::capture;
use crate::otlp_support::create_spool_root;
use crate::otlp_support::decode_logs;
use crate::otlp_support::log_records_of;
use crate::otlp_support::make_transport;
use crate::otlp_support::ok_fetch;
use crate::otlp_support::recording_fetch;
use crate::otlp_support::spool_names;
use crate::otlp_support::test_resource;
use crate::otlp_support::test_scope;
use crate::otlp_support::test_spans;
use crate::otlp_support::test_wire_projection;

fn seq_of(body: &[u8]) -> Vec<i64> {
    log_records_of(&decode_logs(body))
        .into_iter()
        .map(|record| {
            attribute_map(&record.attributes)
                .get("omo.seq")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(-1)
        })
        .collect()
}

fn entry_input(dir: &std::path::Path, env: &[(&str, &str)]) -> CreateOtlpTelemetryTransportInput {
    CreateOtlpTelemetryTransportInput {
        product_env_prefix: "OMO_TEST".to_string(),
        ingest_key: "test-ingest-key".to_string(),
        spool_dir: dir.to_path_buf(),
        spool_root: dir.parent().map(std::path::Path::to_path_buf).unwrap_or_else(|| dir.to_path_buf()),
        resource: test_resource("1.1.0"),
        scope: test_scope(),
        env: Some(
            env.iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        ),
        endpoint: None,
        project: test_wire_projection(),
        spans: test_spans(),
        consent: None,
        diagnostics: None,
        source: None,
        fetch: None,
        request_timeout_ms: None,
        lease_renew_interval_ms: None,
        sleep: None,
        random: None,
        now_ms: None,
        clock: None,
        is_pid_alive: None,
        pid: None,
        boot_id: None,
        create_event_id: None,
        auto_start: Some(false),
        flush_at: None,
        flush_interval_ms: None,
        sweep_interval_ms: None,
        shutdown_timeout_ms: None,
        cap_bytes: None,
        max_age_ms: None,
    }
}

#[tokio::test]
async fn batches_of_twenty_are_sealed_and_sent_in_emit_order_and_the_rest_waits_for_flush() {
    let root = create_spool_root();
    let recorder = ok_fetch();
    let transport = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(recorder.fetch.clone()),
            ..TransportOverrides::default()
        },
    );

    for index in 0..45 {
        capture(&transport, "feature_used", json!({ "seq": index }));
    }
    transport.ready().await;
    transport.flush().await;

    let calls = recorder.calls.lock().unwrap().clone();
    let batches: Vec<Vec<i64>> = calls.iter().map(|call| seq_of(&call.body)).collect();
    let sizes: Vec<usize> = batches.iter().map(Vec::len).collect();
    assert_eq!(sizes, vec![20, 20, 5]);
    let flat: Vec<i64> = batches.into_iter().flatten().collect();
    assert_eq!(flat, (0..45).collect::<Vec<i64>>());
    assert!(calls
        .iter()
        .all(|call| call.url == "https://collector.test/v1/logs"));
    assert_eq!(calls[0].header("x-omo-ingest-key"), Some("test-ingest-key"));
    assert_eq!(
        calls[0].header("content-type"),
        Some("application/x-protobuf")
    );
    assert!(spool_names(&root.dir).is_empty());
}

#[tokio::test]
async fn a_body_over_eight_kilobytes_is_gzip_encoded_and_a_small_body_is_not() {
    let root = create_spool_root();
    let recorder = ok_fetch();
    let transport = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(recorder.fetch.clone()),
            ..TransportOverrides::default()
        },
    );

    capture(&transport, "feature_used", json!({ "feature": "small" }));
    transport.flush().await;
    for index in 0..19 {
        capture(
            &transport,
            "feature_used",
            json!({
                "blob": "x".repeat(64),
                "more": "y".repeat(64),
                "seq": index,
                "a": "z".repeat(64),
                "b": "w".repeat(64),
            }),
        );
    }
    transport.flush().await;

    let calls = recorder.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].header("content-encoding"), None);
    assert_eq!(calls[1].header("content-encoding"), Some("gzip"));
    assert!(calls[1].body.len() > 8 * 1024);
    assert_eq!(log_records_of(&decode_logs(&calls[1].body)).len(), 19);
}

#[tokio::test]
async fn the_otlp_exporter_variables_are_ignored_and_omo_telemetry_endpoint_overrides() {
    let root = create_spool_root();
    let ignored = make_transport(
        &root.dir,
        TransportOverrides {
            env: Some(
                [
                    (
                        "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
                        "http://evil.test".to_string(),
                    ),
                    (
                        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT".to_string(),
                        "http://evil.test".to_string(),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
            ..TransportOverrides::default()
        },
    );
    let overridden = make_transport(
        &root.dir,
        TransportOverrides {
            env: Some(
                [(
                    "OMO_TELEMETRY_ENDPOINT".to_string(),
                    "https://staging.test/".to_string(),
                )]
                .into_iter()
                .collect(),
            ),
            ..TransportOverrides::default()
        },
    );
    let defaulted = create_otlp_telemetry_transport(entry_input(&root.dir, &[]))
        .expect("consented transport");

    assert_eq!(ignored.endpoint(), "https://collector.test");
    assert_eq!(overridden.endpoint(), "https://staging.test");
    assert_eq!(defaulted.endpoint(), "https://t.omo.dev");
}

#[tokio::test]
async fn otel_sdk_disabled_do_not_track_and_the_product_opt_out_build_nothing() {
    let root = create_spool_root();
    for env in [
        vec![("OTEL_SDK_DISABLED", "TRUE")],
        vec![("DO_NOT_TRACK", "1")],
        vec![("OMO_TEST_SEND_ANONYMOUS_TELEMETRY", "0")],
    ] {
        let transport = create_otlp_telemetry_transport(entry_input(&root.dir, &env));
        assert!(transport.is_none(), "env {env:?} must disable the transport");
    }
    assert!(!root.dir.exists());
}

#[tokio::test]
async fn a_200_carrying_partial_success_settles_the_batch_and_diagnoses_the_rejected_count() {
    let root = create_spool_root();
    let mut partial = Vec::new();
    write_uint(&mut partial, 1, 1);
    write_string(&mut partial, 2, "rejected 1 of 2: #1 unknown_event");
    let mut body = Vec::new();
    write_message(&mut body, 1, &partial);
    let recorder = recording_fetch(Arc::new(move |_url, _call| OtlpResponse {
        status: 200,
        retry_after: None,
        body: body.clone(),
    }));
    let diagnostics: Arc<Mutex<Vec<TelemetryDiagnosticInput>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&diagnostics);
    let transport = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(recorder.fetch.clone()),
            diagnostics: Some(Arc::new(move |input: &TelemetryDiagnosticInput| {
                sink.lock().unwrap().push(input.clone());
            })),
            ..TransportOverrides::default()
        },
    );

    capture(&transport, "feature_used", json!({ "feature": "a" }));
    capture(&transport, "unknown", json!({ "feature": "b" }));
    transport.flush().await;
    transport.flush().await;

    assert_eq!(recorder.calls.lock().unwrap().len(), 1);
    assert!(spool_names(&root.dir).is_empty());
    let events: Vec<TelemetryDiagnosticEvent> = diagnostics
        .lock()
        .unwrap()
        .iter()
        .map(|input| input.event)
        .collect();
    assert_eq!(
        events,
        vec![TelemetryDiagnosticEvent::TelemetryOtlpPartialSuccess]
    );
    assert_eq!(transport.dropped_events(), 0);
}

#[tokio::test]
async fn the_transport_always_records_the_raw_turn_rate() {
    let root = create_spool_root();
    let transport = make_transport(&root.dir, TransportOverrides::default());
    assert_eq!(transport.sampling().raw_turn_rate, 1);
}

#[tokio::test]
async fn an_auto_started_transport_sends_the_batch_of_a_flush_that_raced_its_startup_pass() {
    let root = create_spool_root();
    let recorder = ok_fetch();
    let transport = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(recorder.fetch.clone()),
            auto_start: Some(true),
            ..TransportOverrides::default()
        },
    );

    capture(&transport, "feature_used", json!({ "feature": "a" }));
    transport.flush().await;

    assert_eq!(recorder.calls.lock().unwrap().len(), 1);
    assert!(spool_names(&root.dir)
        .iter()
        .all(|name| !name.starts_with("batch-")));
    transport.shutdown().await;
}

#[tokio::test]
async fn consent_that_starts_panicking_mid_run_purges_the_spool_and_resolves() {
    let root = create_spool_root();
    let recorder = ok_fetch();
    let throwing = Arc::new(Mutex::new(false));
    let flag = Arc::clone(&throwing);
    let transport = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(recorder.fetch.clone()),
            consent: Some(Arc::new(move || !*flag.lock().unwrap())),
            ..TransportOverrides::default()
        },
    );
    capture(&transport, "feature_used", json!({ "feature": "a" }));

    *throwing.lock().unwrap() = true;
    transport.flush().await;
    transport.shutdown().await;

    assert!(recorder.calls.lock().unwrap().is_empty());
    assert!(spool_names(&root.dir)
        .iter()
        .all(|name| !name.ends_with(".otlp")));
}
