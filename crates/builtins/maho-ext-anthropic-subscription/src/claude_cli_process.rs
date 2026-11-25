//! Port of senpi `anthropic-subscription/claude-cli-process.ts` (v2026.11.19-5).
use crate::claude_cli_auth::{ClaudeCliLoginFailure, ClaudeCliLoginResult};
use maho_ai::utils::abort::{AbortSignal, race_with_abort_signal};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep_until, timeout_at},
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

/// Await the login worker, or abort the worker and drop its owned child with this handle.
pub struct ClaudeCliLoginTask(pub(crate) JoinHandle<ClaudeCliLoginResult>);

impl Future for ClaudeCliLoginTask {
    type Output = ClaudeCliLoginResult;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx).map(|result| {
            result.unwrap_or(ClaudeCliLoginResult::Failed(
                ClaudeCliLoginFailure::UnexpectedExit,
            ))
        })
    }
}

impl Drop for ClaudeCliLoginTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) fn spawn_claude_cli(
    run: &ClaudeCliRun<'_>,
    args: &[&str],
    browser: Option<&str>,
) -> std::io::Result<Child> {
    let mut command = Command::new(run.executable);
    command
        .args(args)
        .current_dir(&run.config_dir)
        .env_clear()
        .envs(crate::claude_cli_environment::build_claude_cli_environment(
            run.parent_env,
            &run.config_dir,
            browser,
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(if browser.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    command.spawn()
}

/// Ask only this child to exit, then force termination and reap it if it ignores SIGTERM.
pub(crate) async fn stop_claude_cli(child: &mut Child, grace: Duration) {
    #[cfg(unix)]
    {
        use nix::{
            sys::signal::{Signal, kill},
            unistd::Pid,
        };

        if matches!(child.try_wait(), Ok(None))
            && let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok())
        {
            let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
        }
        if let Ok(Ok(_)) = tokio::time::timeout(grace, child.wait()).await {
            return;
        }
    }
    #[cfg(not(unix))]
    let _ = grace; // Win32 has no graceful SIGTERM for a hidden CLI process.
    // Tokio's kill also waits for the child.
    let _ = child.kill().await;
}

/// Writes complete stdin lines and returns only at the idle deadline; a paste renews that deadline.
pub(crate) async fn write_codes(
    stdin: &mut ChildStdin,
    codes: &mut mpsc::Receiver<String>,
    idle: Duration,
) {
    let mut deadline = Instant::now() + idle;
    while let Ok(Some(mut code)) = timeout_at(deadline, codes.recv()).await {
        deadline = Instant::now() + idle;
        code.push('\n');
        if !matches!(
            timeout_at(deadline, stdin.write_all(code.as_bytes())).await,
            Ok(Ok(()))
        ) {
            codes.close();
            break;
        }
    }
    sleep_until(deadline).await;
}

/// Runs a short command (`auth status`, `auth logout`) to exit, killing it at the deadline or when `signal` aborts
/// (both answer `None`). Never logs its output.
pub async fn run_claude_cli_to_exit(
    run: &ClaudeCliRun<'_>,
    args: &[&str],
    timeout_ms: u64,
    signal: Option<&AbortSignal>,
) -> Option<ClaudeCliExit> {
    if signal.is_some_and(AbortSignal::aborted) {
        return None;
    }
    let mut child = spawn_claude_cli(run, args, None).ok()?;
    drop(child.stdin.take());
    let mut stdout = child.stdout.take()?;
    let mut bytes = Vec::new();
    let status = {
        let output = async { tokio::try_join!(child.wait(), stdout.read_to_end(&mut bytes)) };
        let bounded = tokio::time::timeout(Duration::from_millis(timeout_ms), output);
        match signal {
            Some(signal) => race_with_abort_signal(bounded, signal).await.ok(),
            None => Some(bounded.await),
        }
    };
    let Some(Ok(Ok((status, _)))) = status else {
        let _ = child.kill().await;
        return None;
    };
    Some(ClaudeCliExit {
        exit_code: status.code(),
        stdout: String::from_utf8_lossy(&bytes).into_owned(),
    })
}
