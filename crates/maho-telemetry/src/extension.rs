//! `<builtin:telemetry>`: writes the local usage ledger and pushes OTLP data per agent run, both
//! only as far as [`Settings`] allows (docs/cleanroom/specs/telemetry.md "Surfaces"). No tools and
//! no prompt text; `/usage` prints the ledger summary.
use crate::otlp::{Exporter, Run, now_ns, run_payloads};
use crate::record::{Ledger, UsageRecord, agent_kind, now_ms};
use crate::settings::{ConfigSwitches, Settings};
use crate::stats::{report, summary_text};
use maho_ext_api::{
    AgentMessage, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, Message,
    NotificationType,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// How long the end of a run and session shutdown wait for queued exports (the host's quit
/// budget is about 2 s).
pub const SHUTDOWN_FLUSH: Duration = Duration::from_millis(1500);

/// Reads the current `telemetry.*` switches from the merged configuration view at push time.
/// `Err` means the host could not provide a view (export then fails closed).
pub type ConfigSource = Arc<dyn Fn() -> Result<ConfigSwitches, String> + Send + Sync>;

/// The telemetry extension for one process.
#[derive(Clone)]
pub struct TelemetryExtension {
    pub settings: Settings,
    /// `$MHC_TASK_DEPTH` (marks subagent records).
    pub task_depth: Option<String>,
    /// Builds the exporter on first use; `None` uses HTTP.
    pub exporter: Option<Arc<dyn Fn() -> Exporter + Send + Sync>>,
    /// The push-time configuration re-check; `None` loads the merged native configuration of the
    /// session working directory ([`native_config_source`]).
    pub config_source: Option<ConfigSource>,
    /// Receives diagnostic lines; `None` writes them to the host diagnostic stream.
    pub report: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

impl TelemetryExtension {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            task_depth: None,
            exporter: None,
            config_source: None,
            report: None,
        }
    }
}

fn report_line(line: &str) {
    maho_ext_api::diagnostic(line);
}

/// The default [`ConfigSource`]: loads the merged native configuration for `cwd` with the
/// environment captured when the extension registered (environment variables are not re-read).
/// When a configuration file cannot be read or parsed, the previous switches are kept (the host
/// keeps the previous view in that case).
pub fn native_config_source(cwd: PathBuf, env: maho_config::Env) -> ConfigSource {
    let last: Mutex<Option<ConfigSwitches>> = Mutex::new(None);
    Arc::new(move || {
        let loaded = maho_config::load_native(&cwd, &env);
        let broken = loaded.diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.kind,
                maho_config::DiagnosticKind::Read | maho_config::DiagnosticKind::Parse
            )
        });
        let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
        if broken && let Some(previous) = *last {
            return Ok(previous);
        }
        let switches = ConfigSwitches::from_view(&loaded.view);
        *last = Some(switches);
        Ok(switches)
    })
}

/// Why a push was not allowed by the push-time re-check.
fn push_blocked(source: Option<&ConfigSource>) -> Option<String> {
    let source = source?;
    match source() {
        Ok(switches) => {
            let off =
                switches.enabled == Some(false) || switches.otlp_export_enabled == Some(false);
            off.then(|| "it was turned off in the configuration".to_owned())
        }
        Err(error) => Some(format!("the configuration could not be read ({error})")),
    }
}

struct State {
    run: Option<Run>,
    exporter: Option<Exporter>,
}

pub(crate) struct Shared {
    extension: TelemetryExtension,
    cwd: String,
    state: Mutex<State>,
    ledger_failed: AtomicBool,
    discard_reported: AtomicBool,
}

impl Shared {
    pub(crate) fn new(extension: TelemetryExtension, cwd: String) -> Self {
        Self {
            extension,
            cwd,
            state: Mutex::new(State {
                run: None,
                exporter: None,
            }),
            ledger_failed: AtomicBool::new(false),
            discard_reported: AtomicBool::new(false),
        }
    }

