//! Given/when/then cases of docs/cleanroom/specs/telemetry.md "Delta (omo v5.1.52)" D1-D6.
use crate::extension::{ConfigSource, Shared, TelemetryExtension};
use crate::otlp::{
    Exporter, Run, Transport, exported_input, gen_ai_provider_name, logs_request, metrics_request,
    traces_request,
};
use crate::record::{Ledger, UsageRecord};
use crate::settings::{ConfigSwitches, Settings, host_arch_for, os_type_for, resolve};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

// ---- a minimal protobuf reader for the payloads this crate writes ----

#[derive(Clone, Debug)]
enum Field {
    Varint(u64),
    Fixed64(u64),
    Len(Vec<u8>),
}

fn read_varint(buf: &[u8], at: &mut usize) -> u64 {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let byte = buf[*at];
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
        shift += 7;
    }
}

fn fields(buf: &[u8]) -> Vec<(u64, Field)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < buf.len() {
        let key = read_varint(buf, &mut at);
        let field = match key & 7 {
            0 => Field::Varint(read_varint(buf, &mut at)),
            1 => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&buf[at..at + 8]);
                at += 8;
                Field::Fixed64(u64::from_le_bytes(bytes))
            }
            2 => {
                let len = usize::try_from(read_varint(buf, &mut at)).unwrap_or(usize::MAX);
                let bytes = buf[at..at + len].to_vec();
                at += len;
                Field::Len(bytes)
            }
            other => panic!("unexpected wire type {other}"),
        };
        out.push((key >> 3, field));
    }
    out
}

fn sub(buf: &[u8], number: u64) -> Vec<Vec<u8>> {
    fields(buf)
        .into_iter()
        .filter_map(|(n, field)| match field {
            Field::Len(bytes) if n == number => Some(bytes),
            _ => None,
        })
        .collect()
}

fn text(buf: &[u8], number: u64) -> String {
    sub(buf, number)
        .first()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default()
}

fn double(buf: &[u8], number: u64) -> Option<f64> {
    fields(buf).into_iter().find_map(|(n, field)| match field {
        Field::Fixed64(bits) if n == number => Some(f64::from_bits(bits)),
        _ => None,
    })
}

#[derive(Clone, Debug, PartialEq)]
enum Val {
    Str(String),
    Int(i64),
    Other,
}

fn any_value(buf: &[u8]) -> Val {
    for (n, field) in fields(buf) {
        match (n, field) {
            (1, Field::Len(bytes)) => return Val::Str(String::from_utf8_lossy(&bytes).into()),
            // Two's-complement reinterpretation is the protobuf `int64` encoding.
            (3, Field::Varint(value)) => return Val::Int(value as i64),
            _ => {}
        }
    }
    Val::Other
}

type Attrs = Vec<(String, Val)>;

fn attributes(buf: &[u8], number: u64) -> Attrs {
    sub(buf, number)
        .iter()
        .map(|kv| {
            let value = sub(kv, 2).first().map_or(Val::Other, |v| any_value(v));
            (text(kv, 1), value)
        })
        .collect()
}

fn get<'a>(attrs: &'a Attrs, key: &str) -> Option<&'a Val> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn int(value: Option<&Val>) -> Option<i64> {
    match value {
        Some(Val::Int(number)) => Some(*number),
        _ => None,
    }
}

fn s(value: &str) -> Val {
    Val::Str(value.to_owned())
}

/// `(name, attributes)` of every span.
fn spans(body: &[u8]) -> Vec<(String, Attrs)> {
    let mut out = Vec::new();
    for resource_spans in sub(body, 1) {
        for scope_spans in sub(&resource_spans, 2) {
            for span in sub(&scope_spans, 2) {
                out.push((text(&span, 5), attributes(&span, 9)));
            }
        }
    }
    out
}

fn resource_attributes(body: &[u8]) -> Attrs {
    let resource_items = sub(body, 1);
    let resource = sub(&resource_items[0], 1);
    attributes(&resource[0], 1)
}

