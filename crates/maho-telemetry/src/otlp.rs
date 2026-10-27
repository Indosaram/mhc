//! OTLP/HTTP protobuf export of one agent run: the GenAI span tree (port of oh-my-pi
//! `packages/agent/src/telemetry.ts`: `invoke_agent`, `chat {model}`, `execute_tool {tool}`), the
//! run metrics and the run log (port of oh-my-pi `telemetry-export-otlp.ts`
//! `AgentMetricRecorder`/`emitRunSummaryLog`, with the `omp.` prefix renamed `mhc.`), and a
//! background push thread. Content capture is always off.
use crate::proto::{AnyValue, Attributes, Message, put_attributes};
use crate::record::UsageRecord;
use crate::settings::{ExportConfig, Signal, SignalTarget};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub const SCOPE_NAME: &str = "maho-telemetry";
pub const SCOPE_VERSION: &str = env!("CARGO_PKG_VERSION");
const SPAN_KIND_INTERNAL: u64 = 1;
const SPAN_KIND_CLIENT: u64 = 3;
const STATUS_ERROR: u64 = 2;
const TEMPORALITY_DELTA: u64 = 1;
const SEVERITY_INFO: u64 = 9;

/// Nanoseconds since the Unix epoch.
pub fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        })
}

fn ms_to_ns(ms: i64) -> u64 {
    u64::try_from(ms).unwrap_or(0).saturating_mul(1_000_000)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    let mut filled = 0;
    while filled < N {
        let chunk = uuid::Uuid::new_v4().into_bytes();
        let take = (N - filled).min(chunk.len());
        out[filled..filled + take].copy_from_slice(&chunk[..take]);
        filled += take;
    }
    out
}

/// One chat call inside a run.
#[derive(Clone, Debug)]
pub struct ChatCall {
    pub record: UsageRecord,
    pub span_id: [u8; 8],
    pub start_ns: u64,
    pub end_ns: u64,
}

/// One tool execution inside a run.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub name: String,
    pub call_id: String,
    pub span_id: [u8; 8],
    pub start_ns: u64,
    pub end_ns: u64,
    pub error: bool,
}

/// What one agent run produced (oh-my-pi `run-collector`, reduced).
#[derive(Clone, Debug)]
pub struct Run {
    pub trace_id: [u8; 16],
    pub root_span: [u8; 8],
    pub session_id: String,
    pub start_ns: u64,
    pub end_ns: u64,
    pub chats: Vec<ChatCall>,
    pub tools: Vec<ToolCall>,
    open_tools: BTreeMap<String, (String, u64)>,
}

impl Run {
    pub fn start(session_id: &str, start_ns: u64) -> Self {
        Self {
            trace_id: random_bytes(),
            root_span: random_bytes(),
            session_id: session_id.to_owned(),
            start_ns,
            end_ns: start_ns,
            chats: Vec::new(),
            tools: Vec::new(),
            open_tools: BTreeMap::new(),
        }
    }

    /// Adds a chat call that finished at `now`; its span starts `durationMs` earlier.
    pub fn chat(&mut self, record: &UsageRecord, now: u64) {
        let start_ns = record
            .duration_ms
            .map_or(now, |duration| now.saturating_sub(ms_to_ns(duration)));
        self.chats.push(ChatCall {
            record: record.clone(),
            span_id: random_bytes(),
            start_ns,
            end_ns: now,
        });
    }

    pub fn tool_start(&mut self, call_id: &str, name: &str, now: u64) {
        self.open_tools
            .insert(call_id.to_owned(), (name.to_owned(), now));
    }

    pub fn tool_end(&mut self, call_id: &str, name: &str, error: bool, now: u64) {
        let (name, start_ns) = self
            .open_tools
            .remove(call_id)
            .unwrap_or_else(|| (name.to_owned(), now));
        self.tools.push(ToolCall {
            name,
            call_id: call_id.to_owned(),
            span_id: random_bytes(),
            start_ns,
            end_ns: now,
            error,
        });
    }

