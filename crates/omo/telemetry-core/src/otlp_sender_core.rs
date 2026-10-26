//! Rust port of `packages/telemetry-core/src/otlp-sender-core.ts`.
//!
//! The sender itself, with no opt-out check of its own. Only `start_otlp_transport` (otlp-sender.rs),
//! which checks every opt-out and consent first, loads this module; tests drive it directly.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::PoisonError;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Map;
use serde_json::Value;

use crate::otel_minimal_logs::LogRecordInput;
use crate::otel_minimal_trace::MinimalTracerProvider;
use crate::otel_providers::CreateTelemetryProvidersInput;
use crate::otel_providers::TelemetryProviders;
use crate::otel_providers::create_telemetry_providers;
use crate::otel_traceparent_fetch::OMO_TRACE_HOSTS;
use crate::otel_traceparent_fetch::create_traceparent_fetch;
use crate::otel_traceparent_fetch::url_host;
use crate::otel_turn_span::TurnSpanRecord;
use crate::otel_turn_span::TurnTracer;
use crate::otel_turn_span::create_turn_tracing;
use crate::otlp_batch_sender::BatchSender;
use crate::otlp_batch_sender::BatchSenderInput;
use crate::otlp_config::WireAttributes;
use crate::otlp_config::resolve_otlp_endpoint;
use crate::otlp_entry::purge_owned_spool;
use crate::otlp_gate::safe_consent;
use crate::otlp_http::DEFAULT_REQUEST_TIMEOUT_MS;
use crate::otlp_http::create_reqwest_fetch;
use crate::otlp_pid_alive::is_pid_alive;
use crate::otlp_sleep::CancelHandle;
use crate::otlp_sleep::abortable_sleep;
use crate::otlp_sleep::every_unref;
use crate::otlp_spool_cap::SpoolCap;
use crate::otlp_spool_dir::DropReason;
use crate::otlp_spool_dir::SPOOL_CAP_BYTES;
use crate::otlp_spool_dir::SPOOL_MAX_AGE_MS;
use crate::otlp_spool_dir::SpoolContext;
use crate::otlp_spool_dir::create_batch_id;
use crate::otlp_spool_dir::sweep_spool;
use crate::otlp_spool_file::Signal;
use crate::otlp_transport_types::Consent;
use crate::otlp_transport_types::CreateOtlpTelemetryTransportInput;
use crate::otlp_transport_types::Sampling;
use crate::otlp_wal::SpoolWriter;
use crate::otlp_wal::WalSpan;
use crate::types::TelemetryCaptureMessage;
use crate::types::TelemetryDiagnosticEvent;
use crate::types::TelemetryDiagnosticInput;
use crate::types::TelemetryError;
use crate::types::TelemetryTransport;
use crate::types::TurnSpanAttribute;
use crate::types::TurnSpanStatus;
use crate::types::TurnTracing;

const SEAL_BYTES: u64 = 200 * 1024;
const LEASE_RENEW_INTERVAL_MS: u64 = 60_000;
const SHUTDOWN_CLEANUP_RESERVE_MS: u64 = 200;
const DEFAULT_FLUSH_AT: usize = 20;
const DEFAULT_FLUSH_INTERVAL_MS: u64 = 10_000;
const DEFAULT_SWEEP_INTERVAL_MS: u64 = 60_000;
const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 2_000;

const DROP_REASONS: [(DropReason, &str); 5] = [
    (DropReason::Evicted, "evicted"),
    (DropReason::Expired, "expired"),
    (DropReason::Rejected, "rejected"),
    (DropReason::Corrupt, "corrupt"),
    (DropReason::WriteFailed, "write_failed"),
];

pub struct OtlpTelemetryTransport {
    sender: Arc<OtlpSender>,
    endpoint: String,
}

impl OtlpTelemetryTransport {
    pub fn sampling(&self) -> Sampling {
        Sampling::default()
    }

    pub fn tracer(&self) -> &Arc<MinimalTracerProvider> {
        self.sender.providers.tracer()
    }

