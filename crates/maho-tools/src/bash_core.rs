//! Port of `packages/coding-agent/src/core/tools/bash-core.ts`.

use crate::bash_schema::{
    BashSpawnContext, BashSpawnHook, BashToolInput, ShellEnvProvider, ShellToolConfig,
    bash_output_schema_json, bash_schema_json, bash_tool_config,
};
use crate::definition::{ToolContext, ToolDefinition, ToolError, ToolExecutor, ToolExposure};
use crate::machine_output_spill::create_machine_output_spill;
use crate::output_accumulator_core::{
    CoreOutputAccumulator, CoreOutputAccumulatorOptions, OutputSpillFactory,
};
use crate::shell_operations::{
    BashExecOptions, BashOperations, create_shell_operations, machine_shell_exec_host,
};
use crate::shell_output::ShellOutputState;
use crate::tool_machine::ToolMachine;
use crate::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
use maho_machine_ops::{ExecShell, OpName};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const STRUCTURED_OUTPUT_MAX_BYTES: usize = 1024 * 1024;

fn deliver_output_update(
    state: &Mutex<Option<ShellOutputState>>,
    update: Option<(
        crate::definition::ToolUpdateCallback,
        crate::definition::ToolResult,
    )>,
) -> Result<(), ToolError> {
    if let Some((callback, result)) = update
        && let Err(error) = callback(result)
    {
        let mut guard = state
            .lock()
            .map_err(|_| ToolError::Message("Output accumulator lock poisoned".into()))?;
        if let Some(output) = guard.as_mut() {
            output.record_output_update_error(error);
        }
    }
    Ok(())
}

/// Optional trust annotation hook (`engine-trust/shell-trust.ts`).
pub type ShellTrustAnnotator =
    Arc<dyn Fn(i32, &str) -> Option<(Option<String>, Value)> + Send + Sync>;

#[derive(Clone)]
pub struct ShellToolCoreOptions {
    /// Runs the policy gate once per call.
    pub machine: ToolMachine,
    /// Where the command runs.
    pub operations: Arc<dyn BashOperations>,
    /// Where output past the display limits is kept.
    pub output_spill: OutputSpillFactory,
    /// Base environment before session `PI_*` values.
    pub shell_env: Option<ShellEnvProvider>,
    /// Optional trust annotator on non-zero exit.
    pub annotate_trust: Option<ShellTrustAnnotator>,
    /// Redact credential-shaped output before streaming or spilling. Default: false.
    pub redact_secrets_in_output: bool,
    /// Command prefix prepended to every command.
    pub command_prefix: Option<String>,
    /// Expose current Pi session metadata as `PI_*` environment variables. Default: true.
    pub expose_session_environment: Option<bool>,
    /// Hook to adjust command, cwd, or env before execution.
    pub spawn_hook: Option<BashSpawnHook>,
    /// SAFETY-DIVERGENCE(senpi tools/bash.ts): `Some(passthrough)` scrubs secret-looking variables
    /// from the final spawn environment, keeping the listed names (`SD-2`).
    pub env_passthrough: Option<Vec<String>>,
    /// SAFETY-DIVERGENCE(senpi tools/bash.ts): opt-in OS write-confinement (`SD-10`).
    pub sandbox: Option<crate::bash_sandbox::BashSandboxMode>,
    /// Agent dir holding sandbox `sessions`/`cache` and `tmp/bash-<session>` dirs.
    pub sandbox_agent_dir: Option<PathBuf>,
}

#[derive(Clone)]
pub struct CoreBashToolOptions {
    pub machine: ToolMachine,
    pub operations: Option<Arc<dyn BashOperations>>,
    pub shell: Option<ExecShell>,
    pub output_spill: Option<OutputSpillFactory>,
    pub redact_secrets_in_output: bool,
    pub command_prefix: Option<String>,
    pub expose_session_environment: Option<bool>,
    pub spawn_hook: Option<BashSpawnHook>,
}

impl CoreBashToolOptions {
    pub fn new(machine: ToolMachine) -> Self {
        Self {
            machine,
            operations: None,
            shell: None,
            output_spill: None,
            redact_secrets_in_output: false,
            command_prefix: None,
            expose_session_environment: None,
            spawn_hook: None,
        }
    }
}

