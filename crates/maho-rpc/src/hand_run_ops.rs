//! Port of senpi `packages/coding-agent/src/modes/rpc/hand-run-ops.ts`.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use maho_agent::execution::device_files::{
    DeviceFileError, DeviceFileErrorCode, FILE_CHANNEL_MAX_BYTES, base64_length, decode_base64,
    encode_base64,
};
use maho_agent::execution::plane::NestedRequestFn;
use maho_ai::utils::abort::AbortSignal;
use maho_tools::filesystem_policy::{
    FilesystemOperation, FilesystemPolicyChecker, FilesystemPolicyDecision, FilesystemPolicyRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use uuid::Uuid;

use crate::hand_activity_lock::{FlockBackend, hold_activity_lock};
use crate::hand_file_ops::HandFileGuards;
use crate::hand_run_allowlist::{
    AllowedExe, HandRunContext, RunPlan, RunRequest, plan_run, render_command,
};
use crate::hand_run_checks::{
    PolicyOperation, confine_paths, confined_index_file, git_is_supported, has_url_rewrites,
    run_directory,
};
use crate::hand_run_transport::transport_for;

#[derive(Debug, Clone)]
pub struct RunArgs {
    pub exe: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: HashMap<String, String>,
    pub stdin: Option<String>,
    pub timeout_ms: Option<u64>,
    pub activity_lock: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOutcome {
    pub code: Option<i32>,
    pub signal: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

const RUN_TIMEOUT_CAP_MS: u64 = 300_000;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const KILL_GRACE_MS: u64 = 10_000;

async fn judged(
    guards: &dyn HandFileGuards,
    command: &str,
    nested: Option<NestedRequestFn>,
) -> Result<(), DeviceFileError> {
    let mut input = Map::new();
    input.insert("command".to_owned(), Value::String(command.to_owned()));
    let decision = guards
        .before_tool_call(
            "bash",
            &format!("run-{}", Uuid::new_v4()),
            &mut input,
            nested,
        )
        .await
        .map_err(|err| DeviceFileError::new(DeviceFileErrorCode::RefusedByPolicy, err))?;
    if let Some(decision) = decision
        && decision.block == Some(true)
    {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::RefusedByPolicy,
            decision.reason.unwrap_or_else(|| "blocked".to_owned()),
        ));
    }
    if input.get("command").and_then(Value::as_str) != Some(command) {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::RefusedByPolicy,
            "a tool_call handler rewrote the command",
        ));
    }
    Ok(())
}

async fn check_policy(
    policy: Option<&FilesystemPolicyChecker>,
    operation: FilesystemOperation,
    canonical_path: &Path,
) -> Result<(), DeviceFileError> {
    let Some(checker) = policy else {
        return Ok(());
    };
    let decision = checker(FilesystemPolicyRequest {
        operation,
        canonical_path: canonical_path.to_path_buf(),
        tool_name: "bash".into(),
    })
    .await
    .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::RefusedByPolicy, e.to_string()))?;
    if let FilesystemPolicyDecision::Deny { reason } = decision {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::RefusedByPolicy,
            reason,
        ));
    }
    Ok(())
}

pub struct RunOnHandDeps<'a> {
    pub root: &'a Path,
    pub guards: &'a dyn HandFileGuards,
    pub context: &'a HandRunContext,
    pub flock: Option<&'a FlockBackend>,
    pub nested: Option<NestedRequestFn>,
}

