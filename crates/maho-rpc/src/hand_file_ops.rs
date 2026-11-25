//! Port of senpi `packages/coding-agent/src/modes/rpc/hand-file-ops.ts`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_agent::execution::device_files::{
    DeviceFileArgs, DeviceFileError, DeviceFileErrorCode, DeviceFileOptions, DeviceFileOutcome,
    DeviceFileWriteOptions, FILE_CHANNEL_MAX_BYTES, base64_length, decode_base64,
    decode_device_file_args, encode_base64,
};
use maho_agent::execution::device_files_node::local_device_files;
use maho_agent::execution::plane::NestedRequestFn;
use maho_agent::execution::remote_file_call::{file_refusal, file_result};
use maho_agent::types::AgentToolResult;
use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::ToolCallEventResult;
use maho_tools::file_mutation_queue::{with_file_mutation_queue, with_file_mutation_queues};
use maho_tools::filesystem_policy::{
    FilesystemOperation, FilesystemPolicyChecker, FilesystemPolicyDecision,
    FilesystemPolicyRequest, canonicalize_filesystem_path,
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::hand_activity_lock::FlockBackend;
use crate::hand_run_allowlist::HandRunContext;
use crate::hand_run_ops::{RunArgs, RunOnHandDeps, run_on_hand};

/// What a hand scope checks every device file operation against: its runtime's handlers and
/// filesystem policy.
pub trait HandFileGuards: Send + Sync {
    fn before_tool_call<'a>(
        &'a self,
        tool_name: &'a str,
        tool_call_id: &'a str,
        input: &'a mut Map<String, Value>,
        nested: Option<NestedRequestFn>,
    ) -> BoxFuture<'a, Result<Option<ToolCallEventResult>, String>>;

    fn filesystem_policy(&self) -> Option<FilesystemPolicyChecker>;
}

#[derive(Clone, Copy)]
enum FileToolName {
    Read,
    Ls,
    Write,
}

impl FileToolName {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Ls => "ls",
            Self::Write => "write",
        }
    }
}

struct Check {
    tool_name: FileToolName,
    operation: FilesystemOperation,
    path: String,
    content: Option<String>,
    /// Acts on the directory entry itself (a link, not its target), as the local filesystem does.
    entry: bool,
}

fn checks_of(args: &DeviceFileArgs) -> Result<Vec<Check>, DeviceFileError> {
    match args {
        DeviceFileArgs::Read { path } => Ok(vec![Check {
            tool_name: FileToolName::Read,
            operation: FilesystemOperation::Read,
            path: path.clone(),
            content: None,
            entry: false,
        }]),
        DeviceFileArgs::List { path } => Ok(vec![Check {
            tool_name: FileToolName::Ls,
            operation: FilesystemOperation::Enumerate,
            path: path.clone(),
            content: None,
            entry: false,
        }]),
        DeviceFileArgs::Stat { path } => Ok(vec![Check {
            tool_name: FileToolName::Ls,
            operation: FilesystemOperation::Enumerate,
            path: path.clone(),
            content: None,
            entry: true,
        }]),
        DeviceFileArgs::Write { path, data, .. } => {
            let decoded = decode_base64(data)?;
            let content = String::from_utf8_lossy(&decoded).into_owned();
            Ok(vec![Check {
                tool_name: FileToolName::Write,
                operation: FilesystemOperation::Write,
                path: path.clone(),
                content: Some(content),
                entry: false,
            }])
        }
        DeviceFileArgs::Rename { path, to } => Ok(vec![
            Check {
                tool_name: FileToolName::Write,
                operation: FilesystemOperation::Write,
                path: path.clone(),
                content: Some(String::new()),
                entry: true,
            },
            Check {
                tool_name: FileToolName::Write,
                operation: FilesystemOperation::Write,
                path: to.clone(),
                content: Some(String::new()),
                entry: true,
            },
        ]),
        DeviceFileArgs::Remove { path } => Ok(vec![Check {
            tool_name: FileToolName::Write,
            operation: FilesystemOperation::Write,
            path: path.clone(),
            content: Some(String::new()),
            entry: true,
        }]),
        DeviceFileArgs::Mkdir { path } => Ok(vec![Check {
            tool_name: FileToolName::Write,
            operation: FilesystemOperation::Write,
            path: path.clone(),
            content: Some(String::new()),
            entry: false,
        }]),
        DeviceFileArgs::Run { .. } => Err(DeviceFileError::new(
            DeviceFileErrorCode::Protocol,
            "run handled separately",
        )),
    }
}

