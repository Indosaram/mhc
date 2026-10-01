//! Port of senpi packages/coding-agent/src/core/resolve-config-command.ts.
//!
//! Runs a !command config value (credential helper, header broker) off the event loop. Every wait
//! is asynchronous: an RPC host runs every session on one loop, so a blocking spawn or backoff
//! freezes every other session.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;

const COMMAND_EXECUTION_MAX_ATTEMPTS: usize = 3;
const COMMAND_EXECUTION_BACKOFF_MS: [u64; 2] = [250, 1000];
const COMMAND_TIMEOUT_MS: u64 = 10_000;
const COMMAND_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// Separates "the shell ran and produced nothing" from "the shell is missing".
    pub executed: bool,
    pub value: Option<String>,
}

async fn run_command_process(command: &str, env: &[(String, String)]) -> CommandOutcome {
    let mut process = tokio::process::Command::new("/bin/sh");
    process.arg("-c").arg(command);
    process.stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null());
    for (key, value) in env {
        process.env(key, value);
    }
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CommandOutcome { executed: error.kind() != std::io::ErrorKind::NotFound, value: None };
        }
    };
    let mut stdout = child.stdout.take();
    let read = async {
        let mut buffer = Vec::new();
        if let Some(stdout) = stdout.as_mut() {
            let mut chunk = vec![0u8; 8192];
            loop {
                match stdout.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(count) => {
                        buffer.extend_from_slice(&chunk[..count]);
                        if buffer.len() > COMMAND_MAX_OUTPUT_BYTES {
                            return None;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        Some(buffer)
    };

    match tokio::time::timeout(Duration::from_millis(COMMAND_TIMEOUT_MS), read).await {
        Ok(Some(buffer)) => {
            let status = child.wait().await.ok();
            let text = String::from_utf8_lossy(&buffer).trim().to_owned();
            let value = status.filter(|status| status.success()).and_then(|_| if text.is_empty() { None } else { Some(text) });
            CommandOutcome { executed: true, value }
        }
        Ok(None) => {
            let _ = child.start_kill();
            CommandOutcome { executed: true, value: None }
        }
        Err(_) => {
            let _ = child.start_kill();
            CommandOutcome { executed: true, value: None }
        }
    }
}

async fn execute_command_once(command: &str, env: &[(String, String)]) -> Option<String> {
    run_command_process(command, env).await.value
}

/// Executes a !command config value, retrying a transient failure with an awaited backoff.
pub async fn run_config_command(command_config: &str, env: &[(String, String)]) -> Option<String> {
    let command = command_config.strip_prefix('!').unwrap_or(command_config);
    for attempt in 0..COMMAND_EXECUTION_MAX_ATTEMPTS {
        if let Some(value) = execute_command_once(command, env).await {
            return Some(value);
        }
        if let Some(backoff) = COMMAND_EXECUTION_BACKOFF_MS.get(attempt) {
            tokio::time::sleep(Duration::from_millis(*backoff)).await;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_command_that_prints_a_value_returns_it() {
        assert_eq!(run_config_command("!echo token-value", &[]).await.as_deref(), Some("token-value"));
    }

    #[tokio::test]
    async fn a_command_that_prints_nothing_returns_none() {
        assert_eq!(run_config_command("!true", &[]).await, None);
    }

    #[tokio::test]
    async fn a_failing_command_returns_none() {
        assert_eq!(run_config_command("!exit 1", &[]).await, None);
    }

    #[tokio::test]
    async fn a_command_reads_the_passed_environment() {
        let env = vec![("MAHO_TEST_KEY".to_owned(), "env-value".to_owned())];
        assert_eq!(run_config_command("!printenv MAHO_TEST_KEY", &env).await.as_deref(), Some("env-value"));
    }
}