/// One `run` on a hand (S40 PR 2, design v2.2-v2.4): the allowlist's exact argv and environment,
/// the scope's handlers judging the rendered command as `bash`, the policy checking `cwd` as a
/// write, `GIT_INDEX_FILE` confined, git's version floor, the activity lock held until the child
/// exits, and no shell. Streams and time are bounded.
pub async fn run_on_hand(
    args: RunArgs,
    deps: RunOnHandDeps<'_>,
    signal: AbortSignal,
) -> Result<RunOutcome, DeviceFileError> {
    let plan = plan_run(
        &RunRequest {
            exe: args.exe.clone(),
            argv: args.argv.clone(),
            env: args.env.clone(),
        },
        deps.context,
    );
    let (exe, plan_argv, plan_env, paths) = match plan {
        RunPlan::Ok {
            exe,
            argv,
            env,
            paths,
        } => (exe, argv, env, paths),
        RunPlan::Refused { reason } => {
            return Err(DeviceFileError::new(
                DeviceFileErrorCode::RefusedByPolicy,
                reason,
            ));
        }
    };
    if let Some(ref stdin) = args.stdin
        && base64_length(stdin) > FILE_CHANNEL_MAX_BYTES
    {
        return Err(DeviceFileError::new(DeviceFileErrorCode::TooLarge, "stdin"));
    }
    let cwd = run_directory(deps.root, &args.cwd).await?;
    let policy = deps.guards.filesystem_policy();
    check_policy(policy.as_ref(), FilesystemOperation::Write, &cwd).await?;

    let (argv, mut env, local) = if exe == AllowedExe::Git {
        let t = transport_for(&plan_argv, &plan_env, &paths);
        (t.argv, t.env, t.local)
    } else {
        (plan_argv, plan_env, false)
    };
    if exe == AllowedExe::Git {
        if !git_is_supported(&env).await {
            return Err(DeviceFileError::new(
                DeviceFileErrorCode::Unsupported,
                "git 2.29 or newer is required",
            ));
        }
        if local && has_url_rewrites(&cwd, &env).await? {
            return Err(DeviceFileError::new(
                DeviceFileErrorCode::RefusedByPolicy,
                "local_url_rewritten: a url.* rewrite is configured",
            ));
        }
    }
    let rendered = render_command(exe.as_str(), &argv);
    judged(deps.guards, &rendered, deps.nested).await?;
    if exe == AllowedExe::Git {
        if let Some(index_file) = env.get("GIT_INDEX_FILE").cloned() {
            let confined = confined_index_file(&index_file, &cwd, &env).await?;
            env.insert(
                "GIT_INDEX_FILE".to_owned(),
                confined.to_string_lossy().into_owned(),
            );
        }
        for target in confine_paths(&paths, &cwd, &env).await? {
            let op = match target.operation {
                PolicyOperation::Read => FilesystemOperation::Read,
                PolicyOperation::Write => FilesystemOperation::Write,
            };
            check_policy(policy.as_ref(), op, &target.canonical_path).await?;
        }
    }
    let hold = if args.activity_lock && exe == AllowedExe::Git {
        Some(hold_activity_lock(&cwd, deps.flock, None).await?)
    } else {
        None
    };
    let outcome = spawn_bounded(
        exe.as_str(),
        &argv,
        &cwd,
        &env,
        &args,
        hold.as_ref().and_then(|h| h.file()),
        signal,
    )
    .await;
    if let Some(hold) = hold {
        hold.release();
    }
    outcome
}

async fn read_bounded<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> Result<Vec<u8>, bool> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => return Ok(out),
            Ok(n) => {
                if out.len() + n > limit {
                    return Err(true);
                }
                out.extend_from_slice(&buf[..n]);
            }
            Err(_) => return Ok(out),
        }
    }
}

fn send_sigterm(child: &tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id()
        && let Some(pid) = rustix::process::Pid::from_raw(pid as i32)
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
    #[cfg(not(unix))]
    {
        let _ = child;
    }
}

