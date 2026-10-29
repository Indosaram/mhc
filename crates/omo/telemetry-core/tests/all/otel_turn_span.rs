//! Port of `otel-turn-span.test.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;

use serde_json::json;
use telemetry_core::SpanAllowlist;
use telemetry_core::TurnSpanAttribute;
use telemetry_core::TurnSpanAttributes;
use telemetry_core::TurnSpanStart;
use telemetry_core::TurnSpanStatus;

use crate::otlp_support::TransportOverrides;
use crate::otlp_support::attribute_map;
use crate::otlp_support::capture;
use crate::otlp_support::create_spool_root;
use crate::otlp_support::decode_logs;
use crate::otlp_support::decode_traces;
use crate::otlp_support::log_records_of;
use crate::otlp_support::make_transport;
use crate::otlp_support::ok_fetch;
use crate::otlp_support::spans_of;

fn text(value: &str) -> TurnSpanAttribute {
    TurnSpanAttribute::String(value.to_string())
}

fn turn_attributes() -> TurnSpanAttributes {
    BTreeMap::from([
        ("gen_ai.operation.name".to_string(), text("chat")),
        ("session.id".to_string(), text("session-hash")),
        ("omo.session.role".to_string(), text("main")),
        ("omo.turn.index".to_string(), TurnSpanAttribute::Number(0.0)),
    ])
}

fn turn_spans() -> SpanAllowlist {
    let mut spans = SpanAllowlist::new();
    spans.insert(
        "omo.turn".to_string(),
        [
            "gen_ai.operation.name",
            "session.id",
            "omo.session.role",
            "omo.turn.index",
            "gen_ai.response.model",
        ]
        .map(str::to_string)
        .to_vec(),
    );
    spans
}

fn start(
    transport: &telemetry_core::OtlpTelemetryTransport,
    parent: Option<String>,
) -> telemetry_core::TurnSpanHandle {
    transport
        .traces()
        .start_turn(TurnSpanStart {
            attributes: turn_attributes(),
            parent_traceparent: parent,
        })
        .expect("turn span started")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sent_spans(
    recorder: &crate::otlp_support::RecordingFetch,
) -> Vec<telemetry_core::otlp_proto::trace::Span> {
    recorder
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.url.ends_with("/v1/traces"))
        .flat_map(|call| {
            spans_of(&decode_traces(&call.body))
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect()
}

fn sent_logs(
    recorder: &crate::otlp_support::RecordingFetch,
) -> Vec<(String, String, String)> {
    recorder
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.url.ends_with("/v1/logs"))
        .flat_map(|call| {
            log_records_of(&decode_logs(&call.body))
                .into_iter()
                .map(|record| {
                    (
                        record.event_name.clone(),
                        hex(&record.trace_id),
                        hex(&record.span_id),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test]
async fn one_turn_span_is_exported_and_only_the_in_turn_event_carries_its_exact_pair() {
    let root = create_spool_root();
    let recorder = ok_fetch();
    let transport = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(recorder.fetch.clone()),
            spans: Some(turn_spans()),
            ..TransportOverrides::default()
        },
    );
    let turn = start(&transport, None);

    turn.run(|| capture(&transport, "turn_completed", json!({ "turn_index": 0 })));
    turn.set_attributes(BTreeMap::from([(
        "gen_ai.response.model".to_string(),
        text("gpt-5.6-sol"),
    )]));
    turn.end(TurnSpanStatus::Ok);
    capture(&transport, "session_summary", json!({ "turns": 1 }));

    transport.flush().await;

    assert_eq!(turn.trace_id.len(), 32);
    assert_eq!(turn.span_id.len(), 16);
    assert_eq!(
        turn.traceparent,
        format!("00-{}-{}-01", turn.trace_id, turn.span_id)
    );

    let spans = sent_spans(&recorder);
    assert_eq!(spans.len(), 1);
    let span = &spans[0];
    assert_eq!(span.name, "omo.turn");
    assert_eq!(hex(&span.trace_id), turn.trace_id);
    assert_eq!(hex(&span.span_id), turn.span_id);
    assert!(span.parent_span_id.is_empty());
    assert_eq!(span.kind, 1);
    assert_eq!(span.status.as_ref().map(|status| status.code), Some(1));
    let attributes = attribute_map(&span.attributes);
    assert_eq!(attributes.get("gen_ai.operation.name"), Some(&json!("chat")));
    assert_eq!(attributes.get("session.id"), Some(&json!("session-hash")));
    assert_eq!(attributes.get("omo.session.role"), Some(&json!("main")));
    assert_eq!(attributes.get("omo.turn.index"), Some(&json!(0)));
    assert_eq!(
        attributes.get("gen_ai.response.model"),
        Some(&json!("gpt-5.6-sol"))
    );
    assert!(span.end_time_unix_nano >= span.start_time_unix_nano);

    assert_eq!(
        sent_logs(&recorder),
        vec![
            (
                "omo.turn_completed".to_string(),
                turn.trace_id.clone(),
                turn.span_id.clone()
            ),
            ("omo.session_summary".to_string(), String::new(), String::new()),
        ]
    );
}

#[tokio::test]
async fn a_failing_turn_ends_with_the_error_status_once() {
    let root = create_spool_root();
    let recorder = ok_fetch();
    let transport = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(recorder.fetch.clone()),
            spans: Some(turn_spans()),
            ..TransportOverrides::default()
        },
    );
    let turn = start(&transport, None);

    turn.run(|| capture(&transport, "turn_completed", json!({ "turn_index": 0 })));
    turn.end(TurnSpanStatus::Error);
    turn.end(TurnSpanStatus::Ok);
    transport.flush().await;

    let statuses: Vec<i32> = sent_spans(&recorder)
        .iter()
        .map(|span| span.status.as_ref().map_or(0, |status| status.code))
        .collect();
    assert_eq!(statuses, vec![2]);
    let logs = sent_logs(&recorder);
    assert_eq!(logs[0].1, turn.trace_id);
    assert_eq!(logs[0].2, turn.span_id);
}

#[tokio::test]
async fn an_inherited_traceparent_with_the_sampled_flag_clear_still_records_the_span() {
    let root = create_spool_root();
    let recorder = ok_fetch();
    let transport = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(recorder.fetch.clone()),
            spans: Some(turn_spans()),
            ..TransportOverrides::default()
        },
    );
    let parent_trace = "0af7651916cd43dd8448eb211c80319c";
    let parent_span = "b7ad6b7169203331";
    let turn = start(
        &transport,
        Some(format!("00-{parent_trace}-{parent_span}-00")),
    );
    turn.run(|| capture(&transport, "turn_completed", json!({ "turn_index": 0 })));
    turn.end(TurnSpanStatus::Ok);
    transport.flush().await;

    let spans = sent_spans(&recorder);
    assert_eq!(spans.len(), 1);
    assert_eq!(hex(&spans[0].trace_id), parent_trace);
    assert_eq!(hex(&spans[0].parent_span_id), parent_span);
    assert_eq!(spans[0].flags, 1);
    assert_ne!(hex(&spans[0].span_id), parent_span);
    let logs = sent_logs(&recorder);
    assert_eq!(logs[0].1, parent_trace);
    assert_eq!(logs[0].2, turn.span_id);
}

