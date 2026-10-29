//! Support for the OTLP transport tests: a spool root, a recording fetch, and a transport builder.

use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use futures::FutureExt;
use serde_json::Map;
use serde_json::Value;
use telemetry_core::AbortSignal;
use telemetry_core::Consent;
use telemetry_core::CreateOtlpTelemetryTransportInput;
use telemetry_core::InstrumentationScope;
use telemetry_core::IsPidAlive;
use telemetry_core::NowMs;
use telemetry_core::OtlpFetch;
use telemetry_core::OtlpRequest;
use telemetry_core::OtlpResponse;
use telemetry_core::OtlpTelemetryTransport;
use telemetry_core::OtlpWireProjection;
use telemetry_core::ResourceAttributes;
use telemetry_core::SpanAllowlist;
use telemetry_core::TelemetryCaptureMessage;
use telemetry_core::TelemetryDiagnostics;
use telemetry_core::TelemetryEnv;
use telemetry_core::TelemetryTransport;
use telemetry_core::WireAttributes;
use telemetry_core::create_otlp_sender;
use telemetry_core::is_wire_attribute_key;
use telemetry_core::otlp_proto::collector_logs::ExportLogsServiceRequest;
use telemetry_core::otlp_proto::collector_logs::decode_export_logs_service_request;
use telemetry_core::otlp_proto::collector_trace::ExportTraceServiceRequest;
use telemetry_core::otlp_proto::collector_trace::decode_export_trace_service_request;
use telemetry_core::otlp_proto::logs::LogRecord;
use telemetry_core::otlp_proto::trace::Span;

pub const TEST_SCOPE_NAME: &str = "omo-senpi";
pub const TEST_SCOPE_VERSION: &str = "3.17.0";

pub fn test_scope() -> InstrumentationScope {
    InstrumentationScope {
        name: TEST_SCOPE_NAME.to_string(),
        version: TEST_SCOPE_VERSION.to_string(),
    }
}

/// Test stand-in for a schema artifact's span allowlist.
pub fn test_spans() -> SpanAllowlist {
    let mut spans = SpanAllowlist::new();
    spans.insert(
        "omo.turn".to_string(),
        vec![
            "gen_ai.request.model".to_string(),
            "n".to_string(),
            "nan".to_string(),
        ],
    );
    spans
}

/// Test stand-in for a schema artifact's projection: `$session_id` maps to the semconv `session.id`,
/// and every other primitive that passes the key rule becomes `omo.<name>`.
pub fn test_wire_projection() -> OtlpWireProjection {
    Arc::new(|_event: &str, properties: &Map<String, Value>| {
        let mut attributes = WireAttributes::new();
        if let Some(Value::String(session)) = properties.get("$session_id")
            && !session.is_empty() {
                attributes.insert("session.id".to_string(), Value::String(session.clone()));
        }
        for (key, value) in properties {
            if !is_wire_attribute_key(key) {
                continue;
            }
            match value {
                Value::String(_) | Value::Bool(_) => {
                    attributes.insert(format!("omo.{key}"), value.clone());
                }
                Value::Number(number) if number.as_f64().is_some_and(f64::is_finite) => {
                    attributes.insert(format!("omo.{key}"), value.clone());
                }
                _ => {}
            }
        }
        attributes
    })
}

pub fn test_resource(version: &str) -> ResourceAttributes {
    let mut resource = ResourceAttributes::new();
    resource.insert("service.name".into(), "omo-senpi".into());
    resource.insert("service.version".into(), version.into());
    resource.insert(
        "service.instance.id".into(),
        "0123456789abcdef0123456789abcdef".into(),
    );
    resource.insert("omo.install.hash".into(), "ab".repeat(32).into());
    resource.insert("telemetry.sdk.name".into(), "omo-otel-minimal".into());
    resource.insert("omo.schema.version".into(), 3.into());
    resource
}

pub struct SpoolRoot {
    pub dir: PathBuf,
    _temp: tempfile::TempDir,
}

pub fn create_spool_root() -> SpoolRoot {
    let temp = tempfile::tempdir().expect("temp dir");
    let dir = temp.path().join("telemetry-spool");
    SpoolRoot { dir, _temp: temp }
}

pub fn spool_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub _wire_bytes: usize,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub type Responder = Arc<dyn Fn(&str, usize) -> OtlpResponse + Send + Sync>;

pub struct RecordingFetch {
    pub calls: Arc<Mutex<Vec<RecordedRequest>>>,
    pub fetch: OtlpFetch,
}

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = flate2::read::GzDecoder::new(bytes);
    let mut out = Vec::new();
    let _read = decoder.read_to_end(&mut out);
    out
}

pub fn recording_fetch(respond: Responder) -> RecordingFetch {
    let calls: Arc<Mutex<Vec<RecordedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let fetch: OtlpFetch = Arc::new(move |url: String, request: OtlpRequest| {
        let headers = request.headers.clone();
        let compressed = headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("content-encoding") && value.eq_ignore_ascii_case("gzip")
        });
        let body = if compressed {
            gunzip(&request.body)
        } else {
            request.body.clone()
        };
        let call = {
            let mut calls = recorded.lock().unwrap();
            calls.push(RecordedRequest {
                url: url.clone(),
                headers,
                body,
                _wire_bytes: request.body.len(),
            });
            calls.len()
        };
        let response = respond(&url, call);
        async move { Ok::<OtlpResponse, String>(response) }.boxed()
    });
    RecordingFetch { calls, fetch }
}

