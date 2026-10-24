//! The user bash `BashOperations` under `--tool-runtime child` (senpi#3566 todo 9): `exec` runs the command as a
//! `user.bash` job on the session's tool child and feeds its streamed `{ data: <base64> }` updates to `on_data`. The
//! environment is resolved here, on the server, exactly as the in-process operations resolve it.
use super::tool_dispatcher::{ToolChildKindCall, ToolChildKindRunner};
use crate::utils::shell::get_shell_env;
use base64::Engine;
use maho_tools::{
    bash::{BashExecOptions, BashExit, BashOperations},
    definition::{ToolError, ToolFuture},
};
use serde_json::{Map, Value};
use std::{path::Path, sync::Arc};

const USER_BASH_JOB_KIND: &str = "user.bash";

fn chunk_of(update: &Value) -> Option<Vec<u8>> {
    let data = update.get("data")?.as_str()?;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

fn exit_code_of(value: &Value) -> Option<i32> {
    let exit_code = value.get("exitCode")?;
    exit_code
        .as_i64()
        .and_then(|code| i32::try_from(code).ok())
        .or_else(|| exit_code.as_f64().map(|code| code as i32))
}

struct ChildBashOperations {
    runner: ToolChildKindRunner,
    shell_path: Option<String>,
}

impl BashOperations for ChildBashOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a Path,
        options: BashExecOptions,
    ) -> ToolFuture<'a, BashExit> {
        Box::pin(async move {
            let BashExecOptions {
                on_data,
                signal,
                timeout,
                env,
            } = options;
            if signal.is_aborted() {
                return Err(ToolError::Message("aborted".to_owned()));
            }
            let shell_env: Map<String, Value> =
                (if env.is_empty() { get_shell_env() } else { env })
                    .into_iter()
                    .map(|(name, value)| (name, Value::String(value)))
                    .collect();
            let mut args = Map::new();
            args.insert("command".into(), Value::String(command.to_owned()));
            args.insert(
                "cwd".into(),
                Value::String(cwd.to_string_lossy().into_owned()),
            );
            args.insert("env".into(), Value::Object(shell_env));
            if let Some(shell_path) = &self.shell_path {
                args.insert("shellPath".into(), Value::String(shell_path.clone()));
            }
            if let Some(timeout) = timeout.filter(|timeout| *timeout > 0.0) {
                args.insert("timeoutSeconds".into(), serde_json::json!(timeout));
            }
            let sink = on_data.clone();
            let result = (self.runner)(
                USER_BASH_JOB_KIND.to_owned(),
                Some(Value::Object(args)),
                ToolChildKindCall {
                    tool_call_id: format!("user-bash-{}", uuid::Uuid::new_v4()),
                    signal: Some(signal),
                    on_update: Some(Arc::new(move |update: Value| {
                        if let Some(chunk) = chunk_of(&update) {
                            // A sink failure cannot reach the child's stream; the job ends by its own exit.
                            let _ = sink(&chunk);
                        }
                    })),
                },
            )
            .await
            .map_err(|error| ToolError::Message(error.to_string()))?;
            Ok(BashExit {
                exit_code: exit_code_of(&result.value),
            })
        })
    }
}

pub fn child_bash_operations(
    runner: ToolChildKindRunner,
    shell_path: Option<&str>,
) -> Arc<dyn BashOperations> {
    Arc::new(ChildBashOperations {
        runner,
        shell_path: shell_path.map(str::to_owned),
    })
}