#[tokio::test]
async fn a_malformed_inherited_traceparent_makes_the_turn_a_root_span() {
    let root = create_spool_root();
    let recorder = ok_fetch();
    let transport = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(recorder.fetch.clone()),
            spans: Some(turn_spans()),
            ..TransportOverrides::default()
        },
    );
    let turn = start(
        &transport,
        Some("00-00000000000000000000000000000000-b7ad6b7169203331-01".to_string()),
    );
    turn.end(TurnSpanStatus::Ok);
    transport.flush().await;

    let spans = sent_spans(&recorder);
    assert_eq!(spans.len(), 1);
    assert!(spans[0].parent_span_id.is_empty());
}

#[tokio::test]
async fn two_concurrent_turns_never_share_an_id_and_each_event_keeps_its_own_pair() {
    let root = create_spool_root();
    let recorder = ok_fetch();
    let transport = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(recorder.fetch.clone()),
            spans: Some(turn_spans()),
            ..TransportOverrides::default()
        },
    );
    let first = start(&transport, None);
    let second = start(&transport, None);
    // Upstream: packages/telemetry-core/src/otel-turn-span.test.ts lines 168-196
    let (open1, gate1) = tokio::sync::oneshot::channel::<()>();
    let (open2, gate2) = tokio::sync::oneshot::channel::<()>();

    let first_run = first.run_async(async {
        let _ = gate1.await;
        capture(&transport, "feature_used", json!({ "feature": "f0" }));
        let _ = open2.send(());
    });
    let second_run = second.run_async(async {
        let _ = open1.send(());
        let _ = gate2.await;
        capture(&transport, "feature_used", json!({ "feature": "f1" }));
    });
    let joined = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(first_run, second_run);
    })
    .await;
    assert!(joined.is_ok(), "both turns must finish");
    first.end(TurnSpanStatus::Ok);
    second.end(TurnSpanStatus::Ok);
    transport.flush().await;

    assert_ne!(first.span_id, second.span_id);
    let span_ids: Vec<String> = sent_spans(&recorder)
        .iter()
        .map(|span| hex(&span.span_id))
        .collect();
    assert_eq!(span_ids.len(), 2);
    let logs = sent_logs(&recorder);
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].2, first.span_id);
    assert_eq!(logs[1].2, second.span_id);
}

#[tokio::test]
async fn withdrawn_consent_starts_no_span_and_sends_nothing() {
    let root = create_spool_root();
    let recorder = ok_fetch();
    let consented = Arc::new(Mutex::new(true));
    let flag = Arc::clone(&consented);
    let transport = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(recorder.fetch.clone()),
            spans: Some(turn_spans()),
            consent: Some(Arc::new(move || *flag.lock().unwrap())),
            ..TransportOverrides::default()
        },
    );
    *consented.lock().unwrap() = false;

    let turn = transport.traces().start_turn(TurnSpanStart {
        attributes: turn_attributes(),
        parent_traceparent: None,
    });
    *consented.lock().unwrap() = true;
    transport.flush().await;

    assert!(turn.is_none());
    assert!(recorder.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_shut_down_transport_starts_no_span() {
    let root = create_spool_root();
    let transport = make_transport(
        &root.dir,
        TransportOverrides {
            spans: Some(turn_spans()),
            ..TransportOverrides::default()
        },
    );
    transport.shutdown().await;

    let turn = transport.traces().start_turn(TurnSpanStart {
        attributes: turn_attributes(),
        parent_traceparent: None,
    });
    assert!(turn.is_none());
}