/// `(attributes, value)` of every point of the metric `name` (histogram sum or counter value).
fn metric_points(body: &[u8], name: &str) -> Vec<(Attrs, f64)> {
    let mut out = Vec::new();
    for resource_metrics in sub(body, 1) {
        for scope_metrics in sub(&resource_metrics, 2) {
            for metric in sub(&scope_metrics, 2) {
                if text(&metric, 1) != name {
                    continue;
                }
                for histogram in sub(&metric, 9) {
                    for point in sub(&histogram, 1) {
                        out.push((attributes(&point, 9), double(&point, 5).unwrap_or(0.0)));
                    }
                }
                for sum in sub(&metric, 7) {
                    for point in sub(&sum, 1) {
                        out.push((attributes(&point, 7), double(&point, 4).unwrap_or(0.0)));
                    }
                }
            }
        }
    }
    out
}

fn log_attributes(body: &[u8]) -> Attrs {
    let mut out = Vec::new();
    for resource_logs in sub(body, 1) {
        for scope_logs in sub(&resource_logs, 2) {
            for record in sub(&scope_logs, 2) {
                out.extend(attributes(&record, 6));
            }
        }
    }
    out
}

/// Every string attribute value in the three payloads (resource included).
fn all_strings(traces: &[u8], metrics: &[u8], logs: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |attrs: Attrs| {
        for (_, value) in attrs {
            if let Val::Str(text) = value {
                out.push(text);
            }
        }
    };
    for body in [traces, metrics, logs] {
        push(resource_attributes(body));
    }
    for (_, attrs) in spans(traces) {
        push(attrs);
    }
    for resource_metrics in sub(metrics, 1) {
        for scope_metrics in sub(&resource_metrics, 2) {
            for metric in sub(&scope_metrics, 2) {
                let name = text(&metric, 1);
                for (attrs, _) in metric_points(metrics, &name) {
                    push(attrs);
                }
            }
        }
    }
    push(log_attributes(logs));
    out
}

// ---- fixtures ----

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |key: &str| map.get(key).cloned()
}

const ENDPOINT: (&str, &str) = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318");

fn settings(dir: &Path, pairs: &[(&str, &str)]) -> Settings {
    resolve(&env_of(pairs), ConfigSwitches::default(), dir, "0")
}

fn assistant(provider: &str, model: &str, input: u64, read: u64, write: u64) -> UsageRecord {
    UsageRecord::from_assistant(&serde_json::json!({
        "role": "assistant", "api": "openai-completions", "provider": provider, "model": model,
        "usage": {"input": input, "output": 5, "cacheRead": read, "cacheWrite": write,
                  "cost": {"total": 0.01}},
        "stopReason": "stop", "timestamp": 1_800_000_000_000_i64
    }))
    .expect("assistant record")
}

type Posts = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

struct Sink(Posts);

impl Transport for Sink {
    fn post(
        &mut self,
        url: &str,
        _headers: &[(String, String)],
        body: Vec<u8>,
    ) -> Result<(), String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((url.to_owned(), body));
        Ok(())
    }
}

struct Harness {
    shared: Shared,
    posts: Posts,
    reports: Arc<Mutex<Vec<String>>>,
    exporters_built: Arc<AtomicUsize>,
}