pub fn resolve_spawn_context(
    command: String,
    cwd: PathBuf,
    hook: Option<&BashSpawnHook>,
    expose: bool,
    context: Option<&dyn ToolContext>,
    shell_env: Option<&ShellEnvProvider>,
) -> Result<BashSpawnContext, ToolError> {
    let mut env: BTreeMap<_, _> = shell_env.map_or_else(BTreeMap::new, |provider| provider());
    for key in [
        "PI_SESSION_ID",
        "PI_SESSION_FILE",
        "PI_SESSION_CWD",
        "PI_GOAL_STORE_FILE",
        "PI_PROVIDER",
        "PI_MODEL",
        "PI_REASONING_LEVEL",
        "OMO_BROWSER_ENGINE",
    ] {
        env.remove(key);
    }
    if expose && let Some(context) = context {
        env.insert(
            "PI_SESSION_ID".into(),
            context.session_manager().session_id().into(),
        );
        env.insert(
            "PI_SESSION_CWD".into(),
            context.cwd().to_string_lossy().into_owned(),
        );
        for (key, path) in [
            ("PI_SESSION_FILE", context.session_manager().session_file()),
            ("PI_GOAL_STORE_FILE", context.goal_store_file()),
        ] {
            if let Some(path) = path {
                env.insert(key.into(), path.to_string_lossy().into_owned());
            }
        }
        if let Some(model) = context.model() {
            let provider = serde_json::to_value(&model.provider)?;
            env.insert(
                "PI_PROVIDER".into(),
                provider.as_str().unwrap_or_default().into(),
            );
            env.insert("PI_MODEL".into(), model.id.clone());
        }
        if let Some(level) = context.thinking_level() {
            env.insert(
                "PI_REASONING_LEVEL".into(),
                serde_json::to_value(level)?
                    .as_str()
                    .unwrap_or_default()
                    .into(),
            );
        }
        if let Some(engine) = context.browser_engine() {
            env.insert("OMO_BROWSER_ENGINE".into(), engine.into());
        }
    }
    let base_context = BashSpawnContext { command, cwd, env };
    if let Some(hook) = hook {
        hook(base_context)
    } else {
        Ok(base_context)
    }
}

fn append_status(text: &str, status: &str) -> String {
    if text.is_empty() {
        status.to_owned()
    } else {
        format!("{text}\n\n{status}")
    }
}