    pub fn finish(&mut self, now: u64) {
        self.end_ns = now.max(self.start_ns);
        let open = std::mem::take(&mut self.open_tools);
        for (call_id, (name, start_ns)) in open {
            self.tools.push(ToolCall {
                name,
                call_id,
                span_id: random_bytes(),
                start_ns,
                end_ns: self.end_ns,
                error: false,
            });
        }
    }

    fn input_tokens(&self) -> u64 {
        self.chats.iter().map(|c| c.record.input).sum()
    }
    fn output_tokens(&self) -> u64 {
        self.chats.iter().map(|c| c.record.output).sum()
    }
    fn total_tokens(&self) -> u64 {
        self.chats.iter().map(|c| c.record.total_tokens).sum()
    }
    fn cost(&self) -> f64 {
        self.chats
            .iter()
            .filter_map(|c| c.record.cost.map(|cost| cost.total))
            .sum()
    }
}

fn attrs(pairs: Vec<(&str, AnyValue)>) -> Attributes {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn resource_message(config: &ExportConfig) -> Message {
    let mut resource = Message::new();
    let attributes: Attributes = config
        .resource
        .iter()
        .map(|(key, value)| (key.clone(), AnyValue::from(value.as_str())))
        .collect();
    put_attributes(&mut resource, 1, &attributes);
    resource
}

fn scope_message() -> Message {
    let mut scope = Message::new();
    scope.string(1, SCOPE_NAME).string(2, SCOPE_VERSION);
    scope
}

/// Wraps scoped items into `Export*ServiceRequest { resource_* = 1 { resource = 1; scope_* = 2 {
/// scope = 1; items = 2 } } }` (the same layout for spans, metrics and logs).
fn export_request(config: &ExportConfig, items: &[Message]) -> Vec<u8> {
    let mut scoped = Message::new();
    scoped.message(1, &scope_message());
    for item in items {
        scoped.message(2, item);
    }
    let mut resource_items = Message::new();
    resource_items
        .message(1, &resource_message(config))
        .message(2, &scoped);
    let mut request = Message::new();
    request.message(1, &resource_items);
    request.into_bytes()
}

struct SpanSpec<'a> {
    trace_id: &'a [u8; 16],
    span_id: &'a [u8; 8],
    parent: Option<&'a [u8; 8]>,
    name: &'a str,
    kind: u64,
    start_ns: u64,
    end_ns: u64,
    attributes: Attributes,
    error: bool,
}

fn span(spec: SpanSpec<'_>) -> Message {
    let mut span = Message::new();
    span.bytes(1, spec.trace_id).bytes(2, spec.span_id);
    if let Some(parent) = spec.parent {
        span.bytes(4, parent);
    }
    span.string(5, spec.name)
        .varint(6, spec.kind)
        .fixed64(7, spec.start_ns)
        .fixed64(8, spec.end_ns.max(spec.start_ns));
    put_attributes(&mut span, 9, &spec.attributes);
    if spec.error {
        let mut status = Message::new();
        status.varint(3, STATUS_ERROR);
        span.message(15, &status);
    }
    span
}