impl Harness {
    fn new(settings: Settings, source: Option<ConfigSource>) -> Self {
        let posts: Posts = Arc::default();
        let reports: Arc<Mutex<Vec<String>>> = Arc::default();
        let exporters_built = Arc::new(AtomicUsize::new(0));
        let sink = Arc::clone(&reports);
        let report: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |line: &str| {
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(line.to_owned());
        });
        let make_posts = Arc::clone(&posts);
        let make_report = Arc::clone(&report);
        let built = Arc::clone(&exporters_built);
        let mut extension = TelemetryExtension::new(settings);
        extension.exporter = Some(Arc::new(move || {
            built.fetch_add(1, Ordering::SeqCst);
            let posts = Arc::clone(&make_posts);
            Exporter::spawn(
                Box::new(move || Ok(Box::new(Sink(posts)) as Box<dyn Transport>)),
                Arc::clone(&make_report),
            )
        }));
        extension.config_source = source;
        extension.report = Some(report);
        Self {
            shared: Shared::new(extension, "/work".to_owned()),
            posts,
            reports,
            exporters_built,
        }
    }

    fn posts(&self) -> Vec<(String, Vec<u8>)> {
        self.posts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn body(&self, suffix: &str) -> Vec<u8> {
        self.posts()
            .into_iter()
            .find(|(url, _)| url.ends_with(suffix))
            .map(|(_, body)| body)
            .unwrap_or_else(|| panic!("no request to {suffix}"))
    }

    fn reports(&self) -> Vec<String> {
        self.reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// A config source that reads the switches from a JSON file standing in for the merged view
/// (the project config edited during the session).
fn file_source(path: PathBuf) -> ConfigSource {
    Arc::new(move || {
        let text = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let view: serde_json::Value =
            serde_json::from_str(&text).map_err(|error| error.to_string())?;
        Ok(ConfigSwitches::from_view(&view))
    })
}

fn ledger_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

// ---- D1: model ids verbatim ----

#[test]
fn d1_new_model_id_reaches_ledger_and_chat_span_verbatim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = settings(dir.path(), &[ENDPOINT]);
    let ledger = settings.ledger.clone().expect("ledger on");
    let harness = Harness::new(settings, None);
    harness
        .shared
        .on_record(&assistant("anthropic", "claude-fable-5-5", 10, 0, 0), "s1");
    harness.shared.flush();
    let lines = ledger_lines(&ledger);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].contains(r#""model":"claude-fable-5-5""#),
        "{}",
        lines[0]
    );
    let spans = spans(&harness.body("/v1/traces"));
    let (_, chat) = spans
        .iter()
        .find(|(name, _)| name == "chat claude-fable-5-5")
        .expect("chat span");
    assert_eq!(
        get(chat, "gen_ai.request.model"),
        Some(&s("claude-fable-5-5"))
    );
}

#[test]
fn d1_usage_by_model_keeps_the_exact_ids() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = settings(dir.path(), &[]);
    let ledger = Ledger::new(settings.ledger.clone().expect("ledger on"));
    ledger
        .append(&assistant("kimi-coding", "k3.1-pro-highspeed", 1, 0, 0))
        .expect("append");
    ledger
        .append(&assistant("moonshotai", "kimi-k3.1-pro", 1, 0, 0))
        .expect("append");
    let json: serde_json::Value = serde_json::from_str(
        &crate::cli::run(&["--json".to_owned()], &settings, dir.path()).expect("report"),
    )
    .expect("json");
    let by_model = json["byModel"].as_array().expect("byModel");
    let mut ids: Vec<(String, String)> = by_model
        .iter()
        .map(|entry| {
            (
                entry["provider"].as_str().unwrap_or_default().to_owned(),
                entry["model"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            ("kimi-coding".to_owned(), "k3.1-pro-highspeed".to_owned()),
            ("moonshotai".to_owned(), "kimi-k3.1-pro".to_owned()),
        ]
    );
}

#[test]
fn d1_unknown_model_is_never_replaced_by_a_placeholder() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = settings(dir.path(), &[ENDPOINT]);
    let config = settings.export.clone().expect("export on");
    let mut run = Run::start("s", 1);
    run.chat(&assistant("openrouter", "zz-experimental-9", 3, 0, 0), 2);
    run.finish(3);
    let traces = traces_request(&config, &run);
    let metrics = metrics_request(&config, &run);
    let logs = logs_request(&config, &run);
    let strings = all_strings(&traces, &metrics, &logs);
    assert!(strings.iter().any(|value| value == "zz-experimental-9"));
    assert!(
        !strings
            .iter()
            .any(|value| value == "custom" || value == "other"),
        "{strings:?}"
    );
    assert_eq!(
        get(&log_attributes(&logs), "mhc.agent.coverage.models_used"),
        Some(&s("zz-experimental-9"))
    );
}

// ---- D2: identity and environment invariants ----

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn d2_host_name_and_its_digest_never_appear() {
    // SHA-256 hex digest of "build-box-gamma".
    const DIGEST: &str = "6e40b737d53da75bb343a0e83e5042398993c3eedcb2d031f6f27619cd119ff2";
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = settings(
        dir.path(),
        &[
            ENDPOINT,
            ("HOSTNAME", "build-box-gamma"),
            ("HOST", "build-box-gamma"),
            ("COMPUTERNAME", "build-box-gamma"),
        ],
    );
    let harness = Harness::new(settings, None);
    harness
        .shared
        .on_record(&assistant("anthropic", "m", 1, 0, 0), "s");
    harness.shared.flush();
    let upper = DIGEST.to_ascii_uppercase();
    let posts = harness.posts();
    assert_eq!(posts.len(), 3);
    for (url, body) in posts {
        for needle in ["build-box-gamma", DIGEST, upper.as_str()] {
            assert!(!contains(&body, needle), "{url} contains {needle}");
        }
    }
}