/// Shared model-facing definition and execution core of `bash` and `powershell`.
pub fn create_shell_tool_core(
    cwd: PathBuf,
    config: ShellToolConfig,
    options: ShellToolCoreOptions,
) -> ToolDefinition {
    let expose_environment = options.expose_session_environment.unwrap_or(true);
    let sandbox_warned = Arc::new(AtomicBool::new(false));
    let config_for_exec = config.clone();

    let execute: ToolExecutor = Arc::new(move |call| {
        let cwd = cwd.clone();
        let options = options.clone();
        let config = config_for_exec.clone();
        let sandbox_warned = Arc::clone(&sandbox_warned);
        Box::pin(async move {
            let input: BashToolInput = serde_json::from_value(call.params)?;
            let gate_args = json!({ "command": input.command, "timeout": input.timeout });
            if let Some(admitted) =
                options
                    .machine
                    .begin(OpName::ProcessExec, gate_args, Some(&call.signal))
            {
                admitted.await?;
            }

            let resolved_command = match options.command_prefix.as_deref().filter(|p| !p.is_empty())
            {
                Some(prefix) => format!("{prefix}\n{}", input.command),
                None => input.command,
            };
            let mut context = resolve_spawn_context(
                resolved_command,
                call.context
                    .map_or(cwd.as_path(), ToolContext::cwd)
                    .to_path_buf(),
                options.spawn_hook.as_ref(),
                expose_environment,
                call.context,
                options.shell_env.as_ref(),
            )?;

            // SAFETY-DIVERGENCE(senpi tools/bash.ts): scrub the FINAL environment, after the spawn hook (`SD-2`).
            if let Some(passthrough) = options.env_passthrough.as_deref() {
                crate::env_scrub::scrub_secrets(&mut context.env, passthrough);
            }

            // SAFETY-DIVERGENCE(senpi tools/bash.ts): bash runs unconfined; opt-in bwrap/sandbox-exec
            // write confinement via `bashSandbox` (default off), applied after the hook and scrub (`SD-10`).
            if let Some(mode) = options
                .sandbox
                .filter(|mode| *mode != crate::bash_sandbox::BashSandboxMode::Off)
            {
                let agent_dir = options.sandbox_agent_dir.clone().ok_or_else(|| {
                    ToolError::Message(
                        "bashSandbox is enabled but no agent directory is configured".into(),
                    )
                })?;
                let session_id = call
                    .context
                    .map_or("default", |context| context.session_manager().session_id());
                let (private_tmp, writable) =
                    crate::bash_sandbox::prepare_dirs(&agent_dir, session_id)?;
                let (wrapped, notice) = crate::bash_sandbox::wrap_with_notice(
                    context,
                    mode,
                    &private_tmp,
                    &writable,
                    &crate::bash_sandbox::find_in_path,
                )?;
                context = wrapped;
                if let Some(notice) = notice
                    && !sandbox_warned.swap(true, Ordering::AcqRel)
                {
                    eprintln!("{notice}");
                }
            }

            let accumulator = CoreOutputAccumulator::new(
                CoreOutputAccumulatorOptions {
                    temp_file_prefix: config.temp_file_prefix.clone(),
                    ..Default::default()
                },
                Arc::clone(&options.output_spill),
            );
            let redact = options.redact_secrets_in_output;
            let state = Arc::new(Mutex::new(Some(ShellOutputState::new(
                accumulator,
                call.on_update.clone(),
                redact,
            ))));
            if let Some(on_update) = &call.on_update {
                on_update(crate::definition::ToolResult {
                    content: Vec::new(),
                    details: None,
                    structured_content: None,
                    is_error: None,
                })?;
            }
            let started_at = Instant::now();

            let data_changed = Arc::new(tokio::sync::Notify::new());
            let on_data = {
                let state = Arc::clone(&state);
                let data_changed = Arc::clone(&data_changed);
                Arc::new(
                    move |data: &[u8], stream: crate::shell_operations::BashStream| {
                        let stream_name = match stream {
                            crate::shell_operations::BashStream::Stdout => "stdout",
                            crate::shell_operations::BashStream::Stderr => "stderr",
                        };
                        let update = {
                            let mut guard = state.lock().map_err(|_| {
                                ToolError::Message("Output accumulator lock poisoned".into())
                            })?;
                            let Some(output_state) = guard.as_mut() else {
                                return Ok(());
                            };
                            output_state.handle_data(data, stream_name)?;
                            output_state.take_due_output_update()
                        };
                        deliver_output_update(&state, update)?;
                        data_changed.notify_one();
                        Ok(())
                    },
                )
            };

            let session_id = if expose_environment {
                call.context
                    .map(|ctx| ctx.session_manager().session_id().to_owned())
            } else {
                None
            };
            let env_opt = if options.shell_env.is_none()
                && options.spawn_hook.is_none()
                && context.env.is_empty()
                && options.env_passthrough.is_none()
            {
                None
            } else {
                Some(context.env)
            };

            let exec_future = options.operations.exec(
                &context.command,
                &context.cwd,
                BashExecOptions {
                    on_data,
                    redact_secrets_in_output: redact,
                    signal: call.signal,
                    timeout: input.timeout,
                    env: env_opt,
                    session_id,
                },
            );
            tokio::pin!(exec_future);

            let execution = loop {
                let next_deadline = {
                    let guard = state.lock().map_err(|_| {
                        ToolError::Message("Output accumulator lock poisoned".into())
                    })?;
                    guard
                        .as_ref()
                        .and_then(ShellOutputState::next_update_deadline)
                };
                tokio::select! {
                    biased;
                    result = &mut exec_future => break result,
                    () = data_changed.notified() => {},
                    () = async {
                        match next_deadline {
                            Some(dl) => tokio::time::sleep_until(dl).await,
                            None => std::future::pending().await,
                        }
                    }, if next_deadline.is_some() => {
                        let update = {
                            let mut guard = state
                                .lock()
                                .map_err(|_| ToolError::Message("Output accumulator lock poisoned".into()))?;
                            guard.as_mut().and_then(ShellOutputState::take_output_update)
                        };
                        deliver_output_update(&state, update)?;
                    }
                }
            };

            let mut output_state = state
                .lock()
                .map_err(|_| ToolError::Message("Output accumulator lock poisoned".into()))?
                .take()
                .ok_or_else(|| ToolError::Message("Output accumulator already finalized".into()))?;

            match execution {
                Ok(exit) => {
                    let snapshot = output_state.finish_output().await?;
                    let formatted = output_state.format_output(&snapshot, "(no output)");
                    let Some(code) = exit.exit_code else {
                        return Err(ToolError::Message(append_status(
                            &formatted.text,
                            "Command terminated without an exit code",
                        )));
                    };
                    let wall_time_seconds =
                        (started_at.elapsed().as_secs_f64() * 10.0).round() / 10.0;
                    let full_output = output_state
                        .accumulator
                        .read_full_output(STRUCTURED_OUTPUT_MAX_BYTES)
                        .await?;
                    let mut structured = json!({
                        "output": full_output.content,
                        "truncated": full_output.truncated,
                        "exit_code": code,
                        "wall_time_seconds": wall_time_seconds,
                    });
                    if full_output.truncated
                        && let Some(path) = &snapshot.full_output_path
                        && let Some(obj) = structured.as_object_mut()
                    {
                        obj.insert("full_output_path".to_owned(), json!(path));
                    }
                    let mut details_value = formatted
                        .details
                        .as_ref()
                        .map(|d| serde_json::to_value(d).unwrap_or_else(|_| json!({})));

                    if code != 0 {
                        let text = append_status(
                            &formatted.text,
                            &format!("Command exited with code {code}"),
                        );
                        if let Some(annotate) = &options.annotate_trust
                            && let Some((extra_text, trust)) = annotate(code, &full_output.content)
                        {
                            if let Some(obj) = structured.as_object_mut() {
                                obj.insert("trust".to_owned(), trust.clone());
                            }
                            let mut det = details_value.unwrap_or_else(|| json!({}));
                            if let Some(obj) = det.as_object_mut() {
                                obj.insert("trust".to_owned(), trust);
                            }
                            details_value = Some(det);
                            let final_text = match extra_text {
                                Some(extra) => format!("{text}\n\n{extra}"),
                                None => text,
                            };
                            return Ok(crate::definition::ToolResult {
                                content: vec![crate::definition::ToolContent::text(final_text)],
                                details: details_value,
                                structured_content: Some(structured),
                                is_error: Some(true),
                            });
                        }
                        return Ok(crate::definition::ToolResult {
                            content: vec![crate::definition::ToolContent::text(text)],
                            details: details_value,
                            structured_content: Some(structured),
                            is_error: Some(true),
                        });
                    }

                    Ok(crate::definition::ToolResult {
                        content: formatted.content,
                        details: details_value,
                        structured_content: Some(structured),
                        is_error: None,
                    })
                }
                Err(err) => {
                    let snapshot = match output_state.finish_output().await {
                        Ok(snapshot) => snapshot,
                        Err(finalization) => {
                            return Err(ToolError::ExecutionAndFinalization {
                                execution: Box::new(err),
                                finalization: Box::new(finalization),
                            });
                        }
                    };
                    let formatted = output_state.format_output(&snapshot, "");
                    let msg = err.to_string();
                    if msg == "aborted" {
                        return Err(ToolError::Message(append_status(
                            &formatted.text,
                            "Command aborted",
                        )));
                    }
                    if let Some(secs) = msg.strip_prefix("timeout:") {
                        return Err(ToolError::Message(append_status(
                            &formatted.text,
                            &format!("Command timed out after {secs} seconds"),
                        )));
                    }
                    if let Err(unlink_err) = output_state.accumulator.remove_temp_file().await {
                        return Err(ToolError::Message(format!(
                            "Bash command and output cleanup failed: {err}; {unlink_err}"
                        )));
                    }
                    Err(err)
                }
            }
        })
    });

    let mut tool = ToolDefinition::new(
        &config.name,
        &format!(
            "Execute a {} command in the current working directory. Returns stdout and stderr. Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
            config.shell_name,
            DEFAULT_MAX_BYTES / 1024
        ),
        bash_schema_json(),
        execute,
    );
    tool.output_schema = Some(bash_output_schema_json());
    tool.constrained_sampling = Some(maho_ai::types::ConstrainedSampling::Config(
        maho_ai::types::ConstrainedSamplingConfig::JsonSchema {
            strict: maho_ai::types::JsonSchemaStrictness::Prefer,
        },
    ));
    if expose_environment {
        tool.prompt_guidelines = config.prompt_guidelines;
    }
    tool.prompt_snippet = Some(config.prompt_snippet);
    tool
}

/// The bash tool for a host whose only effect channel is `machine`.
pub fn create_core_bash_tool_definition(
    cwd: PathBuf,
    options: CoreBashToolOptions,
) -> ToolDefinition {
    let machine = options.machine.clone();
    let operations = options.operations.unwrap_or_else(|| {
        create_shell_operations(
            "bash",
            machine.clone(),
            machine_shell_exec_host(machine.clone(), options.shell),
        )
    });
    let output_spill = options
        .output_spill
        .unwrap_or_else(|| create_machine_output_spill(machine.clone(), None));
    let mut def = create_shell_tool_core(
        cwd,
        bash_tool_config(),
        ShellToolCoreOptions {
            machine,
            operations,
            output_spill,
            shell_env: None,
            annotate_trust: None,
            redact_secrets_in_output: options.redact_secrets_in_output,
            command_prefix: options.command_prefix,
            expose_session_environment: options.expose_session_environment,
            spawn_hook: options.spawn_hook,
            env_passthrough: None,
            sandbox: None,
            sandbox_agent_dir: None,
        },
    );
    def.exposure = Some(ToolExposure::Eval);
    def
}