/// `ExportTraceServiceRequest` for one run.
pub fn traces_request(config: &ExportConfig, run: &Run) -> Vec<u8> {
    let mut spans = vec![span(SpanSpec {
        trace_id: &run.trace_id,
        span_id: &run.root_span,
        parent: None,
        name: "invoke_agent mhc",
        kind: SPAN_KIND_INTERNAL,
        start_ns: run.start_ns,
        end_ns: run.end_ns,
        attributes: attrs(vec![
            ("gen_ai.operation.name", "invoke_agent".into()),
            ("gen_ai.agent.name", "mhc".into()),
            ("gen_ai.conversation.id", run.session_id.as_str().into()),
            ("mhc.agent.step_count", (run.chats.len() as u64).into()),
        ]),
        error: false,
    })];
    for chat in &run.chats {
        let record = &chat.record;
        let mut attributes = attrs(vec![
            ("gen_ai.operation.name", "chat".into()),
            ("gen_ai.provider.name", record.provider.as_str().into()),
            ("gen_ai.request.model", record.model.as_str().into()),
            (
                "gen_ai.response.finish_reasons",
                AnyValue::Array(vec![record.stop_reason.as_str().into()]),
            ),
            ("gen_ai.usage.input_tokens", record.input.into()),
            ("gen_ai.usage.output_tokens", record.output.into()),
            (
                "gen_ai.usage.cache_read.input_tokens",
                record.cache_read.into(),
            ),
            (
                "gen_ai.usage.cache_creation.input_tokens",
                record.cache_write.into(),
            ),
        ]);
        if record.reasoning > 0 {
            attributes.push((
                "gen_ai.usage.reasoning.output_tokens".into(),
                record.reasoning.into(),
            ));
        }
        if let Some(ttft) = record.ttft_ms {
            attributes.push((
                "gen_ai.response.time_to_first_chunk".into(),
                (ttft as f64 / 1000.0).into(),
            ));
        }
        if let Some(cost) = record.cost {
            attributes.push(("mhc.gen_ai.cost.usd".into(), cost.total.into()));
        }
        spans.push(span(SpanSpec {
            trace_id: &run.trace_id,
            span_id: &chat.span_id,
            parent: Some(&run.root_span),
            name: &format!("chat {}", record.model),
            kind: SPAN_KIND_CLIENT,
            start_ns: chat.start_ns,
            end_ns: chat.end_ns,
            attributes,
            error: record.failed(),
        }));
    }
    for tool in &run.tools {
        spans.push(span(SpanSpec {
            trace_id: &run.trace_id,
            span_id: &tool.span_id,
            parent: Some(&run.root_span),
            name: &format!("execute_tool {}", tool.name),
            kind: SPAN_KIND_INTERNAL,
            start_ns: tool.start_ns,
            end_ns: tool.end_ns,
            attributes: attrs(vec![
                ("gen_ai.operation.name", "execute_tool".into()),
                ("gen_ai.tool.name", tool.name.as_str().into()),
                ("gen_ai.tool.call.id", tool.call_id.as_str().into()),
            ]),
            error: tool.error,
        }));
    }
    export_request(config, &spans)
}

fn metric(name: &str, description: &str, unit: &str) -> Message {
    let mut metric = Message::new();
    metric
        .string(1, name)
        .string(2, description)
        .string(3, unit);
    metric
}

/// `Sum { data_points = 1; aggregation_temporality = 2; is_monotonic = 3 }` with delta points.
fn counter(
    name: &str,
    description: &str,
    unit: &str,
    points: &[(Attributes, f64)],
    window: (u64, u64),
    integer: bool,
) -> Option<Message> {
    if points.is_empty() {
        return None;
    }
    let mut sum = Message::new();
    for (attributes, value) in points {
        let mut point = Message::new();
        put_attributes(&mut point, 7, attributes);
        point.fixed64(2, window.0).fixed64(3, window.1);
        if integer {
            // Counts are whole numbers by construction.
            point.fixed64(6, *value as i64 as u64);
        } else {
            point.double(4, *value);
        }
        sum.message(1, &point);
    }
    sum.varint(2, TEMPORALITY_DELTA).varint(3, 1);
    let mut metric = metric(name, description, unit);
    metric.message(7, &sum);
    Some(metric)
}

/// `Histogram` with one single-bucket point per observation.
fn histogram(
    name: &str,
    description: &str,
    unit: &str,
    points: &[(Attributes, f64)],
    window: (u64, u64),
) -> Option<Message> {
    if points.is_empty() {
        return None;
    }
    let mut histogram = Message::new();
    for (attributes, value) in points {
        let mut point = Message::new();
        put_attributes(&mut point, 9, attributes);
        point
            .fixed64(2, window.0)
            .fixed64(3, window.1)
            .fixed64(4, 1)
            .double(5, *value)
            .packed_fixed64(6, &[1])
            .double(11, *value)
            .double(12, *value);
        histogram.message(1, &point);
    }
    histogram.varint(2, TEMPORALITY_DELTA);
    let mut metric = metric(name, description, unit);
    metric.message(9, &histogram);
    Some(metric)
}

