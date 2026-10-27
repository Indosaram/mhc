//! Opt-out resolution (docs/cleanroom/specs/telemetry.md "Opt-out" and "Per-signal rules").
//!
//! The export gate and the per-signal endpoint/exporter/protocol rules are a port of oh-my-pi
//! `packages/coding-agent/src/telemetry-export.ts` (`initTelemetryExport`, `resolveSignalConfig`,
//! `signalEnabled`) and `telemetry-settings.ts` (`telemetry.otlpExportEnabled`). The master
//! switch, `DO_NOT_TRACK` and the ledger switch are specified in the clean-room spec.
use percent_encoding::percent_decode_str;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One OTLP signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Traces,
    Metrics,
    Logs,
}

impl Signal {
    pub const ALL: [Self; 3] = [Self::Traces, Self::Metrics, Self::Logs];

    /// The `<S>` part of the per-signal `OTEL_*` variable names.
    pub fn env_part(self) -> &'static str {
        match self {
            Self::Traces => "TRACES",
            Self::Metrics => "METRICS",
            Self::Logs => "LOGS",
        }
    }

    /// The OTLP/HTTP path appended to the common endpoint.
    pub fn path(self) -> &'static str {
        match self {
            Self::Traces => "/v1/traces",
            Self::Metrics => "/v1/metrics",
            Self::Logs => "/v1/logs",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Traces => "traces",
            Self::Metrics => "metrics",
            Self::Logs => "logs",
        }
    }
}

/// Where one enabled signal is pushed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalTarget {
    pub signal: Signal,
    pub url: String,
    /// Secret material: never print.
    pub headers: Vec<(String, String)>,
}

/// Resolved OTLP export configuration (present only when at least one signal is enabled).
#[derive(Clone, Debug, PartialEq)]
pub struct ExportConfig {
    pub targets: Vec<SignalTarget>,
    /// Resource attributes (`service.name`, `service.version`, `OTEL_RESOURCE_ATTRIBUTES`, ...).
    pub resource: Vec<(String, String)>,
    /// `OTEL_LOG_LEVEL` admits INFO records (the run log).
    pub info_logs: bool,
}

impl ExportConfig {
    pub fn target(&self, signal: Signal) -> Option<&SignalTarget> {
        self.targets.iter().find(|target| target.signal == signal)
    }
}

/// Everything the telemetry extension and `mhc usage` need to know about the user's choices.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The ledger file; `None` when the ledger is off.
    pub ledger: Option<PathBuf>,
    /// The ledger file the user would get with the ledger on (for reports).
    pub ledger_location: PathBuf,
    pub ledger_off: Vec<String>,
    pub export: Option<ExportConfig>,
    pub export_off: Vec<String>,
    /// One-line notes (unsupported protocol, ...); never contain header values.
    pub diagnostics: Vec<String>,
}

/// The config switches read from the merged native configuration view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConfigSwitches {
    pub enabled: Option<bool>,
    pub otlp_export_enabled: Option<bool>,
    pub usage_stats: Option<bool>,
}

impl ConfigSwitches {
    /// Reads `/telemetry/{enabled,otlpExportEnabled,usageStats}`; non-booleans are ignored.
    pub fn from_view(view: &Value) -> Self {
        let flag = |pointer: &str| view.pointer(pointer).and_then(Value::as_bool);
        Self {
            enabled: flag("/telemetry/enabled"),
            otlp_export_enabled: flag("/telemetry/otlpExportEnabled"),
            usage_stats: flag("/telemetry/usageStats"),
        }
    }
}

const FALSY: [&str; 4] = ["0", "false", "no", "off"];

fn normalized(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

/// Non-empty and not `0`/`false`/`no`/`off`.
pub fn truthy(value: Option<String>) -> bool {
    normalized(value).is_some_and(|value| !FALSY.contains(&value.as_str()))
}

/// One of `0`/`false`/`no`/`off`.
pub fn falsy(value: Option<String>) -> bool {
    normalized(value).is_some_and(|value| FALSY.contains(&value.as_str()))
}

/// `k=v,k2=v2` with trimmed, percent-decoded keys and values; entries without `=` are skipped.
pub fn parse_key_values(raw: &str) -> Vec<(String, String)> {
    let decode = |text: &str| {
        percent_decode_str(text.trim())
            .decode_utf8_lossy()
            .into_owned()
    };
    raw.split(',')
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            let key = decode(key);
            (!key.is_empty()).then(|| (key, decode(value)))
        })
        .collect()
}

fn merge(into: &mut Vec<(String, String)>, entries: Vec<(String, String)>) {
    for (key, value) in entries {
        if let Some(slot) = into.iter_mut().find(|(existing, _)| *existing == key) {
            slot.1 = value;
        } else {
            into.push((key, value));
        }
    }
}