    pub fn traces(&self) -> &dyn TurnTracing {
        &self.sender.traces
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Resolves once the startup sweep and pass have run.
    pub async fn ready(&self) {
        self.sender.ensure_started().await;
    }

    pub async fn flush(&self) {
        self.sender.flush().await;
    }

    pub async fn shutdown(&self) {
        self.sender.shutdown().await;
    }

    pub fn sweep(&self) {
        self.sender.sweep();
    }

    pub fn dropped_events(&self) -> usize {
        self.sender.dropped_events()
    }
}

impl TelemetryTransport for OtlpTelemetryTransport {
    fn capture(&self, message: &TelemetryCaptureMessage) -> Result<(), TelemetryError> {
        self.sender.capture(message);
        Ok(())
    }

    fn flush(&self) -> Option<BoxFuture<'_, Result<(), TelemetryError>>> {
        Some(
            async move {
                self.sender.flush().await;
                Ok::<(), TelemetryError>(())
            }
            .boxed(),
        )
    }

    fn shutdown(&self) -> BoxFuture<'_, Result<(), TelemetryError>> {
        async move {
            self.sender.shutdown().await;
            Ok::<(), TelemetryError>(())
        }
        .boxed()
    }

    fn traces(&self) -> Option<&dyn TurnTracing> {
        Some(&self.sender.traces)
    }
}

