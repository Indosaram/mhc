use crate::manager::TerminalManager;
use maho_ai::types::{TextAudience, TextContent};
use maho_pty::PtyExit;
use maho_tools::definition::{ToolContent, ToolResult};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
/// What a background exit notice reads: on the tool child it is a summary the child sent, not the live session.
#[derive(Clone)]
pub struct TerminalExitView {
    pub exit_result: Option<PtyExit>,
    /// Set when the opt-in auto-reap stopped the session at this age (`senpi#6201`).
    pub auto_reaped_after_minutes: Option<u64>,
    /// Set when `interrupt { background: "stop" }` on turn `turn_id` stopped the session.
    pub stopped_by_interrupt: Option<String>,
    full_output: String,
}
impl TerminalExitView {
    pub fn new(exit_result: Option<PtyExit>, full_output: String) -> Self {
        Self {
            exit_result,
            auto_reaped_after_minutes: None,
            stopped_by_interrupt: None,
            full_output,
        }
    }
    pub fn with_auto_reaped_after_minutes(mut self, minutes: u64) -> Self {
        self.auto_reaped_after_minutes = Some(minutes);
        self
    }
    pub fn with_stopped_by_interrupt(mut self, turn_id: String) -> Self {
        self.stopped_by_interrupt = Some(turn_id);
        self
    }
    pub fn full_output(&self) -> &str {
        &self.full_output
    }
    /// Snapshot of a live runtime session (upstream passes the session itself, which satisfies the view).
    pub fn of_runtime(runtime: &crate::runtime_session::TerminalRuntimeSession) -> Self {
        Self::new(
            runtime.exit_result().ok().flatten(),
            runtime.full_output().unwrap_or_default(),
        )
    }
    /// Snapshot from a completion handle (survives registry eviction / disposal).
    pub fn of_completion(handle: &crate::runtime_session::TerminalCompletionHandle) -> Self {
        handle.exit_view()
    }
}
/// Writes bytes into a session's PTY.
pub type PtyWriteFn = dyn Fn(&[u8]) -> Result<(), String> + Send + Sync;
/// A PTY session's exit as the launch barrier watches it: unset while running.
pub type PtyExitState = Option<Result<PtyExit, String>>;
/// Registers a session `(id, description, started_at_ms, command, auto_reap_minutes)` when it turns background.
pub type BackgroundStartFn = dyn Fn(&str, &str, f64, Option<&str>, Option<u64>) + Send + Sync;
/// A person or the model wrote to a session's stdin (`bash_input`): it is interactive from now on.
pub type SessionInputFn = dyn Fn(&str) + Send + Sync;
/// Notified with a background session's id and exit.
pub type BackgroundExitFn = dyn Fn(&str, TerminalExitView) + Send + Sync;
/// The opt-in auto-reap's one stop at `auto_reap_minutes` did not end the session.
pub type BackgroundReapFailedFn = dyn Fn(&str, u64, &str) + Send + Sync;
/// Optional warning logger callback `(event_name, fields)` for stalled spawns and auto-reap failures.
pub type SessionWarnFn = dyn Fn(&str, Value) + Send + Sync;

/// What the launch barrier sees of a started session (upstream passes `{ pid, write, onExit }`): its pid, a writer
/// into the PTY, and the exit watch `onExit` listens on.
pub struct TerminalLaunchSession {
    pub pid: Option<u32>,
    pub write: Arc<PtyWriteFn>,
    pub exit: tokio::sync::watch::Receiver<PtyExitState>,
}
pub type TerminalBarrierFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
/// The tool child's execution barrier for terminal launches (senpi#3566 todo 9): `wrap` rewrites the launch behind a
/// go-line shim, and `release` records the started session with the server and only then lets the command run.
/// Absent on the in-process path, which spawns exactly as before.
pub trait TerminalLaunchBarrier: Send + Sync {
    fn wrap(&self, command: &str, args: &[String]) -> (String, Vec<String>);
    fn release(&self, session: TerminalLaunchSession) -> TerminalBarrierFuture<'_>;
}
/// What a command launched on the tool child runs with: the barrier, the environment the server built (the shell
/// environment with the session's PI_* overrides), and the cwd of the call.
#[derive(Clone)]
pub struct TerminalChildLaunch {
    pub barrier: Arc<dyn TerminalLaunchBarrier>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
}
/// Shared terminal configuration the tool context reads live: resolved settings and the explicit shell path.
pub type TerminalConfiguration =
    Arc<Mutex<(crate::settings::ResolvedTerminalSettings, Option<String>)>>;
