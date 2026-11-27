//! Port of senpi `packages/agent/src/execution/remote-device-files.ts`.

use std::sync::Arc;

use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use serde_json::{Value, json};

use super::device_files::{
    DeviceFileError, DeviceFileErrorCode, DeviceFileOp, DeviceFileOptions, DeviceFileStat,
    DeviceFileWriteOptions, DeviceFiles, DeviceRunRequest, DeviceRunResult, FILE_CHANNEL,
    FILE_CHANNEL_MAX_BYTES, RUN_CHANNEL, decode_base64, encode_base64, file_outcome_of,
};
use super::remote_link::{HandLink, HandReply};
use super::remote_protocol::RemoteFile;
use super::tool_host::ExecutionScope;

pub const HELLO_WAIT_MS: u64 = 5000;

fn is_grant_refusal(reason: &str) -> bool {
    matches!(
        reason,
        "grant_missing" | "grant_expired" | "workspace_unknown" | "capability_missing"
    )
}

fn refusal_error(reason: &str) -> DeviceFileError {
    if is_grant_refusal(reason) {
        DeviceFileError::new(DeviceFileErrorCode::RefusedByGrant, reason)
    } else if reason == "epoch_fenced" {
        DeviceFileError::new(DeviceFileErrorCode::RefusedByPolicy, reason)
    } else if reason == "hand_closed" || reason.starts_with("scope_unavailable") {
        DeviceFileError::new(DeviceFileErrorCode::HandUnreachable, reason)
    } else {
        DeviceFileError::new(DeviceFileErrorCode::Protocol, reason)
    }
}

fn value_of(reply: HandReply) -> Result<Value, DeviceFileError> {
    match reply {
        HandReply::Settled { result } => {
            let details = result.details.unwrap_or(Value::Null);
            let outcome = file_outcome_of(&details).ok_or_else(|| {
                DeviceFileError::new(
                    DeviceFileErrorCode::Protocol,
                    "the hand sent no file outcome",
                )
            })?;
            match outcome {
                super::device_files::DeviceFileOutcome::Ok { value, .. } => Ok(value),
                super::device_files::DeviceFileOutcome::Err { code, message, .. } => {
                    Err(DeviceFileError::new(code, message))
                }
            }
        }
        HandReply::Terminal { terminal } => Err(DeviceFileError::with_detail(
            DeviceFileErrorCode::Interrupted,
            format!(
                "the operation ended without an outcome ({})",
                terminal.reason.as_str()
            ),
            Some(terminal.reason.as_str().to_owned()),
            terminal.uncertain,
        )),
        HandReply::Refused { reason } => Err(refusal_error(&reason)),
        HandReply::Aborted { .. } => Err(DeviceFileError::new(
            DeviceFileErrorCode::Aborted,
            "the operation was aborted",
        )),
        HandReply::Unreachable { reason } => Err(DeviceFileError::new(
            DeviceFileErrorCode::HandUnreachable,
            reason,
        )),
        HandReply::Closed { sent } => {
            if sent {
                Err(DeviceFileError::with_detail(
                    DeviceFileErrorCode::Interrupted,
                    "the hand link closed before the operation settled (hand_restarted)",
                    Some("hand_restarted".to_owned()),
                    true,
                ))
            } else {
                Err(DeviceFileError::new(
                    DeviceFileErrorCode::HandUnreachable,
                    "the hand link was closed",
                ))
            }
        }
        HandReply::EpochRefused { reason } => Err(DeviceFileError::new(
            DeviceFileErrorCode::RefusedByPolicy,
            format!("epoch: {reason}"),
        )),
    }
}

struct RemoteDeviceFiles {
    link: Arc<HandLink>,
    scope: ExecutionScope,
    mint_id: Arc<dyn Fn() -> String + Send + Sync>,
}

impl RemoteDeviceFiles {
    async fn call(
        &self,
        op: DeviceFileOp,
        args: Value,
        signal: Option<AbortSignal>,
        channel: &str,
    ) -> Result<Value, DeviceFileError> {
        let hello = self.link.hello(HELLO_WAIT_MS, signal.clone()).await;
        if signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(DeviceFileError::new(
                DeviceFileErrorCode::Aborted,
                "the operation was aborted",
            ));
        }
        let Some(hello) = hello else {
            return Err(DeviceFileError::new(
                DeviceFileErrorCode::Unsupported,
                format!("the hand serves no {channel} channel"),
            ));
        };
        if !hello.ops.iter().any(|c| c == channel) {
            return Err(DeviceFileError::new(
                DeviceFileErrorCode::Unsupported,
                format!("the hand serves no {channel} channel"),
            ));
        }
        let reply = self
            .link
            .dispatch_file(
                RemoteFile {
                    execution_id: (self.mint_id)(),
                    epoch: None,
                    scope: self.scope.clone(),
                    op,
                    args,
                },
                signal,
            )
            .await;
        value_of(reply)
    }
}