async fn spawn_bounded(
    exe: &str,
    argv: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    args: &RunArgs,
    lock_file: Option<&std::fs::File>,
    signal: AbortSignal,
) -> Result<RunOutcome, DeviceFileError> {
    if signal.aborted() {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::Aborted,
            "the run was aborted",
        ));
    }
    #[cfg(unix)]
    if let Some(file) = lock_file {
        // Keep the shared lock descriptor open across exec so git and its grandchildren hold it.
        let _ = rustix::io::fcntl_setfd(file, rustix::io::FdFlags::empty());
    }
    let mut cmd = Command::new(exe);
    cmd.args(argv)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))?;

    let stdin_bytes = match &args.stdin {
        Some(encoded) => Some(decode_base64(encoded)?),
        None => None,
    };
    if let Some(mut stdin) = child.stdin.take() {
        tokio::spawn(async move {
            if let Some(bytes) = stdin_bytes {
                let _ = stdin.write_all(&bytes).await;
            }
            let _ = stdin.shutdown().await;
        });
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| DeviceFileError::new(DeviceFileErrorCode::Io, "missing child stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| DeviceFileError::new(DeviceFileErrorCode::Io, "missing child stderr"))?;
    let out_task = tokio::spawn(read_bounded(stdout, FILE_CHANNEL_MAX_BYTES));
    let err_task = tokio::spawn(read_bounded(stderr, FILE_CHANNEL_MAX_BYTES));

    let timeout_ms = args
        .timeout_ms
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .min(RUN_TIMEOUT_CAP_MS);
    let sleep = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(sleep);

    let mut overflow = false;
    let status = tokio::select! {
        res = child.wait() => {
            res.map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))?
        }
        () = &mut sleep => {
            send_sigterm(&child);
            match tokio::time::timeout(Duration::from_millis(KILL_GRACE_MS), child.wait()).await {
                Ok(res) => res.map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))?,
                Err(_) => {
                    let _ = child.kill().await;
                    out_task.abort();
                    err_task.abort();
                    return Err(DeviceFileError::new(DeviceFileErrorCode::Io, "the run timed out"));
                }
            }
        }
        () = signal.cancelled() => {
            send_sigterm(&child);
            let _ = tokio::time::timeout(Duration::from_millis(KILL_GRACE_MS), child.wait()).await;
            let _ = child.kill().await;
            return Err(DeviceFileError::new(DeviceFileErrorCode::Aborted, "the run was aborted"));
        }
    };

    if signal.aborted() {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::Aborted,
            "the run was aborted",
        ));
    }

    let out_bytes = match out_task.await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(true)) => {
            overflow = true;
            Vec::new()
        }
        _ => Vec::new(),
    };
    let err_bytes = match err_task.await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(true)) => {
            overflow = true;
            Vec::new()
        }
        _ => Vec::new(),
    };
    if overflow {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::TooLarge,
            "the run's output passed the limit",
        ));
    }

    #[cfg(unix)]
    let exit_signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|sig| format!("SIG{sig}"))
    };
    #[cfg(not(unix))]
    let exit_signal: Option<String> = None;

    Ok(RunOutcome {
        code: status.code(),
        signal: exit_signal,
        stdout: encode_base64(&out_bytes),
        stderr: String::from_utf8_lossy(&err_bytes).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::types::BoxFuture;
    use maho_ai::utils::abort::AbortController;
    use maho_ext_api::ToolCallEventResult;

    struct BlockAllGuards;
    impl HandFileGuards for BlockAllGuards {
        fn before_tool_call<'a>(
            &'a self,
            _tool_name: &'a str,
            _tool_call_id: &'a str,
            _input: &'a mut Map<String, Value>,
            _nested: Option<NestedRequestFn>,
        ) -> BoxFuture<'a, Result<Option<ToolCallEventResult>, String>> {
            Box::pin(async {
                Ok(Some(ToolCallEventResult {
                    block: Some(true),
                    reason: Some("blocked by test".to_owned()),
                    terminate: None,
                }))
            })
        }
        fn filesystem_policy(&self) -> Option<FilesystemPolicyChecker> {
            None
        }
    }

    struct AllowGuards;
    impl HandFileGuards for AllowGuards {
        fn before_tool_call<'a>(
            &'a self,
            _tool_name: &'a str,
            _tool_call_id: &'a str,
            _input: &'a mut Map<String, Value>,
            _nested: Option<NestedRequestFn>,
        ) -> BoxFuture<'a, Result<Option<ToolCallEventResult>, String>> {
            Box::pin(async { Ok(None) })
        }
        fn filesystem_policy(&self) -> Option<FilesystemPolicyChecker> {
            None
        }
    }

    #[tokio::test]
    async fn run_on_hand_refuses_disallowed_exe_and_handler_blocks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = HandRunContext::current();
        let ctrl = AbortController::new();

        let bad_exe = run_on_hand(
            RunArgs {
                exe: "sh".into(),
                argv: vec!["-c".into(), "true".into()],
                cwd: ".".into(),
                env: HashMap::new(),
                stdin: None,
                timeout_ms: None,
                activity_lock: false,
            },
            RunOnHandDeps {
                root: dir.path(),
                guards: &AllowGuards,
                context: &ctx,
                flock: None,
                nested: None,
            },
            ctrl.signal(),
        )
        .await
        .expect_err("disallowed exe");
        assert_eq!(bad_exe.code, DeviceFileErrorCode::RefusedByPolicy);

        let blocked = run_on_hand(
            RunArgs {
                exe: "git".into(),
                argv: vec!["init".into(), "--template=".into()],
                cwd: ".".into(),
                env: HashMap::new(),
                stdin: None,
                timeout_ms: None,
                activity_lock: false,
            },
            RunOnHandDeps {
                root: dir.path(),
                guards: &BlockAllGuards,
                context: &ctx,
                flock: None,
                nested: None,
            },
            ctrl.signal(),
        )
        .await
        .expect_err("blocked");
        assert_eq!(blocked.code, DeviceFileErrorCode::RefusedByPolicy);
        assert_eq!(blocked.message, "blocked by test");
    }
}