pub fn ok_fetch() -> RecordingFetch {
    recording_fetch(Arc::new(|_url, _call| OtlpResponse {
        status: 200,
        retry_after: None,
        body: Vec::new(),
    }))
}

#[derive(Clone, Default)]
pub struct TransportOverrides {
    pub consent: Option<Consent>,
    pub fetch: Option<OtlpFetch>,
    pub env: Option<TelemetryEnv>,
    pub endpoint: Option<String>,
    pub now_ms: Option<NowMs>,
    pub diagnostics: Option<TelemetryDiagnostics>,
    pub auto_start: Option<bool>,
    pub project: Option<OtlpWireProjection>,
    pub spans: Option<SpanAllowlist>,
    pub is_pid_alive: Option<IsPidAlive>,
    pub pid: Option<u32>,
    pub boot_id: Option<String>,
    pub cap_bytes: Option<u64>,
    pub flush_at: Option<usize>,
}

pub fn make_transport(dir: &Path, overrides: TransportOverrides) -> OtlpTelemetryTransport {
    let spool_root = dir.parent().map(Path::to_path_buf).unwrap_or_else(|| dir.to_path_buf());
    create_otlp_sender(CreateOtlpTelemetryTransportInput {
        product_env_prefix: "OMO_TEST".to_string(),
        ingest_key: "test-ingest-key".to_string(),
        spool_dir: dir.to_path_buf(),
        spool_root,
        resource: test_resource("1.1.0"),
        scope: test_scope(),
        env: overrides.env,
        endpoint: Some(
            overrides
                .endpoint
                .unwrap_or_else(|| "https://collector.test".to_string()),
        ),
        project: overrides.project.unwrap_or_else(test_wire_projection),
        spans: overrides.spans.unwrap_or_else(test_spans),
        consent: overrides.consent,
        diagnostics: overrides.diagnostics,
        source: None,
        fetch: overrides.fetch,
        request_timeout_ms: None,
        lease_renew_interval_ms: None,
        // No test ever waits on a real retry pause.
        sleep: Some(Arc::new(|_ms: u64, _abort: AbortSignal| {
            futures::future::ready(()).boxed()
        })),
        random: Some(Arc::new(|| 0.0)),
        now_ms: overrides.now_ms,
        clock: None,
        is_pid_alive: overrides.is_pid_alive,
        pid: overrides.pid,
        boot_id: overrides.boot_id,
        create_event_id: None,
        auto_start: Some(overrides.auto_start.unwrap_or(false)),
        flush_at: overrides.flush_at,
        flush_interval_ms: None,
        sweep_interval_ms: None,
        shutdown_timeout_ms: None,
        cap_bytes: overrides.cap_bytes,
        max_age_ms: None,
    })
}

pub fn capture(transport: &OtlpTelemetryTransport, event: &str, properties: Value) {
    let message = TelemetryCaptureMessage {
        distinct_id: "unused-on-otlp".to_string(),
        event: event.to_string(),
        uuid: None,
        timestamp: None,
        properties: match properties {
            Value::Object(map) => map,
            other => panic!("expected object, got {other}"),
        },
    };
    transport.capture(&message).expect("capture");
}

pub fn decode_logs(body: &[u8]) -> ExportLogsServiceRequest {
    decode_export_logs_service_request(body).expect("valid logs request")
}

pub fn decode_traces(body: &[u8]) -> ExportTraceServiceRequest {
    decode_export_trace_service_request(body).expect("valid traces request")
}

pub fn log_records_of(request: &ExportLogsServiceRequest) -> Vec<&LogRecord> {
    request
        .resource_logs
        .iter()
        .flat_map(|resource_logs| resource_logs.scope_logs.iter())
        .flat_map(|scope_logs| scope_logs.log_records.iter())
        .collect()
}

pub fn spans_of(request: &ExportTraceServiceRequest) -> Vec<&Span> {
    request
        .resource_spans
        .iter()
        .flat_map(|resource_spans| resource_spans.scope_spans.iter())
        .flat_map(|scope_spans| scope_spans.spans.iter())
        .collect()
}

pub fn attribute_map(
    attributes: &[telemetry_core::otlp_proto::common::KeyValue],
) -> Map<String, Value> {
    use telemetry_core::otlp_proto::common::AnyValue;
    let mut map = Map::new();
    for pair in attributes {
        let value = match &pair.value {
            AnyValue::String(text) => Value::String(text.clone()),
            AnyValue::Bool(flag) => Value::Bool(*flag),
            AnyValue::Int(number) => Value::from(*number),
            AnyValue::Double(number) => Value::from(*number),
            _ => Value::Null,
        };
        map.insert(pair.key.clone(), value);
    }
    map
}
