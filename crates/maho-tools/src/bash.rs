use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::Duration};
use serde::Deserialize;
use serde_json::json;
use crate::{definition::*, output_accumulator::{OutputAccumulator, OutputAccumulatorOptions, OutputSnapshot}, truncate::*, model_only_text::model_only_text};

pub type BashDataCallback = Arc<dyn Fn(&[u8]) -> Result<(), ToolError> + Send + Sync>;
pub struct BashExecOptions { pub on_data: BashDataCallback, pub signal: AbortSignal, pub timeout: Option<f64>, pub env: BTreeMap<String,String> }
pub struct BashExit { pub exit_code: Option<i32> }
pub trait BashOperations: Send + Sync {
    fn exec<'a>(&'a self, command: &'a str, cwd: &'a Path, options: BashExecOptions) -> ToolFuture<'a, BashExit>;
}
pub struct LocalShellOperations { pub shell_name: String, pub shell: String, pub args: Vec<String>, pub prefix: String }
pub fn resolve_timeout_ms(timeout: Option<f64>) -> Result<Option<Duration>, ToolError> {
    match timeout {
        None => Ok(None),
        Some(seconds) if !seconds.is_finite() || seconds <= 0.0 => Err(ToolError::Message("Invalid timeout: must be a finite number of seconds".into())),
        Some(seconds) if seconds * 1000.0 > 2_147_483_647.0 => Err(ToolError::Message("Invalid timeout: maximum is 2147483.647 seconds".into())),
        Some(seconds) => Ok(Some(Duration::from_secs_f64(seconds))),
    }
}
impl BashOperations for LocalShellOperations {
    fn exec<'a>(&'a self, command: &'a str, cwd: &'a Path, options: BashExecOptions) -> ToolFuture<'a, BashExit> {
        Box::pin(async move {
            let timeout = resolve_timeout_ms(options.timeout)?;
            if options.signal.is_aborted() { return Err(ToolError::Message("aborted".into())); }
            if tokio::fs::metadata(cwd).await.is_err() { return Err(ToolError::Message(format!("Working directory does not exist: {}\nCannot execute {} commands.", cwd.display(), self.shell_name))); }
            let mut settings = maho_pty::PtySessionOptions::new(&self.shell).cwd(cwd);
            settings.args = self.args.clone(); settings.args.push(format!("{}{command}", self.prefix));
            settings.env = options.env.into_iter().collect(); settings.timeout = timeout;
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
            let mut session = maho_pty::PtySession::start(settings, move |bytes| { if sender.send(bytes.to_vec()).is_err() { /* consumer has cancelled */ } })
                .map_err(|e| ToolError::Message(e.to_string()))?;
            let waiter = session.wait_in_background().map_err(|e| ToolError::Message(e.to_string()))?;
            let mut completion = tokio::task::spawn_blocking(move || waiter.join().map_err(|_| ToolError::Message("PTY wait thread panicked".into()))?.map_err(|e| ToolError::Message(e.to_string())));
            let mut callback_error = None; let mut cancelled = false; let mut receiving = true;
            let exit = loop {
                tokio::select! {
                    result = &mut completion => break result.map_err(|e| ToolError::Message(e.to_string()))??,
                    data = receiver.recv(), if receiving => match data {
                        Some(data) if callback_error.is_none() => if let Err(error) = (options.on_data)(&data) {
                            callback_error = Some(error); session.kill().map_err(|e| ToolError::Message(e.to_string()))?;
                        },
                        Some(_) => {},
                        None => receiving = false,
                    },
                    () = options.signal.cancelled(), if !cancelled => {
                        cancelled = true; session.kill().map_err(|e| ToolError::Message(e.to_string()))?;
                    }
                }
            };
            while let Ok(data) = receiver.try_recv() {
                if callback_error.is_none() && let Err(error) = (options.on_data)(&data) { callback_error = Some(error); }
            }
            if let Some(error) = callback_error { return Err(error); }
            if cancelled || options.signal.is_aborted() { return Err(ToolError::Message("aborted".into())); }
            if exit.timed_out { return Err(ToolError::Message(format!("timeout:{}", options.timeout.unwrap_or_default()))); }
            Ok(BashExit { exit_code: exit.exit_code })
        })
    }
}
pub fn create_local_bash_operations(shell: Option<&str>) -> Arc<dyn BashOperations> {
    Arc::new(LocalShellOperations { shell_name: "bash".into(), shell: shell.unwrap_or("bash").into(), args: vec!["-c".into()], prefix: if cfg!(unix) { "stty -echo -onlcr; exec </dev/null;\n".into() } else { String::new() } })
}
#[derive(Clone)]
pub struct BashSpawnContext { pub command: String, pub cwd: PathBuf, pub env: BTreeMap<String,String> }
pub type BashSpawnHook = Arc<dyn Fn(BashSpawnContext) -> Result<BashSpawnContext, ToolError> + Send + Sync>;
#[derive(Default, Clone)]
pub struct BashToolOptions {
    pub operations: Option<Arc<dyn BashOperations>>, pub command_prefix: Option<String>, pub shell_path: Option<String>,
    pub expose_session_environment: Option<bool>, pub spawn_hook: Option<BashSpawnHook>,
}
#[derive(Deserialize)]
pub struct BashToolInput { pub command: String, pub timeout: Option<f64> }
#[derive(Clone)]
pub struct ShellToolConfig { pub name: String, pub shell_name: String, pub prompt_snippet: String, pub temp_file_prefix: String }
pub fn resolve_spawn_context(command: String, cwd: PathBuf, hook: Option<&BashSpawnHook>, expose: bool, context: Option<&dyn ToolContext>) -> Result<BashSpawnContext, ToolError> {
    let mut env: BTreeMap<_,_> = std::env::vars().collect();
    for key in ["PI_SESSION_ID", "PI_SESSION_FILE", "PI_SESSION_CWD", "PI_GOAL_STORE_FILE", "PI_PROVIDER", "PI_MODEL", "PI_REASONING_LEVEL"] { env.remove(key); }
    if expose && let Some(context) = context {
            env.insert("PI_SESSION_ID".into(), context.session_manager().session_id().into());
            env.insert("PI_SESSION_CWD".into(), context.cwd().to_string_lossy().into_owned());
            for (key, path) in [("PI_SESSION_FILE", context.session_manager().session_file()), ("PI_GOAL_STORE_FILE", context.goal_store_file())] {
                if let Some(path) = path { env.insert(key.into(), path.to_string_lossy().into_owned()); }
            }
            if let Some(model) = context.model() {
                let provider = serde_json::to_value(&model.provider)?;
                env.insert("PI_PROVIDER".into(), provider.as_str().unwrap_or_default().into()); env.insert("PI_MODEL".into(), model.id.clone());
            }
            if let Some(level) = context.thinking_level() { env.insert("PI_REASONING_LEVEL".into(), serde_json::to_value(level)?.as_str().unwrap_or_default().into()); }
    }
    let context = BashSpawnContext { command, cwd, env }; if let Some(hook) = hook { hook(context) } else { Ok(context) }
}
fn format_output(snapshot: &OutputSnapshot, last_line_bytes: usize, empty: &str) -> (ToolResult, String) {
    let truncation = &snapshot.truncation;
    let text = if snapshot.content.is_empty() { empty } else { &snapshot.content };
    let notice = if truncation.truncated {
        let path = snapshot.full_output_path.as_ref().map_or("undefined".into(), |p| p.display().to_string());
        let start = truncation.total_lines - truncation.output_lines + 1; let end = truncation.total_lines;
        Some(if truncation.last_line_partial { format!("[Showing last {} of line {end} (line is {}). Full output: {path}]", format_size(truncation.output_bytes), format_size(last_line_bytes)) }
        else if truncation.truncated_by.as_deref() == Some("lines") { format!("[Showing lines {start}-{end} of {end}. Full output: {path}]") }
        else { format!("[Showing lines {start}-{end} of {end} ({} limit). Full output: {path}]", format_size(DEFAULT_MAX_BYTES)) })
    } else { None };
    let mut content = vec![ToolContent::text(format!("{text}{}", if notice.is_some() { "\n" } else { "" }))];
    let combined = if let Some(notice) = notice { content.push(model_only_text(&notice)); format!("{text}\n\n{notice}") } else { text.into() };
    (ToolResult { content, details: truncation.truncated.then(|| json!({"truncation":truncation,"fullOutputPath":snapshot.full_output_path})) }, combined)
}
pub fn create_shell_tool_definition(cwd: PathBuf, config: ShellToolConfig, options: BashToolOptions) -> ToolDefinition {
    let expose_environment = options.expose_session_environment.unwrap_or(true);
    let ops = options.operations.clone().unwrap_or_else(|| create_local_bash_operations(options.shell_path.as_deref()));
    let captured_config = config.clone();
    let execute: ToolExecutor = Arc::new(move |call| {
        let cwd = cwd.clone(); let options = options.clone(); let ops = Arc::clone(&ops); let config = captured_config.clone();
        Box::pin(async move {
            let input: BashToolInput = serde_json::from_value(call.params)?;
            let command = match options.command_prefix.as_deref().filter(|p| !p.is_empty()) { Some(prefix) => format!("{prefix}\n{}", input.command), None => input.command };
            let context = resolve_spawn_context(command, call.context.map_or(cwd.as_path(), ToolContext::cwd).to_path_buf(), options.spawn_hook.as_ref(), options.expose_session_environment.unwrap_or(true), call.context)?;
            let output = Arc::new(Mutex::new(OutputAccumulator::new(OutputAccumulatorOptions { temp_file_prefix: config.temp_file_prefix, ..Default::default() })));
            if let Some(update) = &call.on_update { update(ToolResult { content: Vec::new(), details: None })?; }
            let captured = Arc::clone(&output);
            let changed = Arc::new(tokio::sync::Notify::new());
            let data_changed = Arc::clone(&changed);
            let on_data: BashDataCallback = Arc::new(move |data| {
                let mut accumulator = captured.lock().map_err(|_| ToolError::Message("Output accumulator lock poisoned".into()))?;
                accumulator.append(data)?;
                data_changed.notify_one();
                Ok(())
            });
            let operation = ops.exec(&context.command, &context.cwd, BashExecOptions { on_data, signal: call.signal, timeout: input.timeout, env: context.env });
            tokio::pin!(operation);
            let mut dirty = false;
            let mut next_update = tokio::time::Instant::now();
            let mut update_error = None;
            let emit_update = || -> Result<(), ToolError> {
                if let Some(update) = &call.on_update {
                    let snapshot = output.lock().map_err(|_| ToolError::Message("Output accumulator lock poisoned".into()))?.snapshot(true)?;
                    update(ToolResult { content: vec![ToolContent::text(snapshot.content)], details: Some(json!({"truncation":snapshot.truncation.truncated.then_some(snapshot.truncation),"fullOutputPath":snapshot.full_output_path})) })?;
                }
                Ok(())
            };
            let execution = loop {
                tokio::select! {
                    biased;
                    () = changed.notified() => { dirty = true; },
                    result = &mut operation => break result,
                    () = tokio::time::sleep_until(next_update), if dirty && update_error.is_none() => {
                        dirty = false;
                        if let Err(error) = emit_update() { update_error = Some(error); }
                        next_update = tokio::time::Instant::now() + Duration::from_millis(100);
                    }
                }
            };
            let mut output = output.lock().map_err(|_| ToolError::Message("Output accumulator lock poisoned".into()))?;
            output.finish()?; let snapshot = output.snapshot(true)?;
            if update_error.is_none()
                && let Some(update) = &call.on_update
                && let Err(error) = update(ToolResult { content: vec![ToolContent::text(snapshot.content.clone())], details: Some(json!({"truncation":snapshot.truncation.truncated.then_some(&snapshot.truncation),"fullOutputPath":snapshot.full_output_path})) }) { update_error = Some(error); }
            output.close_temp_file()?;
            if let Some(error) = update_error { output.remove_temp_file()?; return Err(error); }
            match execution {
                Ok(exit) => {
                    let (result, text) = format_output(&snapshot, output.get_last_line_bytes(), "(no output)");
                    if let Some(code) = exit.exit_code.filter(|c| *c != 0) { return Err(ToolError::Message(format!("{text}\n\nCommand exited with code {code}"))); }
                    Ok(result)
                }
                Err(error) => {
                    let (_, text) = format_output(&snapshot, output.get_last_line_bytes(), "");
                    let message = error.to_string();
                    let status = if message == "aborted" { Some("Command aborted".into()) }
                        else { message.strip_prefix("timeout:").map(|seconds| format!("Command timed out after {seconds} seconds")) };
                    if let Some(status) = status { Err(ToolError::Message(if text.is_empty() { status } else { format!("{text}\n\n{status}") })) }
                    else { output.remove_temp_file()?; Err(error) }
                }
            }
        })
    });
    let mut tool = ToolDefinition::new(&config.name, &format!("Execute a {} command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.", config.shell_name), json!({"type":"object","properties":{"command":{"type":"string"},"timeout":{"type":"number"}},"required":["command"]}), execute);
    tool.constrained_sampling = Some(maho_ai::types::ConstrainedSampling::Config(maho_ai::types::ConstrainedSamplingConfig::JsonSchema { strict: maho_ai::types::JsonSchemaStrictness::Prefer }));
    if expose_environment { tool.prompt_guidelines = Some(vec!["You can inspect PI_* environment variables for current model and session details.".into()]); }
    tool.prompt_snippet = Some(config.prompt_snippet); tool.exposure = Some(ToolExposure::Eval); tool
}
pub fn create_bash_tool_definition(cwd: PathBuf, options: BashToolOptions) -> ToolDefinition {
    create_shell_tool_definition(cwd, ShellToolConfig { name: "bash".into(), shell_name: "bash".into(), prompt_snippet: "Execute bash commands (ls, rg, find, etc.)".into(), temp_file_prefix: "pi-bash".into() }, options)
}
