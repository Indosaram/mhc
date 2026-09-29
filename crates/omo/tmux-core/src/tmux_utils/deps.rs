//! Injected dependencies shared by the lifecycle functions.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::cmux_detect::is_cmux_compat_environment;
use crate::env_source::{EnvSource, ProcessEnv};
use crate::runner::{RunTmuxOptions, TmuxCommandResult, run_tmux_command};
use crate::tmux_utils::environment::is_inside_tmux_environment;
use crate::tmux_utils::server_health::{IsServerRunningOptions, is_server_running};

pub type LogFn = Arc<dyn Fn(&str, Option<Value>) + Send + Sync>;
pub type RunTmuxCommandFn = Arc<dyn Fn(&str, &[String]) -> TmuxCommandResult + Send + Sync>;
pub type GetTmuxPathFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;
pub type PredicateFn = Arc<dyn Fn() -> bool + Send + Sync>;
pub type ServerRunningFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;
pub type DelayFn = Arc<dyn Fn(u64) + Send + Sync>;

/// Dependencies for every pane/window/session lifecycle function.
///
/// `Default` mirrors the TypeScript defaults: no-op log, the real runner, the
/// real server health check, a tmux path resolver that returns `None`, and
/// environment-derived tmux/cmux detection.
#[derive(Clone)]
pub struct TmuxDeps {
    pub log: LogFn,
    pub run_tmux_command: RunTmuxCommandFn,
    /// `None` derives the answer from [`Self::environment`] (`TMUX` set).
    pub is_inside_tmux: Option<PredicateFn>,
    pub is_server_running: ServerRunningFn,
    pub get_tmux_path: GetTmuxPathFn,
    /// `None` derives the answer from [`Self::environment`].
    pub is_cmux_compat_environment: Option<PredicateFn>,
    /// Environment read for auth args and default tmux/cmux detection.
    pub environment: Arc<dyn EnvSource>,
    pub delay: DelayFn,
}

impl Default for TmuxDeps {
    fn default() -> Self {
        Self {
            log: Arc::new(|_, _| {}),
            run_tmux_command: Arc::new(|tmux, args| {
                run_tmux_command(tmux, args, &RunTmuxOptions::default())
            }),
            is_inside_tmux: None,
            is_server_running: Arc::new(|url| {
                is_server_running(url, &mut IsServerRunningOptions::default())
            }),
            get_tmux_path: Arc::new(|| None),
            is_cmux_compat_environment: None,
            environment: Arc::new(ProcessEnv),
            delay: Arc::new(|milliseconds| std::thread::sleep(Duration::from_millis(milliseconds))),
        }
    }
}

impl TmuxDeps {
    pub(crate) fn inside_tmux(&self) -> bool {
        self.is_inside_tmux.as_ref().map_or_else(
            || is_inside_tmux_environment(&*self.environment),
            |check| check(),
        )
    }

    pub(crate) fn cmux_compat(&self) -> bool {
        self.is_cmux_compat_environment.as_ref().map_or_else(
            || is_cmux_compat_environment(&*self.environment),
            |check| check(),
        )
    }

    pub(crate) fn run(&self, tmux: &str, args: &[String]) -> TmuxCommandResult {
        (self.run_tmux_command)(tmux, args)
    }

    pub(crate) fn log(&self, message: &str, data: Option<Value>) {
        (self.log)(message, data);
    }

    /// Set the `omo-subagent-<description[..20]>` pane title, logging a warning on failure.
    pub(crate) fn set_subagent_title(
        &self,
        tag: &str,
        tmux: &str,
        pane_id: &str,
        description: &str,
    ) {
        let title = format!(
            "omo-subagent-{}",
            description.chars().take(20).collect::<String>()
        );
        let result = self.run(
            tmux,
            &strings(&["select-pane", "-t", pane_id, "-T", &title]),
        );
        if result.exit_code != 0 {
            self.log(
                &format!("[{tag}] WARNING: failed to set pane title"),
                Some(json!({
                    "paneId": pane_id,
                    "title": title,
                    "exitCode": result.exit_code,
                    "stderr": result.stderr.trim(),
                })),
            );
        }
    }
}

pub(crate) fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

pub type SpawnTmuxPaneDeps = TmuxDeps;
pub type SpawnTmuxWindowDeps = TmuxDeps;
pub type SpawnTmuxSessionDeps = TmuxDeps;
pub type ReplaceTmuxPaneDeps = TmuxDeps;
pub type ActivateTmuxPaneDeps = TmuxDeps;
pub type CloseTmuxPaneDependencies = TmuxDeps;
pub type KillTmuxSessionDeps = TmuxDeps;
pub type GetPaneDimensionsDeps = TmuxDeps;
pub type EnforceMainPaneWidthDeps = TmuxDeps;
