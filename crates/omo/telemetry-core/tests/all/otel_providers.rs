//! Port of `otel-providers.test.ts`.

use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use telemetry_core::CreateTelemetryProvidersInput;
use telemetry_core::LogRecordInput;
use telemetry_core::SpanAllowlist;
use telemetry_core::SpanContext;
use telemetry_core::SpanOptions;
use telemetry_core::SpanStatusCode;
use telemetry_core::SpoolRequest;
use telemetry_core::create_telemetry_providers;
use telemetry_core::get_active_trace_context;
use telemetry_core::read_spool_request;
use telemetry_core::with_span_context_sync;

use crate::otlp_support::attribute_map;
use crate::otlp_support::create_spool_root;
use crate::otlp_support::decode_logs;
use crate::otlp_support::decode_traces;
use crate::otlp_support::log_records_of;
use crate::otlp_support::spans_of;
use crate::otlp_support::test_resource;
use crate::otlp_support::test_scope;

const BATCH_ID: &str = "018f0000-0000-7000-8000-000000000000";

fn turn_spans() -> SpanAllowlist {
    let mut spans = SpanAllowlist::new();
    spans.insert(
        "omo.turn".to_string(),
        ["gen_ai.request.model", "n", "nan"]
            .map(str::to_string)
            .to_vec(),
    );
    spans
}

fn attributes(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

fn providers(dir: &std::path::Path) -> telemetry_core::TelemetryProviders {
    create_telemetry_providers(CreateTelemetryProvidersInput {
        resource: test_resource("1.1.0"),
        scope: test_scope(),
        spool_dir: dir.to_path_buf(),
        spans: turn_spans(),
        boot_id: None,
        pid: None,
        clock: None,
        consent: None,
        on_appended: None,
        on_write_error: None,
    })
}

fn seal_logs(providers: &telemetry_core::TelemetryProviders, dir: &std::path::Path) -> Vec<u8> {
    let sealed = providers.seal_logs(BATCH_ID).expect("seal").expect("sealed");
    match read_spool_request(&dir.join(sealed)).expect("readable spool file") {
        SpoolRequest::Ok { body, .. } => body,
        SpoolRequest::Corrupt { .. } => panic!("corrupt spool file"),
    }
}

fn seal_spans(providers: &telemetry_core::TelemetryProviders, dir: &std::path::Path) -> Vec<u8> {
    let sealed = providers
        .seal_spans(BATCH_ID)
        .expect("seal")
        .expect("sealed");
    match read_spool_request(&dir.join(sealed)).expect("readable spool file") {
        SpoolRequest::Ok { body, .. } => body,
        SpoolRequest::Corrupt { .. } => panic!("corrupt spool file"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[tokio::test]
async fn a_log_emitted_inside_an_active_span_carries_that_spans_trace_and_span_id() {
    let root = create_spool_root();
    let created = providers(&root.dir);
    let span = created
        .tracer()
        .start_span("omo.turn", &SpanOptions::default(), None);
    let context: SpanContext = span.span_context();
    with_span_context_sync(context.clone(), || {
        created.emit(&LogRecordInput {
            event_name: Some("omo.turn_completed".to_string()),
            attributes: Some(attributes(&[("log.record.uid", json!("a"))])),
            ..LogRecordInput::default()
        });
        let active = get_active_trace_context().expect("active trace context");
        assert_eq!(active.trace_id, context.trace_id);
        assert_eq!(active.span_id, context.span_id);
    });
    span.end();

    let traces = decode_traces(&seal_spans(&created, &root.dir));
    let logs = decode_logs(&seal_logs(&created, &root.dir));
    let encoded_span = spans_of(&traces).first().copied().cloned().expect("one span");
    let record = log_records_of(&logs)
        .first()
        .copied()
        .cloned()
        .expect("one record");
    assert_eq!(encoded_span.name, "omo.turn");
    assert_eq!(record.event_name, "omo.turn_completed");
    assert_eq!(hex(&record.trace_id), hex(&encoded_span.trace_id));
    assert_eq!(hex(&record.span_id), hex(&encoded_span.span_id));
}

#[tokio::test]
async fn a_sealed_file_carries_the_writers_resource_and_scope_from_its_header() {
    let root = create_spool_root();
    let created = providers(&root.dir);
    created.emit(&LogRecordInput {
        event_name: Some("omo.x".to_string()),
        attributes: Some(attributes(&[("omo.a", json!(1))])),
        ..LogRecordInput::default()
    });

    let request = decode_logs(&seal_logs(&created, &root.dir));
    let resource_logs = request.resource_logs.first().expect("one resource");
    let resource = resource_logs.resource.as_ref().expect("resource");
    assert_eq!(
        attribute_map(&resource.attributes).get("service.version"),
        Some(&json!("1.1.0"))
    );
    let scope = resource_logs.scope_logs[0]
        .scope
        .as_ref()
        .expect("scope")
        .clone();
    assert_eq!(scope.name, "omo-senpi");
    let record = log_records_of(&request)
        .first()
        .copied()
        .cloned()
        .expect("record");
    assert_eq!(
        attribute_map(&record.attributes),
        attributes(&[("omo.a", json!(1))])
    );
}

#[tokio::test]
async fn only_the_status_code_travels_when_a_span_ends() {
    let root = create_spool_root();
    let created = providers(&root.dir);
    let span = created
        .tracer()
        .start_span("omo.turn", &SpanOptions::default(), None);
    span.set_status(SpanStatusCode::Error);
    span.end();

    let traces = decode_traces(&seal_spans(&created, &root.dir));
    let encoded = spans_of(&traces).first().copied().cloned().expect("one span");
    let status = encoded.status.as_ref().expect("status");
    assert_eq!(status.code, 2);
    assert_eq!(status.message, "");
}