#[test]
fn d2_shell_and_terminal_values_never_appear() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = settings(
        dir.path(),
        &[
            ENDPOINT,
            ("SHELL", "/opt/custom/bin/zsh"),
            ("TERM_PROGRAM", "SomeTerm"),
        ],
    );
    let ledger = settings.ledger.clone().expect("ledger on");
    let harness = Harness::new(settings, None);
    harness
        .shared
        .on_record(&assistant("anthropic", "m", 1, 0, 0), "s");
    harness.shared.flush();
    let posts = harness.posts();
    assert_eq!(posts.len(), 3);
    for (url, body) in posts {
        for needle in ["/opt/custom/bin/zsh", "SomeTerm"] {
            assert!(!contains(&body, needle), "{url} contains {needle}");
        }
    }
    for line in ledger_lines(&ledger) {
        assert!(!line.contains("/opt/custom/bin/zsh") && !line.contains("SomeTerm"));
    }
}

fn files_under(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read_dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(root, &path, out);
        } else {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
fn d2_the_only_file_created_is_the_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = settings(dir.path(), &[ENDPOINT]);
    let harness = Harness::new(settings, None);
    harness
        .shared
        .on_record(&assistant("anthropic", "m", 1, 0, 0), "s");
    harness.shared.flush();
    harness.shared.flush();
    let mut files = Vec::new();
    files_under(dir.path(), dir.path(), &mut files);
    assert_eq!(files, ["stats/usage.jsonl"]);
}

#[test]
fn d2_default_resource_keys_are_exactly_the_standard_ones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = settings(dir.path(), &[ENDPOINT]);
    let config = settings.export.clone().expect("export on");
    let mut run = Run::start("s", 1);
    run.finish(2);
    let mut keys: Vec<String> = resource_attributes(&traces_request(&config, &run))
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    keys.sort();
    let mut expected = vec![
        "process.pid".to_owned(),
        "service.instance.id".to_owned(),
        "service.name".to_owned(),
        "service.version".to_owned(),
    ];
    if os_type_for(std::env::consts::OS).is_some() {
        expected.push("os.type".to_owned());
    }
    if host_arch_for(std::env::consts::ARCH).is_some() {
        expected.push("host.arch".to_owned());
    }
    expected.sort();
    assert_eq!(keys, expected);
}

// ---- D3: cache-inclusive exported input ----

#[test]
fn d3_span_input_includes_cache_and_ledger_keeps_uncached_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = settings(dir.path(), &[ENDPOINT]);
    let ledger = settings.ledger.clone().expect("ledger on");
    let harness = Harness::new(settings, None);
    harness
        .shared
        .on_record(&assistant("anthropic", "m", 100, 900, 50), "s");
    harness.shared.flush();
    let spans = spans(&harness.body("/v1/traces"));
    let (_, chat) = spans
        .iter()
        .find(|(name, _)| name == "chat m")
        .expect("chat span");
    assert_eq!(int(get(chat, "gen_ai.usage.input_tokens")), Some(1050));
    assert_eq!(
        int(get(chat, "gen_ai.usage.cache_read.input_tokens")),
        Some(900)
    );
    assert_eq!(
        int(get(chat, "gen_ai.usage.cache_creation.input_tokens")),
        Some(50)
    );
    let lines = ledger_lines(&ledger);
    assert!(lines[0].contains(r#""input":100"#), "{}", lines[0]);
}

fn input_points(metrics: &[u8]) -> Vec<f64> {
    metric_points(metrics, "gen_ai.client.token.usage")
        .into_iter()
        .filter(|(attrs, _)| get(attrs, "gen_ai.token.type") == Some(&s("input")))
        .map(|(_, value)| value)
        .collect()
}

#[test]
fn d3_run_log_sums_exported_input_and_histogram_has_one_point_per_chat() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = settings(dir.path(), &[ENDPOINT]).export.expect("export on");
    let mut run = Run::start("s", 1);
    run.chat(&assistant("anthropic", "m", 100, 900, 50), 2);
    run.chat(&assistant("anthropic", "m", 4, 6, 0), 3);
    run.finish(4);
    assert_eq!(
        int(get(
            &log_attributes(&logs_request(&config, &run)),
            "mhc.agent.usage.input_tokens"
        )),
        Some(1060)
    );
    let mut points = input_points(&metrics_request(&config, &run));
    points.sort_by(f64::total_cmp);
    assert_eq!(points, [10.0, 1050.0]);
}