pub fn create_otlp_sender(input: CreateOtlpTelemetryTransportInput) -> OtlpTelemetryTransport {
    let endpoint = resolve_otlp_endpoint(input.env.as_ref(), input.endpoint.as_deref());
    let sender_endpoint = endpoint.clone();
    let consent = safe_consent(&input);
    let fetch = input.fetch.clone().unwrap_or_else(create_reqwest_fetch);
    let mut hosts: Vec<String> = OMO_TRACE_HOSTS
        .iter()
        .map(|host| (*host).to_string())
        .collect();
    if let Some(host) = url_host(&endpoint)
        && !host.is_empty() {
            hosts.push(host);
        }
    let fetch = create_traceparent_fetch(fetch, &hosts);
    let sender = Arc::new_cyclic(|weak: &Weak<OtlpSender>| {
        let on_appended: Arc<dyn Fn(&mut SpoolWriter) + Send + Sync> = {
            let weak = weak.clone();
            Arc::new(move |writer: &mut SpoolWriter| {
                if let Some(sender) = weak.upgrade() {
                    sender.on_appended(writer);
                }
            })
        };
        let providers = create_telemetry_providers(CreateTelemetryProvidersInput {
            resource: input.resource.clone(),
            scope: input.scope.clone(),
            spool_dir: input.spool_dir.clone(),
            spans: input.spans.clone(),
            boot_id: input.boot_id.clone(),
            pid: input.pid,
            clock: input.clock.clone(),
            consent: Some(Arc::clone(&consent)),
            on_appended: Some(on_appended),
            on_write_error: {
                let weak = weak.clone();
                Some(Arc::new(move |error: &io::Error, records: usize| {
                    if let Some(sender) = weak.upgrade() {
                        sender.on_write_error(error, records);
                    }
                }))
            },
        });
        let ctx = SpoolContext {
            dir: input.spool_dir.clone(),
            boot_id: providers.boot_id.clone(),
            pid: input.pid.unwrap_or_else(std::process::id),
            now_ms: input.now_ms.clone().unwrap_or_else(|| {
                Arc::new(|| {
                    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
                })
            }),
            is_pid_alive: input
                .is_pid_alive
                .clone()
                .unwrap_or_else(|| Arc::new(is_pid_alive)),
            cap_bytes: input.cap_bytes.unwrap_or(SPOOL_CAP_BYTES),
            max_age_ms: input.max_age_ms.unwrap_or(SPOOL_MAX_AGE_MS),
            on_drop: {
                let weak = weak.clone();
                Arc::new(move |reason: DropReason, records: usize| {
                    if let Some(sender) = weak.upgrade() {
                        sender.on_drop(reason, records);
                    }
                })
            },
            on_lock_left: {
                let weak = weak.clone();
                Some(Arc::new(move |error: &io::Error| {
                    if let Some(sender) = weak.upgrade() {
                        sender.report(
                            TelemetryDiagnosticEvent::TelemetrySpoolWriteFailed,
                            Some(TelemetryError::new(error.to_string())),
                        );
                    }
                }))
            },
        };
        let cap = Mutex::new(SpoolCap::new(ctx.clone()));
        let batches = Arc::new(BatchSender::new(BatchSenderInput {
            ctx: ctx.clone(),
            endpoint: sender_endpoint.clone(),
            ingest_key: input.ingest_key.clone(),
            fetch: Arc::clone(&fetch),
            request_timeout_ms: input.request_timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
            lease_renew_interval_ms: input
                .lease_renew_interval_ms
                .unwrap_or(LEASE_RENEW_INTERVAL_MS),
            sleep: input
                .sleep
                .clone()
                .unwrap_or_else(|| Arc::new(abortable_sleep)),
            random: input
                .random
                .clone()
                .unwrap_or_else(|| Arc::new(rand_unit)),
            consent: Arc::clone(&consent),
            on_accepted: {
                let weak = weak.clone();
                Arc::new(move || {
                    if let Some(sender) = weak.upgrade() {
                        sender.dropped_since_200.store(0, Ordering::SeqCst);
                    }
                })
            },
            report: {
                let weak = weak.clone();
                Arc::new(move |event: TelemetryDiagnosticEvent, error: Option<TelemetryError>| {
                    if let Some(sender) = weak.upgrade() {
                        sender.report(event, error);
                    }
                })
            },
        }));
        let traces: TurnTracer = create_turn_tracing(
            {
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .is_some_and(|sender| !sender.stopped() && (sender.consent)())
                })
            },
            {
                let weak = weak.clone();
                Arc::new(move |record: TurnSpanRecord| {
                    if let Some(sender) = weak.upgrade() {
                        sender.append_turn_span(&record);
                    }
                })
            },
        );
        OtlpSender {
            input,
            ctx,
            providers,
            consent,
            batches,
            cap,
            unreported: Mutex::new(HashMap::new()),
            timers: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
            start_lock: tokio::sync::Mutex::new(()),
            dropped_since_200: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
            traces,
            self_weak: weak.clone(),
        }
    });
    let transport = OtlpTelemetryTransport {
        sender: Arc::clone(&sender),
        endpoint,
    };
    // The startup sweep + pass runs first; a flush waits for it, so a batch sealed by an early flush is
    // never left behind by a pass that listed the spool before the seal.
    if input_auto_start(&transport.sender)
        && let Ok(handle) = tokio::runtime::Handle::try_current() {
            let sender = Arc::clone(&transport.sender);
            handle.spawn(async move {
                sender.ensure_started().await;
            });
        }
    transport
}

fn input_auto_start(sender: &OtlpSender) -> bool {
    sender.input.auto_start.unwrap_or(true)
}

fn rand_unit() -> f64 {
    f64::from(uuid::Uuid::new_v4().into_bytes()[0]) / 256.0
}

pub struct OtlpSender {
    input: CreateOtlpTelemetryTransportInput,
    ctx: SpoolContext,
    providers: TelemetryProviders,
    consent: Consent,
    batches: Arc<BatchSender>,
    cap: Mutex<SpoolCap>,
    unreported: Mutex<HashMap<DropReason, usize>>,
    timers: Mutex<Vec<CancelHandle>>,
    started: AtomicBool,
    start_lock: tokio::sync::Mutex<()>,
    dropped_since_200: AtomicUsize,
    stopped: AtomicBool,
    traces: TurnTracer,
    self_weak: Weak<OtlpSender>,
}

impl OtlpSender {
    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    fn dropped_events(&self) -> usize {
        self.dropped_since_200.load(Ordering::SeqCst)
    }

