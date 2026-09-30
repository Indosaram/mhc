//! Port of senpi packages/coding-agent/src/core/exec.ts.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    pub timeout_ms: Option<u64>,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
    pub killed: bool,
}

pub const EXIT_STDIO_GRACE_MS: u64 = 250;
pub const ABORT_EXIT_GRACE_MS: u64 = 5_000;
pub const FORCE_KILL_AFTER_SIGTERM_MS: u64 = 5_000;

pub async fn exec_command(command: &str, args: &[String], cwd: &str, options: &ExecOptions) -> ExecResult {
    let mut process = tokio::process::Command::new(command);
    process.args(args);
    process.current_dir(options.cwd.as_deref().unwrap_or(cwd));
    process.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(env) = &options.env {
        for (key, value) in env {
            process.env(key, value);
        }
    }
    process.kill_on_drop(true);

    let child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ExecResult { stdout: String::new(), stderr: error.to_string(), code: 1, killed: false };
        }
    };

    let killed = Arc::new(AtomicBool::new(false));
    let wait = child.wait_with_output();
    let outcome = match options.timeout_ms.filter(|timeout| *timeout > 0) {
        Some(timeout_ms) => match tokio::time::timeout(Duration::from_millis(timeout_ms), wait).await {
            Ok(result) => result,
            Err(_) => {
                killed.store(true, Ordering::SeqCst);
                return ExecResult { stdout: String::new(), stderr: String::new(), code: 0, killed: true };
            }
        },
        None => wait.await,
    };

    match outcome {
        Ok(output) => ExecResult {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            code: output.status.code().unwrap_or(0),
            killed: killed.load(Ordering::SeqCst),
        },
        Err(_) => ExecResult { stdout: String::new(), stderr: String::new(), code: 1, killed: false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_stdout_stderr_and_exit_code() {
        let result = exec_command(
            "sh",
            &["-c".to_owned(), "printf out; printf err >&2; exit 3".to_owned()],
            ".",
            &ExecOptions::default(),
        )
        .await;
        assert_eq!(result.stdout, "out");
        assert_eq!(result.stderr, "err");
        assert_eq!(result.code, 3);
        assert!(!result.killed);
    }

    #[tokio::test]
    async fn reports_a_missing_binary_instead_of_panicking() {
        let result = exec_command("/definitely/not/a/binary", &[], ".", &ExecOptions::default()).await;
        assert_eq!(result.code, 1);
        assert!(!result.stderr.is_empty());
    }

    #[tokio::test]
    async fn kills_the_command_when_the_timeout_elapses() {
        let options = ExecOptions { timeout_ms: Some(100), ..ExecOptions::default() };
        let result = exec_command("sh", &["-c".to_owned(), "sleep 30".to_owned()], ".", &options).await;
        assert!(result.killed);
    }
}