impl DeviceFiles for RemoteDeviceFiles {
    fn read(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<Vec<u8>, DeviceFileError>> {
        let path = path.to_owned();
        Box::pin(async move {
            let value = self
                .call(
                    DeviceFileOp::Read,
                    json!({ "path": path }),
                    options.signal,
                    FILE_CHANNEL,
                )
                .await?;
            let encoded = value.as_str().ok_or_else(|| {
                DeviceFileError::new(DeviceFileErrorCode::Protocol, "malformed read outcome")
            })?;
            decode_base64(encoded)
        })
    }

    fn write(
        &self,
        path: &str,
        data: Vec<u8>,
        options: DeviceFileOptions,
        write_options: DeviceFileWriteOptions,
    ) -> BoxFuture<'_, Result<(), DeviceFileError>> {
        let path = path.to_owned();
        Box::pin(async move {
            if data.len() > FILE_CHANNEL_MAX_BYTES {
                return Err(DeviceFileError::new(DeviceFileErrorCode::TooLarge, path));
            }
            let encoded = encode_base64(&data);
            self.call(
                DeviceFileOp::Write,
                json!({
                    "path": path,
                    "data": encoded,
                    "exclusive": write_options.exclusive,
                }),
                options.signal,
                FILE_CHANNEL,
            )
            .await?;
            Ok(())
        })
    }

    fn list(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<Vec<String>, DeviceFileError>> {
        let path = path.to_owned();
        Box::pin(async move {
            let value = self
                .call(
                    DeviceFileOp::List,
                    json!({ "path": path }),
                    options.signal,
                    FILE_CHANNEL,
                )
                .await?;
            let arr = value.as_array().ok_or_else(|| {
                DeviceFileError::new(DeviceFileErrorCode::Protocol, "malformed list outcome")
            })?;
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let s = item.as_str().ok_or_else(|| {
                    DeviceFileError::new(DeviceFileErrorCode::Protocol, "malformed list outcome")
                })?;
                out.push(s.to_owned());
            }
            Ok(out)
        })
    }

    fn stat(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<DeviceFileStat, DeviceFileError>> {
        let path = path.to_owned();
        Box::pin(async move {
            let value = self
                .call(
                    DeviceFileOp::Stat,
                    json!({ "path": path }),
                    options.signal,
                    FILE_CHANNEL,
                )
                .await?;
            serde_json::from_value(value).map_err(|_| {
                DeviceFileError::new(DeviceFileErrorCode::Protocol, "malformed stat outcome")
            })
        })
    }

    fn rename(
        &self,
        from: &str,
        to: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<(), DeviceFileError>> {
        let from = from.to_owned();
        let to = to.to_owned();
        Box::pin(async move {
            self.call(
                DeviceFileOp::Rename,
                json!({ "path": from, "to": to }),
                options.signal,
                FILE_CHANNEL,
            )
            .await?;
            Ok(())
        })
    }

    fn remove(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<(), DeviceFileError>> {
        let path = path.to_owned();
        Box::pin(async move {
            self.call(
                DeviceFileOp::Remove,
                json!({ "path": path }),
                options.signal,
                FILE_CHANNEL,
            )
            .await?;
            Ok(())
        })
    }

    fn mkdir(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<(), DeviceFileError>> {
        let path = path.to_owned();
        Box::pin(async move {
            self.call(
                DeviceFileOp::Mkdir,
                json!({ "path": path }),
                options.signal,
                FILE_CHANNEL,
            )
            .await?;
            Ok(())
        })
    }

    fn run(
        &self,
        request: DeviceRunRequest,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<DeviceRunResult, DeviceFileError>> {
        Box::pin(async move {
            if request
                .stdin
                .as_ref()
                .is_some_and(|s| s.len() > FILE_CHANNEL_MAX_BYTES)
            {
                return Err(DeviceFileError::new(DeviceFileErrorCode::TooLarge, "stdin"));
            }
            let mut args = serde_json::Map::new();
            args.insert("path".to_owned(), Value::String(request.cwd));
            args.insert("exe".to_owned(), Value::String(request.exe));
            args.insert(
                "argv".to_owned(),
                Value::Array(request.argv.into_iter().map(Value::String).collect()),
            );
            args.insert(
                "env".to_owned(),
                serde_json::to_value(request.env.unwrap_or_default()).unwrap_or(json!({})),
            );
            if let Some(stdin) = request.stdin {
                args.insert("stdin".to_owned(), Value::String(encode_base64(&stdin)));
            }
            if let Some(timeout) = request.timeout_ms {
                args.insert("timeoutMs".to_owned(), json!(timeout));
            }
            args.insert(
                "activityLock".to_owned(),
                Value::Bool(request.activity_lock == Some(true)),
            );

            let value = self
                .call(
                    DeviceFileOp::Run,
                    Value::Object(args),
                    options.signal,
                    RUN_CHANNEL,
                )
                .await?;
            let obj = value.as_object().ok_or_else(|| {
                DeviceFileError::new(DeviceFileErrorCode::Protocol, "malformed run outcome")
            })?;
            let stdout_str = obj.get("stdout").and_then(Value::as_str).ok_or_else(|| {
                DeviceFileError::new(DeviceFileErrorCode::Protocol, "malformed run outcome")
            })?;
            let stderr = obj
                .get("stderr")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    DeviceFileError::new(DeviceFileErrorCode::Protocol, "malformed run outcome")
                })?
                .to_owned();
            let code = obj.get("code").and_then(Value::as_i64).map(|n| n as i32);
            let sig = obj.get("signal").and_then(Value::as_str).map(str::to_owned);
            Ok(DeviceRunResult {
                code,
                signal: sig,
                stdout: decode_base64(stdout_str)?,
                stderr,
            })
        })
    }
}

pub fn create_remote_device_files(
    link: Arc<HandLink>,
    scope: ExecutionScope,
    mint_id: Arc<dyn Fn() -> String + Send + Sync>,
) -> Arc<dyn DeviceFiles> {
    Arc::new(RemoteDeviceFiles {
        link,
        scope,
        mint_id,
    })
}