fn grouped(entries: impl IntoIterator<Item = (Attributes, f64)>) -> Vec<(Attributes, f64)> {
    let mut out: Vec<(Attributes, f64)> = Vec::new();
    for (attributes, value) in entries {
        if let Some(slot) = out.iter_mut().find(|(existing, _)| *existing == attributes) {
            slot.1 += value;
        } else {
            out.push((attributes, value));
        }
    }
    out
}

/// `ExportMetricsServiceRequest` for one run (delta temporality over the run window).
pub fn metrics_request(config: &ExportConfig, run: &Run) -> Vec<u8> {
    let window = (run.start_ns, run.end_ns.max(run.start_ns));
    let mut tokens = Vec::new();
    for chat in &run.chats {
        let record = &chat.record;
        let base = attrs(vec![
            ("gen_ai.operation.name", "chat".into()),
            ("gen_ai.provider.name", record.provider.as_str().into()),
            ("gen_ai.request.model", record.model.as_str().into()),
        ]);
        for (kind, value) in [
            ("input", record.input),
            ("output", record.output),
            ("total", record.total_tokens),
            ("cache_read_input", record.cache_read),
            ("cache_write_input", record.cache_write),
            ("reasoning_output", record.reasoning),
        ] {
            if value > 0 {
                let mut attributes = base.clone();
                attributes.push(("gen_ai.token.type".into(), kind.into()));
                tokens.push((attributes, value as f64));
            }
        }
    }
    let cost = grouped(run.chats.iter().filter_map(|chat| {
        let total = chat.record.cost.map(|cost| cost.total)?;
        (total > 0.0).then(|| {
            (
                attrs(vec![
                    ("gen_ai.operation.name", "chat".into()),
                    ("gen_ai.provider.name", chat.record.provider.as_str().into()),
                    ("gen_ai.request.model", chat.record.model.as_str().into()),
                ]),
                total,
            )
        })
    }));
    let calls = grouped(run.chats.iter().map(|chat| {
        (
            attrs(vec![(
                "gen_ai.response.finish_reason",
                chat.record.stop_reason.as_str().into(),
            )]),
            1.0,
        )
    }));
    let tool_calls = grouped(run.tools.iter().map(|tool| {
        (
            attrs(vec![
                ("gen_ai.tool.name", tool.name.as_str().into()),
                (
                    "mhc.tool.status",
                    if tool.error { "error" } else { "ok" }.into(),
                ),
            ]),
            1.0,
        )
    }));
    let errors = grouped(
        run.chats
            .iter()
            .filter(|chat| chat.record.failed())
            .map(|_| (attrs(vec![("error.type", "chat".into())]), 1.0))
            .chain(
                run.tools
                    .iter()
                    .filter(|tool| tool.error)
                    .map(|_| (attrs(vec![("error.type", "tool".into())]), 1.0)),
            ),
    );
    let metrics: Vec<Message> = [
        histogram(
            "gen_ai.client.token.usage",
            "Token usage reported by GenAI chat calls.",
            "{token}",
            &tokens,
            window,
        ),
        counter(
            "mhc.agent.chat.cost.estimated_usd",
            "Estimated USD cost for completed chat calls.",
            "USD",
            &cost,
            window,
            false,
        ),
        counter(
            "mhc.agent.runs",
            "Completed agent runs.",
            "{run}",
            &[(Attributes::new(), 1.0)],
            window,
            true,
        ),
        counter(
            "mhc.agent.chat.calls",
            "Chat calls completed inside agent runs.",
            "{call}",
            &calls,
            window,
            true,
        ),
        counter(
            "mhc.agent.tool.calls",
            "Tool calls completed inside agent runs.",
            "{call}",
            &tool_calls,
            window,
            true,
        ),
        counter(
            "mhc.agent.errors",
            "Errors observed in chat and tool execution.",
            "{error}",
            &errors,
            window,
            true,
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    export_request(config, &metrics)
}

fn unique_join<'a>(values: impl Iterator<Item = &'a str>) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for value in values {
        if !seen.contains(&value) {
            seen.push(value);
        }
    }
    seen.join(",")
}