#[test]
fn d3_zero_input_emits_no_input_point_and_sum_saturates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = settings(dir.path(), &[ENDPOINT]).export.expect("export on");
    let mut run = Run::start("s", 1);
    run.chat(&assistant("anthropic", "m", 0, 0, 0), 2);
    run.finish(3);
    assert!(input_points(&metrics_request(&config, &run)).is_empty());
    let huge = UsageRecord {
        input: u64::MAX,
        cache_read: 1,
        cache_write: 1,
        ..UsageRecord::default()
    };
    assert_eq!(exported_input(&huge), u64::MAX);
}

// ---- D4: gen_ai.provider.name well-known values ----

#[test]
fn d4_bedrock_maps_to_the_well_known_value_on_spans_and_metric_points() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = settings(dir.path(), &[ENDPOINT]).export.expect("export on");
    let mut run = Run::start("s", 1);
    run.chat(&assistant("amazon-bedrock", "m", 10, 0, 0), 2);
    run.finish(3);
    let spans = spans(&traces_request(&config, &run));
    let (_, chat) = spans
        .iter()
        .find(|(name, _)| name == "chat m")
        .expect("chat span");
    assert_eq!(get(chat, "gen_ai.provider.name"), Some(&s("aws.bedrock")));
    assert_eq!(get(chat, "mhc.provider.id"), Some(&s("amazon-bedrock")));
    let points = metric_points(&metrics_request(&config, &run), "gen_ai.client.token.usage");
    assert!(!points.is_empty());
    for (attrs, _) in points {
        assert_eq!(get(&attrs, "gen_ai.provider.name"), Some(&s("aws.bedrock")));
        assert_eq!(get(&attrs, "mhc.provider.id"), Some(&s("amazon-bedrock")));
    }
}

#[test]
fn d4_unlisted_provider_is_exported_unchanged_in_both_attributes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = settings(dir.path(), &[ENDPOINT]).export.expect("export on");
    let mut run = Run::start("s", 1);
    run.chat(&assistant("kimi-coding", "m", 10, 0, 0), 2);
    run.finish(3);
    let spans = spans(&traces_request(&config, &run));
    let (_, chat) = spans
        .iter()
        .find(|(name, _)| name == "chat m")
        .expect("chat span");
    assert_eq!(get(chat, "gen_ai.provider.name"), Some(&s("kimi-coding")));
    assert_eq!(get(chat, "mhc.provider.id"), Some(&s("kimi-coding")));
    // The mapping table, exact and case-sensitive.
    for (id, expected) in [
        ("anthropic", "anthropic"),
        ("openai", "openai"),
        ("chatgpt-subscription", "openai"),
        ("openai-codex", "openai"),
        ("azure-openai-responses", "azure.ai.openai"),
        ("amazon-bedrock", "aws.bedrock"),
        ("google", "gcp.gemini"),
        ("google-vertex", "gcp.vertex_ai"),
        ("mistral", "mistral_ai"),
        ("xai", "x_ai"),
        ("groq", "groq"),
        ("deepseek", "deepseek"),
        ("Anthropic", "Anthropic"),
        ("openrouter", "openrouter"),
        ("", ""),
    ] {
        assert_eq!(gen_ai_provider_name(id), expected, "{id}");
    }
}

#[test]
fn d4_ledger_keeps_the_mhc_provider_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = settings(dir.path(), &[ENDPOINT]);
    let ledger = settings.ledger.clone().expect("ledger on");
    let harness = Harness::new(settings, None);
    harness
        .shared
        .on_record(&assistant("google-vertex", "m", 1, 0, 0), "s");
    let lines = ledger_lines(&ledger);
    assert!(
        lines[0].contains(r#""provider":"google-vertex""#),
        "{}",
        lines[0]
    );
}

