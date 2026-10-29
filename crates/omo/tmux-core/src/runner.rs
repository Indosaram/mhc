//! Tmux command runner with retry, timeout and cmux compatibility.

use std::sync::Arc;

use serde::Serialize;
use utils::{ProcessTreeRunOptions, SpawnOptions, StdioMode, run_process_with_tree_timeout, spawn};

use crate::cmux_detect::is_cmux_compat_environment;
use crate::env_source::{EnvSource, ProcessEnv};

/// Options for [`run_tmux_command`].
#[derive(Clone, Default)]
pub struct RunTmuxOptions {
    /// Extra attempts after the first failure (terminal tmux errors are never retried).
    pub retry: u32,
    /// Kill the command after this many milliseconds and report a timeout.
    pub timeout_ms: Option<u64>,
    /// Environment used for cmux detection; defaults to the process environment.
    pub environment: Option<Arc<dyn EnvSource>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TmuxCommandResult {
    pub success: bool,
    pub output: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl TmuxCommandResult {
    #[must_use]
    pub fn new(stdout: &str, stderr: &str, exit_code: i32) -> Self {
        Self {
            success: exit_code == 0,
            output: stdout.to_owned(),
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            exit_code,
        }
    }
}

fn is_terminal_tmux_error(stderr: &str) -> bool {
    let lowered = stderr.to_lowercase();
    lowered.contains("can't find pane") || lowered.contains("can't find session")
}

fn is_cmux_executable_name(name: &str) -> bool {
    let lowered = name.to_lowercase();
    lowered == "cmux"
        || [".bat", ".cmd", ".exe", ".ps1"]
            .iter()
            .any(|extension| lowered.strip_prefix("cmux") == Some(extension))
}

fn resolve_tmux_executable(tmux_path: &str, environment: &dyn EnvSource) -> Vec<String> {
    if !is_cmux_compat_environment(environment) {
        return vec![tmux_path.to_owned()];
    }
    let executable_name = tmux_path.rsplit(['/', '\\']).next().unwrap_or_default();
    let cmux_executable = if is_cmux_executable_name(executable_name) {
        tmux_path
    } else {
        "cmux"
    };
    vec![cmux_executable.to_owned(), "__tmux-compat".to_owned()]
}

fn spawn_failure(error: &std::io::Error) -> TmuxCommandResult {
    TmuxCommandResult::new("", &error.to_string(), 1)
}

fn run_without_timeout(command: &[String]) -> TmuxCommandResult {
    let options = SpawnOptions {
        stdout: Some(StdioMode::Pipe),
        stderr: Some(StdioMode::Pipe),
        ..SpawnOptions::default()
    };
    match spawn(command, &options).and_then(utils::SpawnedProcess::wait_with_output) {
        Ok((exit_code, stdout, stderr)) => {
            TmuxCommandResult::new(stdout.trim(), stderr.trim(), exit_code)
        }
        Err(error) => spawn_failure(&error),
    }
}

fn run_with_timeout(command: &[String], timeout_ms: u64) -> TmuxCommandResult {
    let Some((program, args)) = command.split_first() else {
        return TmuxCommandResult::new("", "spawn requires a command", 1);
    };
    let result = run_process_with_tree_timeout(&ProcessTreeRunOptions {
        command: program.clone(),
        args: args.to_vec(),
        cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
        env: std::env::vars().collect(),
        max_buffer: usize::MAX / 2,
        timeout_ms,
        termination_grace_ms: Some(100),
        termination_wait_ms: None,
        on_termination_report: None,
    });
    if result.timed_out {
        return TmuxCommandResult::new("", "timeout", -1);
    }
    TmuxCommandResult::new(result.stdout.trim(), result.stderr.trim(), result.exit_code)
}

/// Run `tmux_path args...` (or `cmux __tmux-compat args...` under cmux) and
/// capture trimmed stdout/stderr. Spawn failures surface as `exit_code: 1` with
/// the OS error in `stderr`; timeouts as `exit_code: -1` and `stderr: "timeout"`.
pub fn run_tmux_command(
    tmux_path: &str,
    args: &[String],
    options: &RunTmuxOptions,
) -> TmuxCommandResult {
    let environment: Arc<dyn EnvSource> = options
        .environment
        .clone()
        .unwrap_or_else(|| Arc::new(ProcessEnv));
    let mut command = resolve_tmux_executable(tmux_path, &*environment);
    command.extend_from_slice(args);

    let mut attempt = 0;
    loop {
        let result = match options.timeout_ms {
            None => run_without_timeout(&command),
            Some(timeout_ms) => run_with_timeout(&command, timeout_ms),
        };
        if result.exit_code == 0
            || attempt == options.retry
            || is_terminal_tmux_error(&result.stderr)
        {
            return result;
        }
        attempt += 1;
    }
}