/// Port of oh-my-pi `signalEnabled`, plus the request URL and headers of an enabled signal.
fn signal_target(
    signal: Signal,
    env: &dyn Fn(&str) -> Option<String>,
    diagnostics: &mut Vec<String>,
) -> Option<SignalTarget> {
    let part = signal.env_part();
    let non_empty = |key: &str| env(key).filter(|value| !value.trim().is_empty());
    if let Some(selection) = env(&format!("OTEL_{part}_EXPORTER")) {
        let entries: Vec<String> = selection
            .split(',')
            .map(|entry| entry.trim().to_ascii_lowercase())
            .filter(|entry| !entry.is_empty())
            .collect();
        if entries.iter().any(|entry| entry == "none") {
            return None;
        }
        if !entries.is_empty() && !entries.iter().any(|entry| entry == "otlp") {
            return None;
        }
    }
    let url = match non_empty(&format!("OTEL_EXPORTER_OTLP_{part}_ENDPOINT")) {
        Some(endpoint) => endpoint.trim().to_owned(),
        None => {
            let common = non_empty("OTEL_EXPORTER_OTLP_ENDPOINT")?;
            let common = common.trim();
            let base = common.strip_suffix('/').unwrap_or(common);
            format!("{base}{}", signal.path())
        }
    };
    let protocol = env(&format!("OTEL_EXPORTER_OTLP_{part}_PROTOCOL"))
        .or_else(|| env("OTEL_EXPORTER_OTLP_PROTOCOL"))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    if let Some(protocol) = protocol
        && protocol != "http/protobuf"
    {
        diagnostics.push(format!(
            "OTEL {} export disabled: protocol {protocol} is unsupported (supported: http/protobuf)",
            signal.name()
        ));
        return None;
    }
    let mut headers = Vec::new();
    if let Some(raw) = env("OTEL_EXPORTER_OTLP_HEADERS") {
        merge(&mut headers, parse_key_values(&raw));
    }
    if let Some(raw) = env(&format!("OTEL_EXPORTER_OTLP_{part}_HEADERS")) {
        merge(&mut headers, parse_key_values(&raw));
    }
    Some(SignalTarget {
        signal,
        url,
        headers,
    })
}

fn resource(env: &dyn Fn(&str) -> Option<String>, version: &str) -> Vec<(String, String)> {
    let mut attributes = vec![("service.name".to_owned(), "mhc".to_owned())];
    if let Some(raw) = env("OTEL_RESOURCE_ATTRIBUTES") {
        merge(&mut attributes, parse_key_values(&raw));
    }
    if let Some(name) = env("OTEL_SERVICE_NAME").filter(|name| !name.trim().is_empty()) {
        merge(
            &mut attributes,
            vec![("service.name".to_owned(), name.trim().to_owned())],
        );
    }
    merge(
        &mut attributes,
        vec![
            ("service.version".to_owned(), version.to_owned()),
            ("process.pid".to_owned(), std::process::id().to_string()),
        ],
    );
    attributes
}

/// `OTEL_LOG_LEVEL` (oh-my-pi `parseOtelLogLevel`): INFO records pass unless the level is `none`,
/// `error` or `warn`/`warning`.
fn info_logs(env: &dyn Fn(&str) -> Option<String>) -> bool {
    !matches!(
        normalized(env("OTEL_LOG_LEVEL")).as_deref(),
        Some("none" | "error" | "warn" | "warning")
    )
}

/// `$MAHO_USAGE_DIR`, else `<agent dir>/stats`.
pub fn usage_dir(env: &dyn Fn(&str) -> Option<String>, agent_dir: &Path) -> PathBuf {
    env("MAHO_USAGE_DIR")
        .map(|dir| dir.trim().to_owned())
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| agent_dir.join("stats"), PathBuf::from)
}

/// The ledger file name inside the usage directory.
pub const LEDGER_FILE: &str = "usage.jsonl";