// ---- D5: standard resource attributes ----

fn resource_of(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    let dir = tempfile::tempdir().expect("tempdir");
    settings(dir.path(), pairs)
        .export
        .expect("export on")
        .resource
}

fn resource_value<'a>(resource: &'a [(String, String)], key: &str) -> Option<&'a str> {
    resource
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn d5_linux_x86_64_resource_has_os_arch_and_a_uuid_instance_id() {
    let resource = resource_of(&[ENDPOINT]);
    assert_eq!(resource_value(&resource, "os.type"), Some("linux"));
    assert_eq!(resource_value(&resource, "host.arch"), Some("amd64"));
    let id = resource_value(&resource, "service.instance.id").expect("instance id");
    assert!(uuid::Uuid::parse_str(id).is_ok(), "{id}");
}

#[test]
fn d5_platform_mappings_and_omission() {
    for (os, expected) in [
        ("linux", Some("linux")),
        ("macos", Some("darwin")),
        ("windows", Some("windows")),
        ("freebsd", Some("freebsd")),
        ("netbsd", Some("netbsd")),
        ("openbsd", Some("openbsd")),
        ("dragonfly", Some("dragonflybsd")),
        ("solaris", Some("solaris")),
        ("aix", Some("aix")),
        ("haiku", None),
    ] {
        assert_eq!(os_type_for(os), expected, "{os}");
    }
    for (arch, expected) in [
        ("x86_64", Some("amd64")),
        ("aarch64", Some("arm64")),
        ("x86", Some("x86")),
        ("arm", Some("arm32")),
        ("powerpc", Some("ppc32")),
        ("powerpc64", Some("ppc64")),
        ("s390x", Some("s390x")),
        ("riscv64", None),
    ] {
        assert_eq!(host_arch_for(arch), expected, "{arch}");
    }
}

#[test]
fn d5_user_resource_attributes_override_the_defaults() {
    let resource = resource_of(&[
        ENDPOINT,
        (
            "OTEL_RESOURCE_ATTRIBUTES",
            "os.type=custom-os,service.instance.id=fixed",
        ),
    ]);
    assert_eq!(resource_value(&resource, "os.type"), Some("custom-os"));
    assert_eq!(
        resource_value(&resource, "service.instance.id"),
        Some("fixed")
    );
    // service.version and process.pid stay authoritative.
    let resource = resource_of(&[
        ENDPOINT,
        (
            "OTEL_RESOURCE_ATTRIBUTES",
            "service.version=9,process.pid=1",
        ),
    ]);
    assert_eq!(resource_value(&resource, "service.version"), Some("0"));
    assert_eq!(
        resource_value(&resource, "process.pid"),
        Some(std::process::id().to_string().as_str())
    );
}

#[test]
fn d5_status_prints_the_resolved_resource() {
    let dir = tempfile::tempdir().expect("tempdir");
    let status = settings(dir.path(), &[ENDPOINT]).status_text();
    assert!(status.contains("  resource: service.name=mhc"), "{status}");
    assert!(status.contains("service.instance.id="), "{status}");
}

const PRINT_INSTANCE_ID: &str = "MAHO_TELEMETRY_TEST_PRINT_INSTANCE_ID";
const INSTANCE_TEST: &str =
    "delta_5152_tests::d5_instance_id_is_stable_in_a_process_and_differs_across_processes";

fn instance_id() -> String {
    resource_value(&resource_of(&[ENDPOINT]), "service.instance.id")
        .expect("instance id")
        .to_owned()
}