/// `ExportLogsServiceRequest` with the run summary record.
pub fn logs_request(config: &ExportConfig, run: &Run) -> Vec<u8> {
    let mut record = Message::new();
    record
        .fixed64(1, run.end_ns)
        .varint(2, SEVERITY_INFO)
        .string(3, "INFO")
        .message(5, &AnyValue::from("agent run completed").encode());
    let attributes = attrs(vec![
        ("mhc.agent.step_count", (run.chats.len() as u64).into()),
        ("mhc.agent.chats.total", (run.chats.len() as u64).into()),
        ("mhc.agent.tools.total", (run.tools.len() as u64).into()),
        (
            "mhc.agent.tools.error",
            (run.tools.iter().filter(|t| t.error).count() as u64).into(),
        ),
        ("mhc.agent.usage.input_tokens", run.input_tokens().into()),
        ("mhc.agent.usage.output_tokens", run.output_tokens().into()),
        ("mhc.agent.usage.total_tokens", run.total_tokens().into()),
        ("mhc.agent.cost.estimated_usd", run.cost().into()),
        (
            "mhc.agent.coverage.models_used",
            unique_join(run.chats.iter().map(|c| c.record.model.as_str())).into(),
        ),
        (
            "mhc.agent.coverage.tools_invoked",
            unique_join(run.tools.iter().map(|t| t.name.as_str())).into(),
        ),
        ("process.pid", u64::from(std::process::id()).into()),
    ]);
    put_attributes(&mut record, 6, &attributes);
    record
        .bytes(9, &run.trace_id)
        .bytes(10, &run.root_span)
        .fixed64(11, now_ns())
        .string(12, "mhc.agent.run.completed");
    export_request(config, &[record])
}

/// The requests one finished run sends: `(target, body)` per enabled signal.
pub fn run_payloads(config: &ExportConfig, run: &Run) -> Vec<(SignalTarget, Vec<u8>)> {
    config
        .targets
        .iter()
        .filter(|target| target.signal != Signal::Logs || config.info_logs)
        .map(|target| {
            let body = match target.signal {
                Signal::Traces => traces_request(config, run),
                Signal::Metrics => metrics_request(config, run),
                Signal::Logs => logs_request(config, run),
            };
            (target.clone(), body)
        })
        .collect()
}

/// Sends one request body.
pub trait Transport: Send {
    fn post(
        &mut self,
        url: &str,
        headers: &[(String, String)],
        body: Vec<u8>,
    ) -> Result<(), String>;
}

/// OTLP over HTTP(S) with reqwest on a private current-thread runtime.
pub struct HttpTransport {
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
}

impl HttpTransport {
    pub fn new() -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self { runtime, client })
    }
}

impl Transport for HttpTransport {
    fn post(
        &mut self,
        url: &str,
        headers: &[(String, String)],
        body: Vec<u8>,
    ) -> Result<(), String> {
        let mut request = self
            .client
            .post(url)
            .header("content-type", "application/x-protobuf")
            .body(body);
        for (key, value) in headers {
            request = request.header(key.as_str(), value.as_str());
        }
        let status = self
            .runtime
            .block_on(request.send())
            .map_err(|error| format!("{url}: {}", error.without_url()))?
            .status();
        if status.is_success() {
            Ok(())
        } else {
            Err(format!("{url}: HTTP {}", status.as_u16()))
        }
    }
}