/// Resolves the user's telemetry choices. `version` is stamped as `service.version`.
pub fn resolve(
    env: &dyn Fn(&str) -> Option<String>,
    config: ConfigSwitches,
    agent_dir: &Path,
    version: &str,
) -> Settings {
    let mut ledger_off = Vec::new();
    let mut export_off = Vec::new();
    let mut diagnostics = Vec::new();
    let master_off = if falsy(env("MAHO_TELEMETRY")) {
        Some("MAHO_TELEMETRY is off")
    } else if config.enabled == Some(false) {
        Some("telemetry.enabled is false")
    } else {
        None
    };
    if let Some(reason) = master_off {
        ledger_off.push(reason.to_owned());
        export_off.push(reason.to_owned());
    }
    if falsy(env("MAHO_USAGE_STATS")) {
        ledger_off.push("MAHO_USAGE_STATS is off".to_owned());
    }
    if config.usage_stats == Some(false) {
        ledger_off.push("telemetry.usageStats is false".to_owned());
    }
    if truthy(env("DO_NOT_TRACK")) {
        export_off.push("DO_NOT_TRACK is set".to_owned());
    }
    if config.otlp_export_enabled == Some(false) {
        export_off.push("telemetry.otlpExportEnabled is false".to_owned());
    }
    if normalized(env("OTEL_SDK_DISABLED")).as_deref() == Some("true") {
        export_off.push("OTEL_SDK_DISABLED is true".to_owned());
    }
    let mut export = None;
    if export_off.is_empty() {
        let targets: Vec<SignalTarget> = Signal::ALL
            .into_iter()
            .filter_map(|signal| signal_target(signal, env, &mut diagnostics))
            .collect();
        if targets.is_empty() {
            export_off.push("no OTEL_EXPORTER_OTLP_*ENDPOINT is configured".to_owned());
        } else {
            export = Some(ExportConfig {
                targets,
                resource: resource(env, version),
                info_logs: info_logs(env),
            });
        }
    }
    let ledger_location = usage_dir(env, agent_dir).join(LEDGER_FILE);
    Settings {
        ledger: ledger_off.is_empty().then(|| ledger_location.clone()),
        ledger_location,
        ledger_off,
        export,
        export_off,
        diagnostics,
    }
}

impl Settings {
    /// The `mhc usage --status` report. Never contains header values.
    pub fn status_text(&self) -> String {
        let mut lines = Vec::new();
        match &self.ledger {
            Some(path) => lines.push(format!("Usage ledger: on ({})", path.display())),
            None => lines.push(format!(
                "Usage ledger: off ({}); location {}",
                self.ledger_off.join("; "),
                self.ledger_location.display()
            )),
        }
        match &self.export {
            Some(export) => {
                lines.push("OTLP export: on (http/protobuf)".to_owned());
                for target in &export.targets {
                    let headers = if target.headers.is_empty() {
                        String::new()
                    } else {
                        format!(" ({} header(s))", target.headers.len())
                    };
                    lines.push(format!(
                        "  {}: {}{headers}",
                        target.signal.name(),
                        target.url
                    ));
                }
            }
            None => lines.push(format!("OTLP export: off ({})", self.export_off.join("; "))),
        }
        lines.extend(self.diagnostics.iter().map(|line| format!("Note: {line}")));
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    fn run(pairs: &[(&str, &str)], config: ConfigSwitches) -> Settings {
        resolve(&env_of(pairs), config, Path::new("/agent"), "1.2.3")
    }

    #[test]
    fn defaults_keep_the_ledger_and_export_nothing_without_an_endpoint() {
        let settings = run(&[], ConfigSwitches::default());
        assert_eq!(
            settings.ledger.as_deref(),
            Some(Path::new("/agent/stats/usage.jsonl"))
        );
        assert!(settings.export.is_none());
        assert_eq!(
            settings.export_off,
            ["no OTEL_EXPORTER_OTLP_*ENDPOINT is configured"]
        );
    }

    #[test]
    fn common_endpoint_gets_signal_paths_and_per_signal_endpoints_win() {
        let settings = run(
            &[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318/"),
                (
                    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
                    "http://collector/custom/logs",
                ),
            ],
            ConfigSwitches::default(),
        );
        let export = settings.export.expect("export on");
        let urls: Vec<&str> = export.targets.iter().map(|t| t.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "http://127.0.0.1:4318/v1/traces",
                "http://127.0.0.1:4318/v1/metrics",
                "http://collector/custom/logs"
            ]
        );
        assert!(export.info_logs);
    }

    #[test]
    fn do_not_track_and_other_kill_switches_disable_export_but_not_the_ledger() {
        let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318");
        for pairs in [
            vec![endpoint, ("DO_NOT_TRACK", "1")],
            vec![endpoint, ("DO_NOT_TRACK", "true")],
            vec![endpoint, ("OTEL_SDK_DISABLED", " TRUE ")],
        ] {
            let settings = run(&pairs, ConfigSwitches::default());
            assert!(settings.export.is_none(), "{pairs:?}");
            assert!(settings.ledger.is_some(), "{pairs:?}");
        }
        let settings = run(
            &[endpoint],
            ConfigSwitches {
                otlp_export_enabled: Some(false),
                ..ConfigSwitches::default()
            },
        );
        assert_eq!(
            settings.export_off,
            ["telemetry.otlpExportEnabled is false"]
        );
        for value in ["0", "false", "", "off"] {
            let settings = run(
                &[endpoint, ("DO_NOT_TRACK", value)],
                ConfigSwitches::default(),
            );
            assert!(settings.export.is_some(), "DO_NOT_TRACK={value:?}");
        }
    }