async fn canonical_of(path: &Path, entry: bool) -> Result<PathBuf, DeviceFileError> {
    if entry {
        let Some(parent) = path.parent() else {
            return canonicalize_filesystem_path(path)
                .await
                .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()));
        };
        let Some(base) = path.file_name() else {
            return canonicalize_filesystem_path(path)
                .await
                .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()));
        };
        let real_parent = canonicalize_filesystem_path(parent)
            .await
            .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))?;
        Ok(real_parent.join(base))
    } else {
        canonicalize_filesystem_path(path)
            .await
            .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))
    }
}

async fn allowed(
    guards: &dyn HandFileGuards,
    root: &Path,
    check: &Check,
    nested: Option<NestedRequestFn>,
) -> Result<PathBuf, DeviceFileError> {
    let content = check.content.as_deref().unwrap_or("");
    let mut input = Map::new();
    input.insert("path".to_owned(), Value::String(check.path.clone()));
    if matches!(check.tool_name, FileToolName::Write) {
        input.insert("content".to_owned(), Value::String(content.to_owned()));
    }
    let decision = guards
        .before_tool_call(
            check.tool_name.as_str(),
            &format!("file-{}", Uuid::new_v4()),
            &mut input,
            nested,
        )
        .await
        .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::RefusedByPolicy, e))?;
    if let Some(decision) = decision
        && decision.block == Some(true)
    {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::RefusedByPolicy,
            decision.reason.unwrap_or_else(|| "blocked".to_owned()),
        ));
    }
    // A handler may patch a tool's input in place; a file operation does not follow a patched path
    // or content.
    if input.get("path").and_then(Value::as_str) != Some(&check.path)
        || (matches!(check.tool_name, FileToolName::Write)
            && input.get("content").and_then(Value::as_str) != Some(content))
    {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::RefusedByPolicy,
            "a tool_call handler rewrote the operation",
        ));
    }
    let canonical_path = canonical_of(&root.join(&check.path), check.entry).await?;
    if let Some(checker) = guards.filesystem_policy() {
        let verdict = checker(FilesystemPolicyRequest {
            operation: check.operation,
            canonical_path: canonical_path.clone(),
            tool_name: check.tool_name.as_str().into(),
        })
        .await
        .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::RefusedByPolicy, e.to_string()))?;
        if let FilesystemPolicyDecision::Deny { reason } = verdict {
            return Err(DeviceFileError::new(
                DeviceFileErrorCode::RefusedByPolicy,
                reason,
            ));
        }
    }
    Ok(canonical_path)
}

pub struct HandRunBinding {
    pub context: HandRunContext,
    pub flock: Option<FlockBackend>,
}

pub type HandFileOpFn = Arc<
    dyn Fn(
            String,
            Value,
            AbortSignal,
            Option<NestedRequestFn>,
        ) -> BoxFuture<'static, AgentToolResult>
        + Send
        + Sync,
>;