    fn report(&self, line: &str) {
        match &self.extension.report {
            Some(report) => report(line),
            None => report_line(line),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn exporting(&self) -> bool {
        self.extension.settings.export.is_some()
    }

    fn record(&self, message: &AgentMessage, session_id: &str) -> Option<UsageRecord> {
        let AgentMessage::Llm(Message::Assistant(assistant)) = message else {
            return None;
        };
        let value = serde_json::to_value(assistant.as_ref()).ok()?;
        let mut record = UsageRecord::from_assistant(&value)?;
        if record.timestamp == 0 {
            record.timestamp = now_ms();
        }
        session_id.clone_into(&mut record.session_id);
        record.cwd.clone_from(&self.cwd);
        record.agent = agent_kind(self.extension.task_depth.as_deref()).to_owned();
        Some(record)
    }

    fn on_message_end(&self, message: &AgentMessage, session_id: &str) {
        let Some(record) = self.record(message, session_id) else {
            return;
        };
        self.on_record(&record, session_id);
    }

    /// Ledger line plus, with export on, one chat call in the current run.
    pub(crate) fn on_record(&self, record: &UsageRecord, session_id: &str) {
        if let Some(path) = &self.extension.settings.ledger
            && let Err(error) = Ledger::new(path).append(record)
            && !self.ledger_failed.swap(true, Ordering::SeqCst)
        {
            self.report(&format!(
                "usage ledger: cannot write {}: {error}",
                path.display()
            ));
        }
        if self.exporting() {
            let mut state = self.lock();
            let run = state
                .run
                .get_or_insert_with(|| Run::start(session_id, now_ns()));
            run.chat(record, now_ns());
        }
    }

    /// Ends the current run and pushes its payloads, unless the push-time re-check of the
    /// configuration says export is now off (then the payloads are discarded unsent).
    pub(crate) fn finish_run(&self) {
        let Some(config) = &self.extension.settings.export else {
            return;
        };
        let mut state = self.lock();
        let Some(mut run) = state.run.take() else {
            return;
        };
        run.finish(now_ns());
        let payloads = run_payloads(config, &run);
        if let Some(reason) = push_blocked(self.extension.config_source.as_ref()) {
            drop(state);
            if !self.discard_reported.swap(true, Ordering::SeqCst) {
                self.report(&format!(
                    "telemetry export skipped because {reason}; discarded {} payload(s)",
                    payloads.len()
                ));
            }
            return;
        }
        let report = self.extension.report.clone();
        let exporter =
            state
                .exporter
                .get_or_insert_with(|| match &self.extension.exporter {
                    Some(make) => make(),
                    None => Exporter::http(report.unwrap_or_else(|| {
                        Arc::new(report_line) as Arc<dyn Fn(&str) + Send + Sync>
                    })),
                });
        for (target, body) in payloads {
            exporter.send(&target, body);
        }
    }

    pub(crate) fn flush(&self) {
        self.finish_run();
        let state = self.lock();
        if let Some(exporter) = &state.exporter {
            exporter.flush(SHUTDOWN_FLUSH);
        }
    }
}

/// The ledger summary shown by `/usage` (also used by `mhc usage`).
pub fn ledger_summary(ledger: Option<&PathBuf>, location: &std::path::Path) -> String {
    let Some(path) = ledger else {
        return format!("The usage ledger is off ({}).", location.display());
    };
    match Ledger::new(path).read() {
        Ok(records) => summary_text(&report("ledger", &records, None)),
        Err(error) => format!("Cannot read {}: {error}", path.display()),
    }
}

impl Extension for TelemetryExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let mut extension = self.clone();
        if extension.settings.export.is_some() && extension.config_source.is_none() {
            extension.config_source = Some(native_config_source(
                api.cwd.clone(),
                maho_config::process_env(),
            ));
        }
        let shared = Arc::new(Shared::new(
            extension,
            api.cwd.to_string_lossy().into_owned(),
        ));
        for line in &self.settings.diagnostics {
            shared.report(line);
        }
        let ledger = self.settings.ledger.clone();
        let location = self.settings.ledger_location.clone();
        api.register_command(
            "usage",
            Some("Model usage, tokens and estimated cost from the local usage ledger".to_owned()),
            None,
            Arc::new(move |_raw, ctx| {
                let text = ledger_summary(ledger.as_ref(), &location);
                Box::pin(async move {
                    ctx.ui.notify(&text, NotificationType::Info);
                    Ok(())
                })
            }),
        );
        if self.settings.ledger.is_none() && self.settings.export.is_none() {
            return;
        }
        let on_message = Arc::clone(&shared);
        api.on(
            EventKind::MessageEnd,
            Arc::new(move |event, ctx| {
                let shared = Arc::clone(&on_message);
                if let ExtensionEvent::MessageEnd { message } = event {
                    shared.on_message_end(message, ctx.session_manager.session_id());
                }
                Box::pin(async move { Ok(EventResult::None) })
            }),
        );
        if self.settings.export.is_none() {
            return;
        }
        let on_start = Arc::clone(&shared);
        api.on(
            EventKind::AgentStart,
            Arc::new(move |_event, ctx| {
                let mut state = on_start.lock();
                if state.run.is_none() {
                    state.run = Some(Run::start(ctx.session_manager.session_id(), now_ns()));
                }
                drop(state);
                Box::pin(async move { Ok(EventResult::None) })
            }),
        );
        let on_tool_start = Arc::clone(&shared);
        api.on(
            EventKind::ToolExecutionStart,
            Arc::new(move |event, ctx| {
                if let ExtensionEvent::ToolExecutionStart {
                    tool_call_id,
                    tool_name,
                    ..
                } = event
                {
                    let mut state = on_tool_start.lock();
                    state
                        .run
                        .get_or_insert_with(|| {
                            Run::start(ctx.session_manager.session_id(), now_ns())
                        })
                        .tool_start(tool_call_id, tool_name, now_ns());
                }
                Box::pin(async move { Ok(EventResult::None) })
            }),
        );
        let on_tool_end = Arc::clone(&shared);
        api.on(
            EventKind::ToolExecutionEnd,
            Arc::new(move |event, ctx| {
                if let ExtensionEvent::ToolExecutionEnd {
                    tool_call_id,
                    tool_name,
                    is_error,
                    ..
                } = event
                {
                    let mut state = on_tool_end.lock();
                    state
                        .run
                        .get_or_insert_with(|| {
                            Run::start(ctx.session_manager.session_id(), now_ns())
                        })
                        .tool_end(tool_call_id, tool_name, *is_error, now_ns());
                }
                Box::pin(async move { Ok(EventResult::None) })
            }),
        );
        let on_end = Arc::clone(&shared);
        api.on(
            EventKind::AgentEnd,
            Arc::new(move |_event, _ctx| {
                // Push at the end of every run (oh-my-pi flushes at natural boundaries), so
                // one-shot `mhc -p` runs export before the process exits.
                let shared = Arc::clone(&on_end);
                Box::pin(async move {
                    let flush = move || shared.flush();
                    match tokio::runtime::Handle::try_current() {
                        Ok(handle) => {
                            let _ = handle.spawn_blocking(flush).await;
                        }
                        Err(_) => flush(),
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let on_shutdown = Arc::clone(&shared);
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_event, _ctx| {
                let shared = Arc::clone(&on_shutdown);
                Box::pin(async move {
                    let flush = move || shared.flush();
                    match tokio::runtime::Handle::try_current() {
                        Ok(handle) => {
                            let _ = handle.spawn_blocking(flush).await;
                        }
                        Err(_) => flush(),
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