    #[test]
    fn master_switch_turns_everything_off() {
        let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318");
        let settings = run(
            &[endpoint, ("MAHO_TELEMETRY", "off")],
            ConfigSwitches::default(),
        );
        assert!(settings.export.is_none() && settings.ledger.is_none());
        let settings = run(
            &[endpoint],
            ConfigSwitches {
                enabled: Some(false),
                ..ConfigSwitches::default()
            },
        );
        assert!(settings.export.is_none() && settings.ledger.is_none());
        let settings = run(&[("MAHO_USAGE_STATS", "0")], ConfigSwitches::default());
        assert!(settings.ledger.is_none());
        assert_eq!(settings.ledger_off, ["MAHO_USAGE_STATS is off"]);
        let settings = run(
            &[],
            ConfigSwitches {
                usage_stats: Some(false),
                ..ConfigSwitches::default()
            },
        );
        assert!(settings.ledger.is_none());
    }

    #[test]
    fn exporter_selection_and_protocol_rules_follow_the_original() {
        let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://h:1");
        let settings = run(
            &[
                endpoint,
                ("OTEL_TRACES_EXPORTER", "otlp, none"),
                ("OTEL_METRICS_EXPORTER", "console"),
                ("OTEL_LOGS_EXPORTER", " , OTLP"),
            ],
            ConfigSwitches::default(),
        );
        let export = settings.export.expect("logs only");
        assert_eq!(export.targets.len(), 1);
        assert_eq!(export.targets[0].signal, Signal::Logs);
        let settings = run(
            &[
                endpoint,
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
                ("OTEL_EXPORTER_OTLP_METRICS_PROTOCOL", "http/protobuf"),
            ],
            ConfigSwitches::default(),
        );
        let export = settings.export.expect("metrics only");
        assert_eq!(export.targets.len(), 1);
        assert_eq!(export.targets[0].signal, Signal::Metrics);
        assert_eq!(settings.diagnostics.len(), 2);
        assert!(settings.diagnostics[0].contains("grpc"));
    }

    #[test]
    fn headers_merge_per_key_and_resource_honours_service_name() {
        let settings = run(
            &[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://h:1"),
                (
                    "OTEL_EXPORTER_OTLP_HEADERS",
                    "authorization=Bearer%20common,x-team=a,broken",
                ),
                (
                    "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
                    "authorization=Bearer%20traces",
                ),
                (
                    "OTEL_RESOURCE_ATTRIBUTES",
                    "service.name=from-attrs,deployment.environment=dev",
                ),
                ("OTEL_SERVICE_NAME", "from-name"),
                ("OTEL_LOG_LEVEL", "warn"),
            ],
            ConfigSwitches::default(),
        );
        let export = settings.export.clone().expect("export on");
        let traces = export.target(Signal::Traces).expect("traces");
        assert_eq!(
            traces.headers,
            [
                ("authorization".to_owned(), "Bearer traces".to_owned()),
                ("x-team".to_owned(), "a".to_owned())
            ]
        );
        let metrics = export.target(Signal::Metrics).expect("metrics");
        assert_eq!(metrics.headers[0].1, "Bearer common");
        assert_eq!(
            export.resource[0],
            ("service.name".to_owned(), "from-name".to_owned())
        );
        assert!(
            export
                .resource
                .contains(&("deployment.environment".to_owned(), "dev".to_owned()))
        );
        assert!(
            export
                .resource
                .contains(&("service.version".to_owned(), "1.2.3".to_owned()))
        );
        assert!(!export.info_logs);
        let status = settings.status_text();
        assert!(status.contains("traces: http://h:1/v1/traces (2 header(s))"));
        assert!(!status.contains("Bearer"));
    }

    #[test]
    fn usage_dir_override_and_config_view() {
        let settings = run(
            &[("MAHO_USAGE_DIR", "/elsewhere")],
            ConfigSwitches::default(),
        );
        assert_eq!(
            settings.ledger_location,
            Path::new("/elsewhere/usage.jsonl")
        );
        let view = serde_json::json!({"telemetry": {"enabled": true, "otlpExportEnabled": "no", "usageStats": false}});
        assert_eq!(
            ConfigSwitches::from_view(&view),
            ConfigSwitches {
                enabled: Some(true),
                otlp_export_enabled: None,
                usage_stats: Some(false)
            }
        );
    }
}