/// Shared dependencies handed to the child-placed tool jobs (`tools/context.ts` `TerminalToolContext`, the fields the
/// placement reads): the live configuration, the session cwd, the environment resolver, and the background hooks.
#[derive(Clone)]
pub struct TerminalToolContext {
    pub configuration: TerminalConfiguration,
    /// Current extension session cwd (`state.ctx?.cwd`), set on session start.
    pub session_cwd: Arc<Mutex<Option<PathBuf>>>,
    /// Resolve the environment for spawned sessions (mirrors core bash `getShellEnv`).
    pub get_env: Arc<dyn Fn() -> BTreeMap<String, String> + Send + Sync>,
    /// Registers a session when it transitions to background liveness.
    pub on_background_start: Option<Arc<BackgroundStartFn>>,
    /// A person or the model wrote to a session's stdin (`bash_input`): it is interactive from now on.
    pub on_session_input: Option<Arc<SessionInputFn>>,
    /// Notified when a background session exits, so the notify layer can wake the agent.
    pub on_background_exit: Option<Arc<BackgroundExitFn>>,
    /// The opt-in auto-reap's one stop at `auto_reap_minutes` did not end the session.
    pub on_background_reap_failed: Option<Arc<BackgroundReapFailedFn>>,
    /// Optional warning sink for `bash_spawn_stalled` and `bash_auto_reap_failed`.
    pub session_logger: Option<Arc<SessionWarnFn>>,
}
impl TerminalToolContext {
    pub fn new(
        configuration: TerminalConfiguration,
        get_env: Arc<dyn Fn() -> BTreeMap<String, String> + Send + Sync>,
    ) -> Self {
        Self {
            configuration,
            session_cwd: Arc::new(Mutex::new(None)),
            get_env,
            on_background_start: None,
            on_session_input: None,
            on_background_exit: None,
            on_background_reap_failed: None,
            session_logger: None,
        }
    }
    /// `state.ctx?.cwd ?? process.cwd()`.
    pub fn cwd(&self) -> PathBuf {
        self.session_cwd
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
    }
    pub fn shell_path(&self) -> Option<String> {
        self.configuration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
            .clone()
    }
    pub fn settings(&self) -> crate::settings::ResolvedTerminalSettings {
        self.configuration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
            .clone()
    }
}
#[derive(Debug)]
pub struct TerminalToolResult {
    pub content: Vec<TextContent>,
    pub details: Option<Map<String, Value>>,
    pub is_error: Option<bool>,
}
impl TerminalToolResult {
    /// The tool result a caller receives (model-only parts keep their audience); `isError` rides along.
    pub fn into_tool_result(self) -> ToolResult {
        ToolResult {
            content: self
                .content
                .into_iter()
                .map(|part| ToolContent::Text {
                    text: part.text,
                    audience: part.audience.map(|_| "model".to_owned()),
                })
                .collect(),
            details: self.details.map(Value::Object),
            structured_content: None,
            is_error: self.is_error,
        }
    }
}
pub fn text_result(text: impl Into<String>) -> TerminalToolResult {
    TerminalToolResult {
        content: vec![TextContent {
            text: text.into(),
            audience: None,
            text_signature: None,
        }],
        details: None,
        is_error: None,
    }
}
pub fn error_result(text: impl Into<String>) -> TerminalToolResult {
    let mut result = text_result(text);
    result.is_error = Some(true);
    result
}
pub fn noticed_result(text: &str, notices: &[Option<&str>]) -> TerminalToolResult {
    TerminalToolResult {
        content: crate::output_format::split_model_only_notices(text, notices),
        details: None,
        is_error: None,
    }
}
pub fn model_only_part(text: impl Into<String>) -> TextContent {
    TextContent {
        text: text.into(),
        audience: Some(TextAudience::Model),
        text_signature: None,
    }
}
pub fn resolve_terminal_id(manager: &TerminalManager, id: &str) -> String {
    manager.resolve_id(id).unwrap_or_else(|| id.to_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolve_keeps_runtime_ids_and_falls_back_for_unseen_monitor_ids() {
        let mut manager = TerminalManager::new(1);
        manager.bind_monitor_id("mon_1", "bash_9");
        assert_eq!(resolve_terminal_id(&manager, "mon_1"), "bash_9");
        assert_eq!(resolve_terminal_id(&manager, "bash_9"), "bash_9");
        assert_eq!(resolve_terminal_id(&manager, "mon_missing"), "mon_missing");
    }
}
