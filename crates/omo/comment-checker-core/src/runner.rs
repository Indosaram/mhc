//! Comment-checker binary resolution and invocation (port of `src/runner.ts`).

use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use url::Url;

use crate::types::ByteStream;
use crate::types::CheckFailure;
use crate::types::CheckResult;
use crate::types::ResolveCommentCheckerBinaryInput;
use crate::types::RunCommentCheckerInput;
use crate::types::RunCommentCheckerOptions;
use crate::types::SpawnSignal;

pub const DEFAULT_PACKAGE_NAME: &str = "@code-yeongyu/comment-checker";
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_KILL_GRACE_MS: u64 = 1_000;
const MAX_FAILURE_STDERR: usize = 500;

fn normalize_message(message: &str) -> String {
    message.replace("\r\n", "\n")
}

fn failed(exit_code: Option<i32>, stderr: &str) -> CheckResult {
    let normalized = normalize_message(stderr);
    let trimmed = normalized.trim();
    let truncated: String = trimmed.chars().take(MAX_FAILURE_STDERR).collect();
    CheckResult {
        has_comments: false,
        message: String::new(),
        failure: Some(CheckFailure {
            exit_code,
            stderr: truncated,
        }),
    }
}

/// Cached path if it still exists, else `<package dir>/bin/<binary_name>` found by
/// Node-style `node_modules` lookup from `import_meta_url`.
pub fn resolve_comment_checker_binary(
    input: &ResolveCommentCheckerBinaryInput<'_>,
) -> Option<String> {
    let package_name = input.package_name.unwrap_or(DEFAULT_PACKAGE_NAME);

    if let Some(cached) = input.cached_binary_path
        && (input.exists_sync)(cached)
    {
        return Some(cached.to_string());
    }

    let base = require_base_dir(input.import_meta_url?)?;
    let package_json = resolve_package_json(&base, package_name)?;
    let binary_path = package_json.parent()?.join("bin").join(input.binary_name);
    let binary_path = binary_path.to_str()?.to_string();
    (input.exists_sync)(&binary_path).then_some(binary_path)
}

fn require_base_dir(import_meta_url: &str) -> Option<PathBuf> {
    let path = if import_meta_url.starts_with("file:") {
        Url::parse(import_meta_url).ok()?.to_file_path().ok()?
    } else {
        PathBuf::from(import_meta_url)
    };
    if !path.is_absolute() {
        return None;
    }
    if import_meta_url.ends_with('/') {
        Some(path)
    } else {
        path.parent().map(Path::to_path_buf)
    }
}

fn resolve_package_json(base: &Path, package_name: &str) -> Option<PathBuf> {
    base.ancestors()
        .filter(|dir| dir.file_name().is_none_or(|name| name != "node_modules"))
        .map(|dir| {
            dir.join("node_modules")
                .join(package_name)
                .join("package.json")
        })
        .find(|candidate| candidate.is_file())
        .map(|found| std::fs::canonicalize(&found).unwrap_or(found))
}

async fn read_text(mut stream: ByteStream) -> io::Result<String> {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.strip_prefix('﻿').unwrap_or(&text).to_string())
}

enum ExitOutcome {
    Exited(i32),
    NotStarted(String),
}

/// Pipes the hook input JSON to `<binary> check [--prompt <p>]`.
///
/// Exit code `2` yields `has_comments: true` with the CRLF-normalised stderr when delivered;
/// exit code `0` yields clean empty result;
/// timeouts yield empty result with SIGTERM sent;
/// exit outside protocol or spawn/start errors produce `CheckResult` with `failure` set.
pub async fn run_comment_checker(
    input: &RunCommentCheckerInput,
    options: &RunCommentCheckerOptions<'_>,
) -> io::Result<CheckResult> {
    let Some(binary_path) = input
        .binary_path
        .as_deref()
        .filter(|path| (options.exists_sync)(path))
    else {
        return Ok(CheckResult::default());
    };

    let mut args = vec![binary_path.to_string(), "check".to_string()];
    if let Some(prompt) = &input.custom_prompt {
        args.push("--prompt".to_string());
        args.push(prompt.clone());
    }

    let timeout = Duration::from_millis(options.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));

    let mut process = match (options.spawn)(&args) {
        Ok(proc) => proc,
        Err(err) => return Ok(failed(None, &err.to_string())),
    };

    let payload = match serde_json::to_string(&input.hook_input) {
        Ok(p) => p,
        Err(err) => return Err(io::Error::other(err)),
    };

    let input_delivered = match process.write_stdin(&payload).and_then(|()| process.end_stdin()) {
        Ok(()) => true,
        Err(_) => {
            let _ = process.kill(SpawnSignal::Sigkill);
            false
        }
    };

    let stdout = process.take_stdout();
    let stderr = process.take_stderr();

    let completed = tokio::time::timeout(timeout, async {
        let stdout_fut = read_text(stdout);
        let stderr_fut = read_text(stderr);
        let exit_fut = process.exited();
        let (stdout_res, stderr_res, exit_res) = tokio::join!(stdout_fut, stderr_fut, exit_fut);
        let exit_outcome = match exit_res {
            Ok(code) => ExitOutcome::Exited(code),
            Err(err) => ExitOutcome::NotStarted(err.to_string()),
        };
        (stdout_res.ok(), stderr_res.ok(), exit_outcome)
    })
    .await;

    let Ok((_stdout, stderr, exit_outcome)) = completed else {
        let _ = process.kill(SpawnSignal::Sigterm);
        return Ok(CheckResult::default());
    };

    match exit_outcome {
        ExitOutcome::NotStarted(reason) => Ok(failed(None, &reason)),
        ExitOutcome::Exited(0) => Ok(CheckResult::default()),
        ExitOutcome::Exited(2) => {
            if input_delivered && let Some(stderr) = stderr {
                Ok(CheckResult {
                    has_comments: true,
                    message: normalize_message(&stderr),
                    failure: None,
                })
            } else {
                Ok(CheckResult::default())
            }
        }
        ExitOutcome::Exited(code) => Ok(failed(Some(code), stderr.as_deref().unwrap_or(""))),
    }
}