fn child_instance_id() -> String {
    let exe = std::env::current_exe().expect("test binary");
    let output = std::process::Command::new(exe)
        .args([INSTANCE_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(PRINT_INSTANCE_ID, "1")
        .output()
        .expect("spawn test binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| line.split_once("instance-id="))
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("no instance id in child output: {stdout}"))
}

#[test]
fn d5_instance_id_is_stable_in_a_process_and_differs_across_processes() {
    if std::env::var_os(PRINT_INSTANCE_ID).is_some() {
        println!("instance-id={}", instance_id());
        return;
    }
    assert_eq!(instance_id(), instance_id());
    let first = child_instance_id();
    let second = child_instance_id();
    assert_ne!(first, second);
    assert_ne!(first, instance_id());
}

// ---- D6: push-time opt-out re-check ----

fn write_view(path: &Path, view: &serde_json::Value) {
    std::fs::write(path, view.to_string()).expect("write view");
}

#[test]
fn d6_export_turned_off_mid_session_discards_with_one_diagnostic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let view = dir.path().join("view.json");
    write_view(&view, &serde_json::json!({"telemetry": {}}));
    let settings = settings(&dir.path().join("agent"), &[ENDPOINT]);
    let ledger = settings.ledger.clone().expect("ledger on");
    let harness = Harness::new(settings, Some(file_source(view.clone())));
    harness
        .shared
        .on_record(&assistant("anthropic", "m", 1, 0, 0), "s");
    write_view(
        &view,
        &serde_json::json!({"telemetry": {"otlpExportEnabled": false}}),
    );
    harness.shared.flush();
    assert!(harness.posts().is_empty());
    let reports = harness.reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert!(
        reports[0].contains("turned off in the configuration"),
        "{reports:?}"
    );
    assert!(reports[0].contains("discarded 3 payload(s)"), "{reports:?}");
    // A second run in the same session: still nothing sent, no second diagnostic.
    harness
        .shared
        .on_record(&assistant("anthropic", "m", 1, 0, 0), "s");
    harness.shared.flush();
    assert!(harness.posts().is_empty());
    assert_eq!(harness.reports().len(), 1);
    // The ledger is unaffected.
    assert_eq!(ledger_lines(&ledger).len(), 2);
}

#[test]
fn d6_master_switch_turned_off_mid_session_also_discards() {
    let dir = tempfile::tempdir().expect("tempdir");
    let view = dir.path().join("view.json");
    write_view(&view, &serde_json::json!({"telemetry": {"enabled": false}}));
    let harness = Harness::new(
        settings(&dir.path().join("agent"), &[ENDPOINT]),
        Some(file_source(view)),
    );
    harness
        .shared
        .on_record(&assistant("anthropic", "m", 1, 0, 0), "s");
    harness.shared.flush();
    assert!(harness.posts().is_empty());
    assert_eq!(harness.reports().len(), 1);
}

#[test]
fn d6_export_still_on_at_push_time_sends() {
    let dir = tempfile::tempdir().expect("tempdir");
    let view = dir.path().join("view.json");
    write_view(
        &view,
        &serde_json::json!({"telemetry": {"enabled": true, "otlpExportEnabled": true}}),
    );
    let harness = Harness::new(
        settings(&dir.path().join("agent"), &[ENDPOINT]),
        Some(file_source(view)),
    );
    harness
        .shared
        .on_record(&assistant("anthropic", "m", 1, 0, 0), "s");
    harness.shared.flush();
    assert_eq!(harness.posts().len(), 3);
    assert!(harness.reports().is_empty());
}

#[test]
fn d6_export_off_at_startup_never_starts_exporting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let view = dir.path().join("view.json");
    write_view(&view, &serde_json::json!({"telemetry": {"enabled": true}}));
    let settings = settings(&dir.path().join("agent"), &[]);
    assert!(settings.export.is_none());
    let harness = Harness::new(settings, Some(file_source(view)));
    harness
        .shared
        .on_record(&assistant("anthropic", "m", 1, 0, 0), "s");
    harness.shared.flush();
    assert!(harness.posts().is_empty());
    assert_eq!(harness.exporters_built.load(Ordering::SeqCst), 0);
}

#[test]
fn d6_unreadable_config_view_fails_closed_with_the_diagnostic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source: ConfigSource = Arc::new(|| Err("no configuration view".to_owned()));
    let harness = Harness::new(
        settings(&dir.path().join("agent"), &[ENDPOINT]),
        Some(source),
    );
    harness
        .shared
        .on_record(&assistant("anthropic", "m", 1, 0, 0), "s");
    harness.shared.flush();
    assert!(harness.posts().is_empty());
    let reports = harness.reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert!(
        reports[0].starts_with("telemetry export skipped"),
        "{reports:?}"
    );
    assert!(reports[0].contains("discarded 3 payload(s)"), "{reports:?}");
}
