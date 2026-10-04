use super::tool_context::HasExecutionToolContext;
use crate::{
    harness::{
        context::Context,
        types::{
            AgentHarnessTool, AgentHarnessToolUpdateOptions, ExecutionErrorCode, ShellExecOptions,
            ShellOutputCaptureOptions, ShellOutputLimits, ShellOutputRetention,
            ShellOutputTruncation, ShellOutputView, TruncationLimit,
        },
        utils::{
            output_capture::apply_shell_output_update,
            truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size},
        },
    },
    types::AgentToolResult,
};
use maho_ai::types::{BoxFuture, Tool};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};

const MAX_TIMEOUT_SECONDS: f64 = 2_147_483.647;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashToolInput {
    pub command: String,
    pub timeout: Option<f64>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<ShellOutputTruncation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}
#[derive(Debug, Clone)]
pub struct BashExecution {
    pub command: String,
    pub cwd: String,
    pub env: Map<String, Value>,
    pub inherit_env: bool,
}
pub type BashPrepare<T> = Arc<
    dyn for<'a> Fn(&'a mut BashExecution, &'a T, &'a Context) -> BoxFuture<'a, Result<(), String>>
        + Send
        + Sync,
>;
pub struct BashToolOptions<T> {
    pub command_prefix: Option<String>,
    pub prepare: Option<BashPrepare<T>>,
}
impl<T> Default for BashToolOptions<T> {
    fn default() -> Self {
        Self {
            command_prefix: None,
            prepare: None,
        }
    }
}
pub fn validate_timeout(timeout: Option<f64>) -> Result<(), String> {
    if let Some(timeout) = timeout {
        if !timeout.is_finite() || timeout <= 0.0 {
            return Err("Invalid timeout: must be a finite number of seconds".into());
        }
        if timeout > MAX_TIMEOUT_SECONDS {
            return Err(format!(
                "Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"
            ));
        }
    }
    Ok(())
}
struct UpdateState {
    view: Option<ShellOutputView>,
    last_checkpoint_at: tokio::time::Instant,
    last_checkpoint: Option<String>,
    accepting: bool,
}
pub fn create_bash_tool<T: HasExecutionToolContext>(
    options: BashToolOptions<T>,
) -> AgentHarnessTool<T> {
    let options = Arc::new(options);
    AgentHarnessTool {
        label: "bash".into(),
        prepare_arguments: None,
        replay: None,
        tool: Tool {
            name: "bash".into(),
            description: format!(
                "Execute a bash command in the current working directory. Returns combined stdout and stderr. Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
                DEFAULT_MAX_BYTES / 1024
            ),
            parameters: json!({"type":"object","properties":{"command":{"type":"string","description":"Bash command to execute"},"timeout":{"type":"number","description":"Timeout in seconds (optional, no default timeout)"}},"required":["command"]}),
            freeform: None,
            constrained_sampling: None,
        },
        execute: Arc::new(move |_, input, on_update, turn: T, _, context| {
            let options = options.clone();
            Box::pin(async move {
                let input: BashToolInput =
                    serde_json::from_value(input).map_err(|e| e.to_string())?;
                validate_timeout(input.timeout)?;
                let env = &turn.execution_tool_context().env;
                let mut execution = BashExecution {
                    command: match options.command_prefix.as_ref().filter(|s| !s.is_empty()) {
                        Some(prefix) => format!("{prefix}\n{}", input.command),
                        None => input.command,
                    },
                    cwd: env.cwd().into(),
                    env: Map::new(),
                    inherit_env: true,
                };
                if let Some(prepare) = &options.prepare {
                    prepare(&mut execution, &turn, &context).await?;
                }
                let state = Arc::new(Mutex::new(UpdateState {
                    view: None,
                    last_checkpoint_at: tokio::time::Instant::now(),
                    last_checkpoint: None,
                    accepting: true,
                }));
                let mut empty = AgentToolResult::text("");
                empty.content.clear();
                empty.details = Value::Null;
                on_update(empty, None);
                let updates = state.clone();
                let result = env
                    .exec(
                        &execution.command,
                        Some(ShellExecOptions {
                            cwd: Some(execution.cwd),
                            env: Some(execution.env),
                            inherit_env: Some(execution.inherit_env),
                            timeout: input.timeout,
                            capture: Some(ShellOutputCaptureOptions {
                                limits: ShellOutputLimits {
                                    max_bytes: DEFAULT_MAX_BYTES,
                                    max_lines: DEFAULT_MAX_LINES,
                                    retain: Some(ShellOutputRetention::Tail),
                                },
                                spill: Some(true),
                            }),
                            on_update: Some(Arc::new(move |update, _| {
                                let mut state = updates
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                if !state.accepting {
                                    return Box::pin(async {});
                                }
                                let view = apply_shell_output_update(state.view.as_ref(), &update);
                                let mut snapshot = AgentToolResult::text(&view.text);
                                let mut details = Map::new();
                                if view.truncation.truncated {
                                    details.insert("truncation".into(), json!(view.truncation));
                                }
                                if let Some(path) = &view.spill_path {
                                    details.insert("fullOutputPath".into(), json!(path));
                                }
                                snapshot.details = Value::Object(details);
                                let encoded = snapshot.details.to_string() + &view.text;
                                let now = tokio::time::Instant::now();
                                let checkpoint =
                                    now.duration_since(state.last_checkpoint_at).as_millis()
                                        >= 2000
                                        && state.last_checkpoint.as_ref() != Some(&encoded);
                                state.view = Some(view);
                                if checkpoint {
                                    state.last_checkpoint_at = now;
                                    state.last_checkpoint = Some(encoded);
                                }
                                on_update(
                                    snapshot,
                                    checkpoint.then_some(AgentHarnessToolUpdateOptions {
                                        checkpoint: Some(true),
                                    }),
                                );
                                Box::pin(async {})
                            })),
                        }),
                        &context,
                    )
                    .await;
                let view = {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.accepting = false;
                    state.view.take()
                };
                let mut output = view.as_ref().map(|v| v.text.clone()).unwrap_or_default();
                let capture = match &result {
                    Ok(value) => Some((
                        value.truncation.clone(),
                        value.spill_path.clone(),
                        value.last_line_bytes,
                    )),
                    Err(_) => view.map(|v| (v.truncation, v.spill_path, v.last_line_bytes)),
                };
                let mut details = Value::Null;
                if let Some((truncation, spill_path, last_line_bytes)) =
                    capture.filter(|(t, _, _)| t.truncated)
                {
                    details = serde_json::to_value(BashToolDetails {
                        truncation: Some(truncation.clone()),
                        full_output_path: spill_path.clone(),
                    })
                    .map_err(|e| e.to_string())?;
                    let path = spill_path.as_deref().unwrap_or("undefined");
                    let start = truncation.total_lines - truncation.output_lines + 1;
                    let end = truncation.total_lines;
                    if truncation.last_line_partial {
                        output.push_str(&format!(
                            "\n\n[Showing last {} of line {end} (line is {}). Full output: {path}]",
                            format_size(truncation.output_bytes),
                            format_size(last_line_bytes.unwrap_or(truncation.output_bytes))
                        ));
                    } else if truncation.truncated_by == Some(TruncationLimit::Lines) {
                        output.push_str(&format!(
                            "\n\n[Showing lines {start}-{end} of {}. Full output: {path}]",
                            truncation.total_lines
                        ));
                    } else {
                        output.push_str(&format!("\n\n[Showing lines {start}-{end} of {} ({} limit). Full output: {path}]", truncation.total_lines, format_size(DEFAULT_MAX_BYTES)));
                    }
                }
                match result {
                    Err(error) => {
                        let status = match error.code {
                            ExecutionErrorCode::Timeout => format!(
                                "Command timed out after {} seconds",
                                input.timeout.map_or("undefined".into(), |v| v.to_string())
                            ),
                            ExecutionErrorCode::Aborted => "Command aborted".into(),
                            _ => error.message,
                        };
                        Err(if output.is_empty() {
                            status
                        } else {
                            format!("{output}\n\n{status}")
                        })
                    }
                    Ok(result) if result.exit_code != 0 => Err(format!(
                        "{}Command exited with code {}",
                        if output.is_empty() {
                            String::new()
                        } else {
                            format!("{output}\n\n")
                        },
                        result.exit_code
                    )),
                    Ok(_) => {
                        let mut result = AgentToolResult::text(if output.is_empty() {
                            "(no output)".into()
                        } else {
                            output
                        });
                        result.details = details;
                        Ok(result)
                    }
                }
            })
        }),
    }
}