struct Job {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

type Pending = Arc<(Mutex<usize>, Condvar)>;

/// A background thread that posts queued requests in order.
pub struct Exporter {
    sender: Option<Sender<Job>>,
    pending: Pending,
    failed: Arc<AtomicBool>,
    report: Arc<dyn Fn(&str) + Send + Sync>,
}

fn worker(
    make: Box<dyn FnOnce() -> Result<Box<dyn Transport>, String> + Send>,
    jobs: Receiver<Job>,
    pending: Pending,
    failed: Arc<AtomicBool>,
    report: Arc<dyn Fn(&str) + Send + Sync>,
) {
    let mut transport = match make() {
        Ok(transport) => Some(transport),
        Err(error) => {
            if !failed.swap(true, Ordering::SeqCst) {
                report(&format!("telemetry export unavailable: {error}"));
            }
            None
        }
    };
    for job in jobs {
        if let Some(transport) = transport.as_mut()
            && let Err(error) = transport.post(&job.url, &job.headers, job.body)
            && !failed.swap(true, Ordering::SeqCst)
        {
            report(&format!("telemetry export failed: {error}"));
        }
        let (count, ready) = &*pending;
        let mut count = count.lock().unwrap_or_else(PoisonError::into_inner);
        *count = count.saturating_sub(1);
        ready.notify_all();
    }
}

impl Exporter {
    /// Starts the push thread; `make` builds the transport on that thread. `report` receives at
    /// most one failure line.
    pub fn spawn(
        make: Box<dyn FnOnce() -> Result<Box<dyn Transport>, String> + Send>,
        report: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Self {
        let (sender, jobs) = channel();
        let pending: Pending = Arc::default();
        let failed = Arc::new(AtomicBool::new(false));
        let thread_pending = Arc::clone(&pending);
        let thread_failed = Arc::clone(&failed);
        let thread_report = Arc::clone(&report);
        let spawned = std::thread::Builder::new()
            .name("maho-telemetry".into())
            .spawn(move || worker(make, jobs, thread_pending, thread_failed, thread_report));
        let sender = match spawned {
            Ok(_) => Some(sender),
            Err(error) => {
                report(&format!("telemetry export unavailable: {error}"));
                None
            }
        };
        Self {
            sender,
            pending,
            failed,
            report,
        }
    }

    /// HTTP exporter.
    pub fn http(report: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        Self::spawn(
            Box::new(|| HttpTransport::new().map(|t| Box::new(t) as Box<dyn Transport>)),
            report,
        )
    }

    /// Queues one request.
    pub fn send(&self, target: &SignalTarget, body: Vec<u8>) {
        let Some(sender) = &self.sender else { return };
        let (count, _) = &*self.pending;
        *count.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        let job = Job {
            url: target.url.clone(),
            headers: target.headers.clone(),
            body,
        };
        if sender.send(job).is_err() {
            *count.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
            if !self.failed.swap(true, Ordering::SeqCst) {
                (self.report)("telemetry export stopped");
            }
        }
    }

    /// Waits until the queue is empty or `timeout` passes; `true` when everything was sent.
    pub fn flush(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (count, ready) = &*self.pending;
        let mut count = count.lock().unwrap_or_else(PoisonError::into_inner);
        while *count > 0 {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            count = ready
                .wait_timeout(count, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Cost;

    fn config(signals: &[Signal]) -> ExportConfig {
        ExportConfig {
            targets: signals
                .iter()
                .map(|signal| SignalTarget {
                    signal: *signal,
                    url: format!("http://collector{}", signal.path()),
                    headers: vec![("authorization".into(), "secret".into())],
                })
                .collect(),
            resource: vec![("service.name".into(), "mhc".into())],
            info_logs: true,
        }
    }

    fn sample_run() -> Run {
        let mut run = Run::start("session-1", 1_000);
        let record = UsageRecord {
            provider: "offline".into(),
            model: "m1".into(),
            stop_reason: "stop".into(),
            input: 10,
            output: 5,
            total_tokens: 15,
            timestamp: 2,
            duration_ms: Some(1),
            ttft_ms: Some(1),
            cost: Some(Cost {
                total: 0.5,
                ..Cost::default()
            }),
            ..UsageRecord::default()
        };
        run.chat(&record, 5_000_000_000);
        run.tool_start("call-1", "write", 2_000);
        run.tool_end("call-1", "write", true, 3_000);
        run.tool_start("call-2", "read", 3_500);
        run.finish(4_000);
        run
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    }

    #[test]
    fn run_collects_chats_and_tools_and_closes_open_tools() {
        let run = sample_run();
        assert_eq!(run.chats[0].end_ns, 5_000_000_000);
        assert_eq!(run.chats[0].start_ns, 4_999_000_000);
        assert_eq!(run.tools.len(), 2);
        assert!(run.tools[0].error);
        assert_eq!(run.tools[1].end_ns, 4_000);
    }

    #[test]
    fn traces_carry_the_span_tree_and_no_content() {
        let config = config(&[Signal::Traces]);
        let body = traces_request(&config, &sample_run());
        for needle in [
            "invoke_agent mhc",
            "chat m1",
            "execute_tool write",
            "gen_ai.usage.input_tokens",
            "gen_ai.conversation.id",
            "session-1",
            "service.name",
            SCOPE_NAME,
        ] {
            assert!(contains(&body, needle), "{needle}");
        }
        assert!(!contains(&body, "secret"));
        // ExportTraceServiceRequest starts with field 1 (resource_spans), wire type LEN.
        assert_eq!(body[0], 0x0a);
    }

    #[test]
    fn metrics_and_logs_use_the_ported_names() {
        let config = config(&[Signal::Metrics, Signal::Logs]);
        let run = sample_run();
        let metrics = metrics_request(&config, &run);
        for needle in [
            "gen_ai.client.token.usage",
            "gen_ai.token.type",
            "mhc.agent.chat.cost.estimated_usd",
            "mhc.agent.runs",
            "mhc.agent.tool.calls",
            "mhc.agent.errors",
        ] {
            assert!(contains(&metrics, needle), "{needle}");
        }
        let logs = logs_request(&config, &run);
        assert!(contains(&logs, "agent run completed"));
        assert!(contains(&logs, "mhc.agent.run.completed"));
        assert!(contains(&logs, "write,read") || contains(&logs, "write"));
        let payloads = run_payloads(&config, &run);
        assert_eq!(payloads.len(), 2);
        let quiet = ExportConfig {
            info_logs: false,
            ..config
        };
        assert_eq!(run_payloads(&quiet, &run).len(), 1);
    }

    struct Recorder {
        seen: Arc<Mutex<Vec<(String, usize)>>>,
        fail: bool,
    }

    impl Transport for Recorder {
        fn post(
            &mut self,
            url: &str,
            _headers: &[(String, String)],
            body: Vec<u8>,
        ) -> Result<(), String> {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((url.to_owned(), body.len()));
            if self.fail {
                Err("down".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn exporter_posts_in_order_flushes_and_reports_one_failure() {
        let seen: Arc<Mutex<Vec<(String, usize)>>> = Arc::default();
        let reports: Arc<Mutex<Vec<String>>> = Arc::default();
        let thread_seen = Arc::clone(&seen);
        let sink = Arc::clone(&reports);
        let exporter = Exporter::spawn(
            Box::new(move || {
                Ok(Box::new(Recorder {
                    seen: thread_seen,
                    fail: true,
                }) as Box<dyn Transport>)
            }),
            Arc::new(move |line: &str| {
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(line.to_owned());
            }),
        );
        let config = config(&[Signal::Traces, Signal::Metrics]);
        for (target, body) in run_payloads(&config, &sample_run()) {
            exporter.send(&target, body);
        }
        assert!(exporter.flush(Duration::from_secs(5)));
        let seen = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, "http://collector/v1/traces");
        let reports = reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(reports, ["telemetry export failed: down"]);
    }

    #[test]
    fn exporter_flush_times_out_and_unavailable_transport_is_reported() {
        let reports: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = Arc::clone(&reports);
        let exporter = Exporter::spawn(
            Box::new(|| Err("no runtime".into())),
            Arc::new(move |line: &str| {
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(line.to_owned());
            }),
        );
        let target = config(&[Signal::Traces]).targets.remove(0);
        exporter.send(&target, vec![1]);
        assert!(exporter.flush(Duration::from_secs(5)));
        assert_eq!(
            reports
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            ["telemetry export unavailable: no runtime"]
        );
    }
}