/// One hand scope's device file operations (S40, `senpi#5645`). Paths resolve against the scope
/// root and are NOT confined to it: they are bounded by the hand's OS user, its filesystem policy
/// and its `tool_call` handlers.
pub fn create_hand_file_ops(
    root: impl Into<PathBuf>,
    guards: Arc<dyn HandFileGuards>,
    run: Option<Arc<HandRunBinding>>,
) -> HandFileOpFn {
    let root = root.into();
    let files = local_device_files();
    Arc::new(move |op, raw_args, signal, nested| {
        let root = root.clone();
        let guards = Arc::clone(&guards);
        let run = run.clone();
        let files = Arc::clone(&files);
        Box::pin(async move {
            let Some(args) = decode_device_file_args(&op, &raw_args) else {
                return file_refusal(
                    DeviceFileErrorCode::Protocol,
                    format!("malformed {op} arguments"),
                );
            };
            if let DeviceFileArgs::Run {
                path,
                exe,
                argv,
                env,
                stdin,
                timeout_ms,
                activity_lock,
            } = args
            {
                let Some(run) = run else {
                    return file_refusal(
                        DeviceFileErrorCode::Unsupported,
                        "this hand serves no run channel",
                    );
                };
                return match run_on_hand(
                    RunArgs {
                        exe,
                        argv,
                        cwd: path,
                        env,
                        stdin,
                        timeout_ms,
                        activity_lock,
                    },
                    RunOnHandDeps {
                        root: &root,
                        guards: guards.as_ref(),
                        context: &run.context,
                        flock: run.flock.as_ref(),
                        nested,
                    },
                    signal,
                )
                .await
                {
                    Ok(outcome) => file_result(DeviceFileOutcome::ok(
                        serde_json::to_value(outcome).unwrap_or(Value::Null),
                    )),
                    Err(err) => file_refusal(err.code, err.message),
                };
            }
            if let DeviceFileArgs::Write {
                ref path, ref data, ..
            } = args
                && base64_length(data) > FILE_CHANNEL_MAX_BYTES
            {
                return file_refusal(DeviceFileErrorCode::TooLarge, path.clone());
            }

            let checks = match checks_of(&args) {
                Ok(c) => c,
                Err(err) => return file_refusal(err.code, err.message),
            };
            let mut targets = Vec::with_capacity(checks.len());
            for check in &checks {
                match allowed(guards.as_ref(), &root, check, nested.clone()).await {
                    Ok(p) => targets.push(p),
                    Err(err) => return file_refusal(err.code, err.message),
                }
            }
            let Some(target) = targets.first().cloned() else {
                return file_refusal(DeviceFileErrorCode::Protocol, "no target");
            };
            let target_str = target.to_string_lossy().into_owned();
            let options = DeviceFileOptions {
                signal: Some(signal),
            };
            match args {
                DeviceFileArgs::Read { .. } => match files.read(&target_str, options).await {
                    Ok(bytes) => {
                        file_result(DeviceFileOutcome::ok(Value::String(encode_base64(&bytes))))
                    }
                    Err(err) => file_refusal(err.code, err.message),
                },
                DeviceFileArgs::List { .. } => match files.list(&target_str, options).await {
                    Ok(entries) => file_result(DeviceFileOutcome::ok(json!(entries))),
                    Err(err) => file_refusal(err.code, err.message),
                },
                DeviceFileArgs::Stat { .. } => match files.stat(&target_str, options).await {
                    Ok(st) => file_result(DeviceFileOutcome::ok(
                        serde_json::to_value(st).unwrap_or(Value::Null),
                    )),
                    Err(err) => file_refusal(err.code, err.message),
                },
                DeviceFileArgs::Write {
                    data, exclusive, ..
                } => {
                    let bytes = match decode_base64(&data) {
                        Ok(b) => b,
                        Err(err) => return file_refusal(err.code, err.message),
                    };
                    let res = with_file_mutation_queue(&target, || {
                        files.write(
                            &target_str,
                            bytes,
                            options,
                            DeviceFileWriteOptions { exclusive },
                        )
                    })
                    .await;
                    match res {
                        Ok(Ok(())) => file_result(DeviceFileOutcome::ok(Value::Null)),
                        Ok(Err(err)) => file_refusal(err.code, err.message),
                        Err(err) => file_refusal(DeviceFileErrorCode::Io, err.to_string()),
                    }
                }
                DeviceFileArgs::Rename { .. } => {
                    let dest = targets.get(1).cloned().unwrap_or_else(|| target.clone());
                    let dest_str = dest.to_string_lossy().into_owned();
                    let res = with_file_mutation_queues(&[target.clone(), dest], || {
                        let files = Arc::clone(&files);
                        Box::pin(async move { files.rename(&target_str, &dest_str, options).await })
                    })
                    .await;
                    match res {
                        Ok(Ok(())) => file_result(DeviceFileOutcome::ok(Value::Null)),
                        Ok(Err(err)) => file_refusal(err.code, err.message),
                        Err(err) => file_refusal(DeviceFileErrorCode::Io, err.to_string()),
                    }
                }
                DeviceFileArgs::Remove { .. } => {
                    let res =
                        with_file_mutation_queue(&target, || files.remove(&target_str, options))
                            .await;
                    match res {
                        Ok(Ok(())) => file_result(DeviceFileOutcome::ok(Value::Null)),
                        Ok(Err(err)) => file_refusal(err.code, err.message),
                        Err(err) => file_refusal(DeviceFileErrorCode::Io, err.to_string()),
                    }
                }
                DeviceFileArgs::Mkdir { .. } => match files.mkdir(&target_str, options).await {
                    Ok(()) => file_result(DeviceFileOutcome::ok(Value::Null)),
                    Err(err) => file_refusal(err.code, err.message),
                },
                DeviceFileArgs::Run { .. } => unreachable!("handled above"),
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_agent::execution::device_files::file_outcome_of;
    use maho_ai::utils::abort::AbortController;

    struct RewritePathGuards;
    impl HandFileGuards for RewritePathGuards {
        fn before_tool_call<'a>(
            &'a self,
            _tool_name: &'a str,
            _tool_call_id: &'a str,
            input: &'a mut Map<String, Value>,
            _nested: Option<NestedRequestFn>,
        ) -> BoxFuture<'a, Result<Option<ToolCallEventResult>, String>> {
            Box::pin(async move {
                input.insert("path".to_owned(), Value::String("/etc/passwd".to_owned()));
                Ok(None)
            })
        }
        fn filesystem_policy(&self) -> Option<FilesystemPolicyChecker> {
            None
        }
    }

    struct PassGuards;
    impl HandFileGuards for PassGuards {
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
    async fn writes_reads_and_refuses_rewritten_paths_and_missing_run_channel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctrl = AbortController::new();

        let guarded = create_hand_file_ops(dir.path(), Arc::new(RewritePathGuards), None);
        let rewritten = guarded(
            "read".into(),
            json!({"path": "ok.txt"}),
            ctrl.signal(),
            None,
        )
        .await;
        assert_eq!(rewritten.is_error, Some(true));
        let outcome = file_outcome_of(&rewritten.details).expect("outcome");
        assert!(matches!(
            outcome,
            DeviceFileOutcome::Err {
                code: DeviceFileErrorCode::RefusedByPolicy,
                ..
            }
        ));

        let ops = create_hand_file_ops(dir.path(), Arc::new(PassGuards), None);
        let write_res = ops(
            "write".into(),
            json!({"path": "hello.txt", "data": encode_base64(b"world"), "exclusive": false}),
            ctrl.signal(),
            None,
        )
        .await;
        assert_eq!(write_res.is_error, Some(false));

        let read_res = ops(
            "read".into(),
            json!({"path": "hello.txt"}),
            ctrl.signal(),
            None,
        )
        .await;
        assert_eq!(
            file_outcome_of(&read_res.details),
            Some(DeviceFileOutcome::ok(Value::String(encode_base64(
                b"world"
            ))))
        );

        let no_run = ops(
            "run".into(),
            json!({"path": ".", "exe": "git", "argv": ["status"]}),
            ctrl.signal(),
            None,
        )
        .await;
        assert!(matches!(
            file_outcome_of(&no_run.details),
            Some(DeviceFileOutcome::Err {
                code: DeviceFileErrorCode::Unsupported,
                ..
            })
        ));
    }
}