    fn report(&self, event: TelemetryDiagnosticEvent, error: Option<TelemetryError>) {
        let Some(diagnostics) = &self.input.diagnostics else {
            return;
        };
        let error_kind = error
            .as_ref()
            .map(|_| crate::types::TelemetryDiagnosticErrorKind::Error);
        diagnostics(&TelemetryDiagnosticInput {
            event,
            source: self
                .input
                .source
                .clone()
                .unwrap_or_else(|| "otlp".to_string()),
            error,
            error_kind,
            entry_kind: None,
            error_code: None,
            count: None,
        });
    }

    /// Records that never reached the spool, or were in an abandoned active file, are a counted loss.
    fn on_write_error(&self, error: &io::Error, records: usize) {
        self.report(
            TelemetryDiagnosticEvent::TelemetrySpoolWriteFailed,
            Some(TelemetryError::new(error.to_string())),
        );
        self.on_drop(DropReason::WriteFailed, records);
    }

    fn on_drop(&self, reason: DropReason, records: usize) {
        self.dropped_since_200.fetch_add(records, Ordering::SeqCst);
        let mut unreported = self.unreported.lock().unwrap_or_else(PoisonError::into_inner);
        *unreported.entry(reason).or_insert(0) += records;
    }

    fn guard<F>(&self, event: TelemetryDiagnosticEvent, run: F)
    where
        F: FnOnce(),
    {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).is_err() {
            self.report(
                event,
                Some(TelemetryError::new("telemetry guard caught a panic")),
            );
        }
    }

    fn emit(&self, name: &str, attributes: &WireAttributes) {
        let uid = attributes.get("log.record.uid");
        let uid = match uid {
            Some(Value::String(text)) => text.clone(),
            _ => self
                .input
                .create_event_id
                .as_ref()
                .map_or_else(|| create_batch_id((self.ctx.now_ms)()), |create| create()),
        };
        let mut merged = attributes.clone();
        merged.insert("log.record.uid".into(), Value::String(uid));
        self.providers.emit(&LogRecordInput {
            event_name: Some(format!("omo.{name}")),
            attributes: Some(merged),
            ..LogRecordInput::default()
        });
    }

    /// Telemetry never panics into the agent loop: a failing projection or emit is diagnosed and dropped.
    fn capture(&self, message: &TelemetryCaptureMessage) {
        if self.stopped() {
            return;
        }
        self.guard(TelemetryDiagnosticEvent::TelemetryCaptureFailed, || {
            self.capture_unguarded(message);
        });
    }

    fn capture_unguarded(&self, message: &TelemetryCaptureMessage) {
        let properties: Map<String, Value> = message.properties.clone();
        let mut projected = (self.input.project)(&message.event, &properties);
        let event_id = properties
            .get("event_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        // The record's identity keys go last, uid then session, the order the collector's fixture pins.
        let session = projected.remove("session.id");
        if let Some(event_id) = event_id {
            projected.insert("log.record.uid".into(), Value::String(event_id));
        }
        if let Some(session) = session {
            projected.insert("session.id".into(), session);
        }
        self.emit(&message.event, &projected);
    }

    /// One turn record becomes one span in the trace WAL, through the schema artifact's allowlist.
    fn append_turn_span(&self, record: &TurnSpanRecord) {
        if self.stopped() {
            return;
        }
        let mut attributes = Map::new();
        for (key, value) in &record.attributes {
            let value = match value {
                TurnSpanAttribute::String(text) => Value::String(text.clone()),
                TurnSpanAttribute::Number(number) => {
                    if *number >= crate::otlp_encode::INT64_MIN
                        && *number < crate::otlp_encode::INT64_LIMIT
                        && number.fract() == 0.0
                    {
                        Value::from(*number as i64)
                    } else {
                        Value::from(*number)
                    }
                }
                TurnSpanAttribute::Boolean(flag) => Value::Bool(*flag),
            };
            attributes.insert(key.clone(), value);
        }
        let nanos = |time: chrono::DateTime<chrono::Utc>| -> u64 {
            time.timestamp_nanos_opt()
                .and_then(|nanos| u64::try_from(nanos).ok())
                .unwrap_or(0)
        };
        let span = WalSpan {
            name: record.name.to_string(),
            trace_id: crate::otlp_encode::hex_to_bytes(&record.trace_id),
            span_id: crate::otlp_encode::hex_to_bytes(&record.span_id),
            parent_span_id: record
                .parent_span_id
                .as_ref()
                .map_or_else(Vec::new, |parent| crate::otlp_encode::hex_to_bytes(parent)),
            trace_flags: record.flags,
            kind: u32::from(record.kind).saturating_sub(1),
            start_time_unix_nano: nanos(record.started_at),
            end_time_unix_nano: nanos(record.ended_at.unwrap_or(record.started_at)),
            attributes,
            dropped_attributes_count: 0,
            status_code: match record.status {
                Some(TurnSpanStatus::Ok) => 1,
                Some(TurnSpanStatus::Error) => 2,
                None => 0,
            },
        };
        self.providers.append_wal_span(&span);
    }

    fn on_appended(&self, writer: &mut SpoolWriter) {
        let flush_at = self.input.flush_at.unwrap_or(DEFAULT_FLUSH_AT);
        if writer.records < flush_at && writer.bytes < SEAL_BYTES {
            return;
        }
        let batch_id = create_batch_id((self.ctx.now_ms)());
        let bytes = writer.bytes;
        let signal = writer.signal();
        self.guard(TelemetryDiagnosticEvent::TelemetrySpoolWriteFailed, || {
            if let Ok(Some(_sealed)) = writer.seal(&batch_id) {
                let _sealed_bytes = self
                    .cap
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .sealed(signal, &batch_id, bytes);
            }
        });
        if let Some(sender) = self.self_weak.upgrade()
            && let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    sender.batches.pump().await;
                });
            }
    }

    fn seal_providers(&self) {
        let batch_id = create_batch_id((self.ctx.now_ms)());
        let logs = self.providers.log_bytes();
        let log_signal = Signal::Logs;
        self.guard(TelemetryDiagnosticEvent::TelemetrySpoolWriteFailed, || {
            if let Ok(Some(_sealed)) = self.providers.seal_logs(&batch_id) {
                let _sealed_bytes = self
                    .cap
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .sealed(log_signal, &batch_id, logs);
            }
        });
        let spans = self.providers.span_bytes();
        let span_signal = Signal::Traces;
        self.guard(TelemetryDiagnosticEvent::TelemetrySpoolWriteFailed, || {
            if let Ok(Some(_sealed)) = self.providers.seal_spans(&batch_id) {
                let _sealed_bytes = self
                    .cap
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .sealed(span_signal, &batch_id, spans);
            }
        });
    }

    fn report_drops(&self) {
        for (reason, name) in DROP_REASONS {
            let count = {
                let mut unreported =
                    self.unreported.lock().unwrap_or_else(PoisonError::into_inner);
                let count = unreported.get(&reason).copied().unwrap_or(0);
                if count > 0 {
                    unreported.insert(reason, 0);
                }
                count
            };
            if count == 0 {
                continue;
            }
            let mut attributes = WireAttributes::new();
            attributes.insert("omo.dropped.count".into(), count.into());
            attributes.insert("omo.dropped.reason".into(), Value::String(name.to_string()));
            self.emit("client_dropped", &attributes);
        }
    }

    /// Consent withdrawn mid-run: close this process's files, then purge the whole spool.
    fn purge(&self) {
        self.guard(TelemetryDiagnosticEvent::TelemetrySpoolWriteFailed, || {
            let source = self.input.source.clone().unwrap_or_else(|| "otlp".to_string());
            match purge_owned_spool(
                &self.ctx.dir,
                &self.input.spool_root,
                self.input.diagnostics.as_ref(),
                &source,
            ) {
                Ok(crate::otlp_spool_purge::PurgeOutcome::Refused) => {
                    self.report(TelemetryDiagnosticEvent::TelemetrySpoolPurgeRefused, None);
                }
                Ok(_) => {}
                Err(error) => self.report(
                    TelemetryDiagnosticEvent::TelemetrySpoolWriteFailed,
                    Some(TelemetryError::new(error.to_string())),
                ),
            }
        });
    }

    fn sweep(&self) {
        self.guard(TelemetryDiagnosticEvent::TelemetrySpoolSweepFailed, || {
            let swept = sweep_spool(&self.ctx).and_then(|()| {
                self.cap
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .enforce()
                    .map(|_total| ())
            });
            if let Err(error) = swept {
                self.report(
                    TelemetryDiagnosticEvent::TelemetrySpoolSweepFailed,
                    Some(TelemetryError::new(error.to_string())),
                );
            }
        });
    }

    async fn ensure_started(self: &Arc<Self>) {
        if self.started.load(Ordering::SeqCst) {
            return;
        }
        let _guard = self.start_lock.lock().await;
        if self.started.load(Ordering::SeqCst) {
            return;
        }
        self.start().await;
        self.started.store(true, Ordering::SeqCst);
    }

    async fn start(self: &Arc<Self>) {
        let flush_interval = self
            .input
            .flush_interval_ms
            .unwrap_or(DEFAULT_FLUSH_INTERVAL_MS);
        let sweep_interval = self
            .input
            .sweep_interval_ms
            .unwrap_or(DEFAULT_SWEEP_INTERVAL_MS);
        let flush_timer = {
            let sender = Arc::clone(self);
            every_unref(
                move || {
                    let sender = Arc::clone(&sender);
                    async move {
                        sender.flush().await;
                    }
                },
                flush_interval,
            )
        };
        let sweep_timer = {
            let sender = Arc::clone(self);
            every_unref(
                move || {
                    let sender = Arc::clone(&sender);
                    async move {
                        sender.sweep_tick().await;
                    }
                },
                sweep_interval,
            )
        };
        {
            let mut timers = self.timers.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(timer) = flush_timer {
                timers.push(timer);
            }
            if let Some(timer) = sweep_timer {
                timers.push(timer);
            }
        }
        self.sweep_tick().await;
    }

    /// Start and every sweep interval: recover what dead or stalled processes left, cap the spool,
    /// replay; purge if consent ended.
    async fn sweep_tick(self: &Arc<Self>) {
        if self.stopped() {
            return;
        }
        if !(self.consent)() {
            self.purge();
            return;
        }
        self.sweep();
        self.batches.pump().await;
    }

    // Boxed so the timer started by `ensure_started` can hold a `Send` future that
    // re-enters `flush` without forming an infinitely sized async state machine.
    fn flush(self: &Arc<Self>) -> BoxFuture<'_, ()> {
        async move {
            if self.stopped() {
                return;
            }
            if !(self.consent)() {
                self.purge();
                return;
            }
            self.ensure_started().await;
            if self.stopped() {
                return;
            }
            self.batches.new_cycle();
            self.report_drops();
            self.seal_providers();
            self.batches.pump().await;
        }
        .boxed()
    }

    async fn shutdown(self: &Arc<Self>) {
        if self.stopped() {
            return;
        }
        {
            let mut timers = self.timers.lock().unwrap_or_else(PoisonError::into_inner);
            for timer in timers.drain(..) {
                timer.cancel();
            }
        }
        // The cap is measured from this call: the deadline is armed before the flush runs, so the
        // flush's synchronous seal and claim work counts against the cap instead of delaying its start.
        let cap = self
            .input
            .shutdown_timeout_ms
            .unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT_MS);
        let flush_budget = cap.saturating_sub(SHUTDOWN_CLEANUP_RESERVE_MS.min(cap / 10));
        let _ = tokio::time::timeout(Duration::from_millis(flush_budget), self.flush()).await;
        self.stopped.store(true, Ordering::SeqCst);
        self.batches
            .stop(Duration::from_millis(cap.saturating_sub(flush_budget)))
            .await;
        self.providers.shutdown();
    }
}
