//! Port of senpi `anthropic-subscription/claude-cli-process.ts` (v2026.11.19-5): the short-command runner.
//! `stopClaudeCli` and the browser argument serve the interactive `claude auth login`, which this port does not carry.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

/// One `claude auth *` invocation for a CLI-store account.
pub struct ClaudeCliRun<'a> {
    /// The executable the turn path resolves, so sign-in and turns use the same binary.
    pub executable: &'a Path,
    pub config_dir: PathBuf,
    pub parent_env: &'a BTreeMap<String, String>,
}

pub struct ClaudeCliExit {
    pub exit_code: Option<i32>,
    pub stdout: String,
}

pub const DEFAULT_SHORT_COMMAND_TIMEOUT_MS: u64 = 10_000;

/// Runs a short command (`auth status`, `auth logout`) to exit, killing it at the deadline or when `signal` aborts
/// (both answer `None`). Never logs its output.
pub async fn run_claude_cli_to_exit(
    run: &ClaudeCliRun<'_>,
    args: &[&str],
    timeout_ms: u64,
    signal: Option<&maho_ai::utils::abort::AbortSignal>,
) -> Option<ClaudeCliExit> {
    if signal.is_some_and(maho_ai::utils::abort::AbortSignal::aborted) {
        return None;
    }
    let mut command = tokio::process::Command::new(run.executable);
    command
        .args(args)
        .current_dir(&run.config_dir)
        .env_clear()
        .envs(crate::claude_cli_environment::build_claude_cli_environment(
            run.parent_env,
            &run.config_dir,
            None,
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = command.output();
    let bounded = tokio::time::timeout(Duration::from_millis(timeout_ms), output);
    let finished = match signal {
        Some(signal) => maho_ai::utils::abort::race_with_abort_signal(bounded, signal)
            .await
            .ok()?,
        None => bounded.await,
    };
    let output = finished.ok()?.ok()?;
    Some(ClaudeCliExit {
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}
