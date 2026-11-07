use super::context::{TerminalToolResult, error_result, noticed_result, text_result};
use super::spawn::describe_exit;
use crate::manager::TerminalManager;
use crate::output_format::format_terminal_tool_output;
use crate::shared::FOREGROUND_ENV_OVERRIDES;
use maho_pty::PtyExit;
use maho_tools::definition::ToolCall;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
pub fn pty_bash_schema() -> Value {
    json!({"type":"object","properties":{"command":{"type":"string","description":"Shell command to execute in a PTY-backed session."},"timeout":{"type":"number","description":"Process kill deadline in seconds (default 1800). Foreground blocking stops at the ~60s window: a command still running then auto-detaches to a live background session instead of being killed. Ignored for run_in_background sessions (they live until exit or kill_bash)."},"description":{"type":"string","description":"Short human-readable label for this command."},"run_in_background":{"type":"boolean","description":"Run as a persistent background session: returns a bash_id immediately (read with bash_output, steer with bash_input, stop with kill_bash). A plain & job dies with the call's PTY hangup (native backend), and nohup at best leaves an untracked orphan (no bash_id, no log) that the call's timeout still group-kills. This flag is the only reliable way a job outlives the call."},"cols":{"type":"number","description":"PTY width in columns (default 120)."},"rows":{"type":"number","description":"PTY height in rows (default 40)."}},"required":["command"]})
}
struct ForegroundSessionGuard {
    manager: Arc<Mutex<TerminalManager>>,
    id: String,
    armed: bool,
}
impl Drop for ForegroundSessionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(runtime) = self
            .manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&self.id)
        {
            if let Err(error) = runtime.kill() {
                maho_ext_api::diagnostic(&format!("terminal foreground cleanup failed: {error}"));
            }
            runtime.settle_cancelled();
        }
    }
}
async fn wait_exit(
    mut receiver: tokio::sync::watch::Receiver<Option<Result<PtyExit, String>>>,
) -> Result<PtyExit, String> {
    loop {
        if let Some(exit) = receiver.borrow_and_update().clone() {
            return exit;
        }
        receiver
            .changed()
            .await
            .map_err(|error| error.to_string())?;
    }
}
pub async fn execute_bash(
    manager: Arc<Mutex<TerminalManager>>,
    call: ToolCall<'_>,
) -> Result<TerminalToolResult, String> {
    execute_bash_with_window(
        manager,
        call,
        std::env::var("PI_BASH_FOREGROUND_SECONDS").ok().as_deref(),
    )
    .await
}
pub async fn execute_bash_with_window(
    manager: Arc<Mutex<TerminalManager>>,
    call: ToolCall<'_>,
    foreground_window: Option<&str>,
) -> Result<TerminalToolResult, String> {
    execute_configured_bash(
        manager,
        call,
        foreground_window,
        &crate::settings::TERMINAL_SETTINGS_DEFAULTS,
        None,
    )
    .await
}
pub async fn execute_configured_bash(
    manager: Arc<Mutex<TerminalManager>>,
    call: ToolCall<'_>,
    foreground_window: Option<&str>,
    settings: &crate::settings::ResolvedTerminalSettings,
    shell: Option<&str>,
) -> Result<TerminalToolResult, String> {
    execute_configured_bash_with_launch(manager, call, foreground_window, settings, shell, None)
        .await
}
/// bash.ts `sessionSpawnContext`: inherited session values a command must not see unless its own call's
/// context sets them.
pub const SESSION_SPAWN_DELETIONS: &[&str] =
    &["PI_SESSION_CWD", "PI_GOAL_STORE_FILE", "OMO_BROWSER_ENGINE"];
/// The `PI_*` variables a spawned command sees about its session (upstream `sessionEnvOverrides`): the session id,
/// cwd, goal store, session file, model, reasoning level and browser engine of the call's context.
pub fn session_env_overrides(
    context: Option<&dyn maho_tools::definition::ToolContext>,
) -> std::collections::BTreeMap<String, String> {
    let mut env = std::collections::BTreeMap::new();
    let Some(context) = context else {
        return env;
    };
    env.insert(
        "PI_SESSION_ID".to_owned(),
        context.session_manager().session_id().to_owned(),
    );
    env.insert(
        "PI_SESSION_CWD".to_owned(),
        context.cwd().to_string_lossy().into_owned(),
    );
    if let Some(path) = context.goal_store_file() {
        env.insert(
            "PI_GOAL_STORE_FILE".to_owned(),
            path.to_string_lossy().into_owned(),
        );
    }
    if let Some(path) = context.session_manager().session_file() {
        env.insert(
            "PI_SESSION_FILE".to_owned(),
            path.to_string_lossy().into_owned(),
        );
    }
    if let Some(model) = context.model() {
        env.insert("PI_PROVIDER".to_owned(), model.provider.clone());
        env.insert("PI_MODEL".to_owned(), model.id.clone());
    }
    if let Some(level) = context.thinking_level()
        && let Ok(serde_json::Value::String(level)) = serde_json::to_value(level)
    {
        env.insert("PI_REASONING_LEVEL".to_owned(), level);
    }
    if let Some(engine) = context.browser_engine().filter(|engine| !engine.is_empty()) {
        env.insert("OMO_BROWSER_ENGINE".to_owned(), engine.to_owned());
    }
    env
}
/// setTimeout's 32-bit delay range (bash.ts `MAX_TIMEOUT_MS`).
const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
/// bash.ts `scheduleDetachedSweep` deadline: the process deadline plus the kill grace, measured from
/// the spawn; none without a timeout or beyond the setTimeout range.
fn detached_sweep_delay(timeout_seconds: Option<f64>) -> Option<Duration> {
    let deadline_ms =
        (timeout_seconds? * 1000.0).trunc() + crate::shared::KILLED_SESSION_EXIT_GRACE_MS as f64;
    if !deadline_ms.is_finite() || deadline_ms > MAX_TIMEOUT_MS {
        return None;
    }
    Some(Duration::from_millis(deadline_ms.max(0.0) as u64))
}
/// bash.ts `scheduleDetachedSweep`: an auto-detached session keeps its process deadline; at
/// `deadline` a session that never settled is stopped so a later teardown cannot hang on it. The
/// sweep ends early when the session exits.
fn spawn_detached_sweep(
    manager: Arc<Mutex<TerminalManager>>,
    id: &str,
    runtime: &crate::runtime_session::TerminalRuntimeSession,
    deadline: tokio::time::Instant,
) {
    if runtime.exited().unwrap_or(true) {
        return;
    }
    let exit = runtime.subscribe_exit();
    let id = id.to_owned();
    tokio::spawn(async move {
        tokio::select! {
            _ = wait_exit(exit) => {}
            () = tokio::time::sleep_until(deadline) => {
                if let Err(error) = crate::manager::stop_shared(&manager, &id).await {
                    maho_ext_api::diagnostic(&format!("terminal detached sweep failed: {error}"));
                }
            }
        }
    });
}
/// `execute_configured_bash` with the tool child's launch (senpi#3566 todo 9): the command starts behind the
/// execution barrier, the cwd and environment are the ones the server built for the call, and the session is
/// registered under the shell's id prefix (`bash_c<generation>_N`) whatever shim process the PTY starts.
pub async fn execute_configured_bash_with_launch(
    manager: Arc<Mutex<TerminalManager>>,
    call: ToolCall<'_>,
    foreground_window: Option<&str>,
    settings: &crate::settings::ResolvedTerminalSettings,
    shell: Option<&str>,
    launch: Option<&super::context::TerminalChildLaunch>,
) -> Result<TerminalToolResult, String> {
    let command = call
        .params
        .get("command")
        .and_then(Value::as_str)
        .ok_or("command must be a string")?;
    let background = call
        .params
        .get("run_in_background")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !background && call.signal.is_aborted() {
        return Ok(error_result("Command aborted"));
    }
    let dimension = |key: &str, fallback: u16| {
        call.params
            .get(key)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 1.0)
            .map_or(fallback, |value| value.trunc() as u16)
    };
    let (options, stdin) = super::spawn::command_options(
        command,
        match launch {
            Some(launch) => Some(launch.cwd.as_path()),
            None => call.context.map(maho_tools::definition::ToolContext::cwd),
        },
        shell,
    )
    .map_err(|error| error.to_string())?;
    let id_command = options.command.clone();
    let mut options = options;
    if let Some(launch) = launch {
        // spawn.ts: `ctx.launchBarrier?.wrap(shell.shell, args)`; `env: { ...ctx.getEnv(), ...envOverrides }`.
        let (wrapped, wrapped_args) = launch.barrier.wrap(&options.command, &options.args);
        options.command = wrapped;
        options.args = wrapped_args;
        options.env = launch
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
    }
    let mut options = options.size(
        dimension("cols", settings.default_cols as u16),
        dimension("rows", settings.default_rows as u16),
    );
    if launch.is_none() {
        // bash.ts `sessionSpawnContext`: explicit deletions of the inherited session paths survive
        // backends that merge overrides with the host environment; the session overrides set them again.
        for key in SESSION_SPAWN_DELETIONS {
            options = options.env_remove(*key);
        }
    }
    if let Some(context) = call.context {
        options = options.cwd(context.cwd());
        for (key, value) in session_env_overrides(Some(context)) {
            options = options.env(key, value);
        }
    }
    if !background {
        for (key, value) in FOREGROUND_ENV_OVERRIDES {
            options = options.env(*key, *value);
        }
        if let Some(timeout) = call.params.get("timeout").and_then(Value::as_f64) {
            options = options
                .timeout(Duration::try_from_secs_f64(timeout).map_err(|error| error.to_string())?);
        }
    }
    let (id, exit, pid) = {
        let mut manager = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = manager
            .create_with_runtime_options(
                command,
                &id_command,
                options,
                settings.redact_secrets_in_output,
            )
            .map_err(|error| error.to_string())?;
        let runtime = manager.get(&id).ok_or("created terminal session missing")?;
        if launch.is_none()
            && let Some(stdin) = &stdin
        {
            runtime
                .write(stdin.as_bytes())
                .map_err(|error| error.to_string())?;
        }
        (id, runtime.subscribe_exit(), runtime.pid())
    };
    let spawned_at = tokio::time::Instant::now();
    let mut cleanup = ForegroundSessionGuard {
        manager: manager.clone(),
        id: id.clone(),
        armed: !background,
    };
    if let Some(launch) = launch {
        let writer_manager = manager.clone();
        let writer_id = id.clone();
        let released = launch
            .barrier
            .release(super::context::TerminalLaunchSession {
                pid,
                write: Arc::new(move |bytes| {
                    writer_manager
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(&writer_id)
                        .ok_or_else(|| "terminal session missing".to_owned())?
                        .write(bytes)
                        .map_err(|error| error.to_string())
                }),
                exit: exit.clone(),
            })
            .await;
        if let Err(error) = released {
            // The command never ran: its gated shim and its registration go with the refusal.
            cleanup.armed = false;
            if let Err(stop) = crate::manager::stop_shared(&manager, &id).await {
                maho_ext_api::diagnostic(&format!("terminal launch cleanup failed: {stop}"));
            }
            return Err(error);
        }
        if let Some(stdin) = &stdin {
            manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&id)
                .ok_or("terminal session missing")?
                .write(stdin.as_bytes())
                .map_err(|error| error.to_string())?;
        }
    }
    if background {
        tokio::select! {_=wait_exit(exit)=>{},_=tokio::time::sleep(Duration::from_millis(250))=>{}}
        let mut manager = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let early = format_terminal_tool_output(
            &manager
                .get(&id)
                .ok_or("terminal session missing")?
                .read_delta()
                .map_err(|error| error.to_string())?
                .text,
        )
        .text;
        let backend_note = if manager
            .get(&id)
            .ok_or("terminal session missing")?
            .backend()
            == "pipe-fallback"
        {
            "\n(native PTY unavailable \u{2014} running via pipe fallback; bash_input/bash_resize are limited.)"
        } else {
            ""
        };
        let mut result = text_result(format!(
            "Command running in background with ID: {id}{backend_note}{}",
            if early.is_empty() {
                String::new()
            } else {
                format!("\n\n{early}")
            }
        ));
        result.details = Some(
            json!({"bash_id":id,"background":true})
                .as_object()
                .ok_or("background details missing")?
                .clone(),
        );
        return Ok(result);
    }
    let sleep_wait =
        super::sleep_wait::classify_sleep_wait(command).map_err(|error| error.to_string())?;
    let window = super::foreground_window::resolve_foreground_window_seconds(foreground_window);
    let window = if sleep_wait.is_some() {
        window.min(super::foreground_window::SLEEP_WAIT_WINDOW_SECONDS)
    } else {
        window
    };
    let detach_delay = if settings.timeout_action == crate::settings::TimeoutAction::Kill
        || call
            .params
            .get("timeout")
            .and_then(Value::as_f64)
            .is_some_and(|timeout| timeout <= window)
    {
        Duration::MAX
    } else {
        Duration::from_secs_f64(window)
    };
    let timeout_grace = call
        .params
        .get("timeout")
        .and_then(Value::as_f64)
        .filter(|timeout| timeout.is_finite() && *timeout >= 0.0)
        .map(|timeout| {
            Duration::from_secs_f64(timeout)
                + Duration::from_millis(crate::shared::KILLED_SESSION_EXIT_GRACE_MS)
        });
    let outcome = super::foreground_detach::foreground_outcome(
        exit,
        &call.signal,
        detach_delay,
        timeout_grace,
    );
    tokio::pin!(outcome);
    let mut updates = if call.on_update.is_some() {
        Some(
            manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&id)
                .ok_or("terminal session missing")?
                .subscribe_output()
                .map_err(|error| error.to_string())?
                .1,
        )
    } else {
        None
    };
    let started_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs_f64()
        * 1000.0;
    let activity = format!(
        "running {}",
        String::from_utf16_lossy(&command.encode_utf16().take(80).collect::<Vec<_>>())
    );
    let emit = || -> Result<(), String> {
        // Format only a line-aligned raw tail (doubling until it yields 2000 units or covers the whole
        // buffer): the last 2000 body units equal those of formatting the full retained output.
        let mut window = 64 * 1024usize;
        let body = loop {
            let (raw, whole) = manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&id)
                .ok_or("terminal session missing")?
                .tail_output(window)
                .map_err(|error| error.to_string())?;
            let body = format_terminal_tool_output(&raw).body;
            if whole || body.encode_utf16().count() >= 2000 {
                break body;
            }
            window = window.saturating_mul(2);
        };
        // Last 2000 UTF-16 units without materializing the whole body as UTF-16; a split surrogate pair
        // decodes to U+FFFD exactly as the lossy slice did.
        let (mut start, mut taken, mut split) = (body.len(), 0usize, false);
        for (index, character) in body.char_indices().rev() {
            let units = character.len_utf16();
            if taken + units > 2000 {
                split = taken < 2000;
                break;
            }
            taken += units;
            start = index;
        }
        let text = if split {
            format!("\u{fffd}{}", &body[start..])
        } else {
            body[start..].to_owned()
        };
        if let Some(update) = &call.on_update {
            update(maho_tools::definition::ToolResult {
                content: vec![maho_tools::definition::ToolContent::text(text)],
                details: Some(json!({"progress":{"activity":activity,"startedAt":started_at}})),
                structured_content: None,
                is_error: None,
            })
            .map_err(|error| error.to_string())?;
        }
        Ok(())
    };
    if let Some(update) = &call.on_update {
        update(maho_tools::definition::ToolResult {
            content: vec![],
            details: None,
            structured_content: None,
            is_error: None,
        })
        .map_err(|error| error.to_string())?;
    }
    let mut last_emission = None;
    let mut trailing = None;
    let settled = loop {
        tokio::select! {
            settled=&mut outcome=>break settled?,
            chunk=async {updates.as_mut().expect("enabled updates").recv().await},if updates.is_some()=>{
                if chunk.is_none() {updates=None;continue;}
                let now=tokio::time::Instant::now();
                if last_emission.is_none_or(|last|now-last>=Duration::from_millis(100)) {emit()?;last_emission=Some(now);trailing=None;}else {trailing=last_emission.map(|last|last+Duration::from_millis(100));}
            }
            _=async {tokio::time::sleep_until(trailing.expect("pending progress")).await},if trailing.is_some()=>{emit()?;last_emission=Some(tokio::time::Instant::now());trailing=None;}
        }
    };
    if trailing.is_some() {
        emit()?;
    }
    let (outcome, sweep, timed_out_grace) = match settled {
        super::foreground_detach::ForegroundOutcome::Exit(exit) => (Some(exit), false, false),
        super::foreground_detach::ForegroundOutcome::Aborted => (None, true, false),
        super::foreground_detach::ForegroundOutcome::TimedOut => (None, true, true),
        super::foreground_detach::ForegroundOutcome::Detached => (None, false, false),
    };
    cleanup.armed = false;
    let sweep_manager = manager.clone();
    // Interrupt is "stop now": SIGKILL the whole process group in one shot (upstream). The exited
    // session is reaped by its own exit waiter and pruned by the manager, so no blocking exit wait
    // runs on the async runtime here (upstream never calls manager.stop on the abort path).
    if sweep
        && let Some(runtime) = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
    {
        if let Err(error) = runtime.kill() {
            maho_ext_api::diagnostic(&format!("terminal abort cleanup failed: {error}"));
        }
        runtime.settle_cancelled();
    }
    let mut manager = manager
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = manager.get(&id).ok_or("terminal session missing")?;
    let formatted =
        format_terminal_tool_output(&runtime.full_output().map_err(|error| error.to_string())?);
    if call.signal.is_aborted() {
        return Ok(error_result(format!(
            "{}Command aborted",
            if formatted.text.is_empty() {
                String::new()
            } else {
                format!("{}\n\n", formatted.text)
            }
        )));
    }
    // `${output ? `${output}\n\n` : ""}`: an empty output adds no leading blank lines.
    let output_prefix = if formatted.text.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", formatted.text)
    };
    let timed_out_after = || {
        error_result(format!(
            "{output_prefix}Command timed out after {} seconds",
            call.params.get("timeout").unwrap_or(&Value::Null)
        ))
    };
    if timed_out_grace {
        return Ok(timed_out_after());
    }
    if let Some(exit) = outcome {
        if exit.timed_out {
            return Ok(timed_out_after());
        }
        if let Some(code) = exit.exit_code
            && code != 0
        {
            return Ok(error_result(format!(
                "{output_prefix}Command exited with code {code}"
            )));
        }
        let mut result = noticed_result(
            if formatted.text.is_empty() {
                "(no output)"
            } else {
                &formatted.text
            },
            &[formatted.marker.as_deref()],
        );
        result.details = Some(
            json!({"status":describe_exit(Some(&exit))})
                .as_object()
                .ok_or("exit details missing")?
                .clone(),
        );
        return Ok(result);
    }
    if let Some(delay) = detached_sweep_delay(call.params.get("timeout").and_then(Value::as_f64)) {
        spawn_detached_sweep(sweep_manager, &id, runtime, spawned_at + delay);
    }
    let delta = runtime.read_delta().map_err(|error| error.to_string())?;
    let partial = format_terminal_tool_output(&delta.text).text;
    let timeout_note = call
        .params
        .get("timeout")
        .and_then(Value::as_f64)
        .filter(|timeout| timeout.is_finite())
        .map_or_else(
            || "not killed; it will run until exit or kill_bash".to_owned(),
            |timeout| format!("not killed; the original {timeout}s timeout still applies"),
        );
    let guidance = if let Some(wait) = &sleep_wait {
        format!(
            "This command is a wait ({}s sleep), so it detached immediately; the wait continues in the background. Do nothing and end your turn: completion will be reported automatically with exit status and output tail. Do NOT poll bash_output({{ bash_id: \"{id}\" }}) for it. When you are waiting for a pattern in a command's output, launch it with monitor({{ command, filter }}) instead so matching lines arrive as events. Use kill_bash({{ bash_id: \"{id}\" }}) to stop this session.",
            wait.seconds
        )
    } else {
        format!(
            "Continue other work; completion will be reported automatically with exit status and output tail. Use bash_output({{ bash_id: \"{id}\" }}) only to peek at new output. monitor cannot attach to this session; use it for future event-driven launches. Use kill_bash({{ bash_id: \"{id}\" }}) to stop this session."
        )
    };
    let mut result = text_result(format!(
        "Command is still running; auto-detached to background with ID: {id} ({timeout_note}).\n\nPartial output:\n{}\n\n{guidance}",
        if partial.is_empty() {
            "(no output yet)"
        } else {
            &partial
        }
    ));
    let mut details =
        json!({"bash_id":id,"background":true,"auto_detached":true,"status":"running"});
    if sleep_wait.is_some() {
        details["sleep_wait"] = json!(true);
    }
    if delta.dropped_chars > 0 {
        details["droppedChars"] = json!(delta.dropped_chars);
    }
    result.details = details.as_object().cloned();
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn dropping_foreground_future_settles_subscribed_exit() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let running = manager.clone();
        let (sender, mut ready) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            execute_bash(
                running,
                ToolCall {
                    id: "dropped",
                    params: json!({"command":"read value","timeout":10}),
                    signal: Default::default(),
                    on_update: Some(Arc::new(move |_| {
                        sender.send(()).unwrap();
                        Ok(())
                    })),
                    context: None,
                },
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), ready.recv())
            .await
            .unwrap()
            .unwrap();
        let mut exit = manager
            .lock()
            .unwrap()
            .list()
            .first()
            .unwrap()
            .1
            .subscribe_exit();
        task.abort();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled()
        );
        let settled = tokio::time::timeout(Duration::from_secs(5), exit.wait_for(Option::is_some))
            .await
            .unwrap()
            .unwrap()
            .clone()
            .unwrap()
            .unwrap();
        assert!(settled.cancelled);
        crate::manager::teardown_shared(&manager).await.unwrap();
    }
    struct SessionContext(std::path::PathBuf);
    impl maho_tools::definition::ToolSessionManager for SessionContext {
        fn session_id(&self) -> &str {
            "terminal-session"
        }
        fn session_file(&self) -> Option<&std::path::Path> {
            Some(std::path::Path::new("/tmp/session.jsonl"))
        }
    }
    impl maho_tools::definition::ToolContext for SessionContext {
        fn cwd(&self) -> &std::path::Path {
            &self.0
        }
        fn model(&self) -> Option<&maho_ai::model::Model> {
            None
        }
        fn thinking_level(&self) -> Option<maho_ai::types::ThinkingLevel> {
            Some(maho_ai::types::ThinkingLevel::High)
        }
        fn session_manager(&self) -> &dyn maho_tools::definition::ToolSessionManager {
            self
        }
        fn goal_store_file(&self) -> Option<&std::path::Path> {
            Some(std::path::Path::new("/tmp/goal.json"))
        }
    }
    fn call<'a>(params: Value) -> ToolCall<'a> {
        ToolCall {
            id: "call",
            params,
            signal: Default::default(),
            on_update: None,
            context: None,
        }
    }
    fn text_of(result: &TerminalToolResult) -> String {
        result
            .content
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
    /// terminal-bash-tool-output.test.ts: foreground commands get the non-interactive environment.
    #[tokio::test]
    async fn foreground_commands_get_the_non_interactive_environment() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let result = execute_bash(manager.clone(), call(json!({"command":"printf '%s|%s|%s|%s|%s|%s' \"$NO_COLOR\" \"$TERM\" \"$PAGER\" \"$GH_PAGER\" \"$GIT_EDITOR\" \"$GIT_TERMINAL_PROMPT\"","timeout":10}))).await.unwrap();
        assert_eq!(text_of(&result), "1|dumb|cat|cat|true|0");
        crate::manager::teardown_shared(&manager).await.unwrap();
    }
    /// ...and background sessions keep the inherited environment (no non-interactive overrides).
    #[tokio::test]
    async fn background_sessions_do_not_get_the_non_interactive_environment() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let result = execute_bash(manager.clone(), call(json!({"command":"printf 'EDITOR=%s PROMPT=%s' \"$GIT_EDITOR\" \"$GIT_TERMINAL_PROMPT\"","run_in_background":true}))).await.unwrap();
        let id = result.details.as_ref().unwrap()["bash_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let exit = manager.lock().unwrap().get(&id).unwrap().subscribe_exit();
        tokio::time::timeout(Duration::from_secs(5), wait_exit(exit))
            .await
            .unwrap()
            .unwrap();
        let inherited = |key: &str| maho_ext_api::process_env::var(key).unwrap_or_default();
        assert!(
            manager
                .lock()
                .unwrap()
                .get(&id)
                .unwrap()
                .full_output()
                .unwrap()
                .contains(&format!(
                    "EDITOR={} PROMPT={}",
                    inherited("GIT_EDITOR"),
                    inherited("GIT_TERMINAL_PROMPT")
                ))
        );
        crate::manager::teardown_shared(&manager).await.unwrap();
    }
    /// terminal-bash-session-env.test.ts: inherited session paths are cleared without a session
    /// context, and a context without a goal store clears only that path.
    #[tokio::test]
    async fn inherited_session_paths_are_cleared_unless_the_context_sets_them() {
        maho_ext_api::process_env::set("PI_SESSION_CWD", "stale-cwd");
        maho_ext_api::process_env::set("PI_GOAL_STORE_FILE", "stale-goal.json");
        let command = "printf '%s|%s' \"${PI_SESSION_CWD-unset}\" \"${PI_GOAL_STORE_FILE-unset}\"";
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let result = execute_bash(
            manager.clone(),
            call(json!({"command":command,"timeout":10})),
        )
        .await
        .unwrap();
        assert_eq!(text_of(&result), "unset|unset");
        struct NoGoal(SessionContext);
        impl maho_tools::definition::ToolContext for NoGoal {
            fn cwd(&self) -> &std::path::Path {
                &self.0.0
            }
            fn model(&self) -> Option<&maho_ai::model::Model> {
                None
            }
            fn thinking_level(&self) -> Option<maho_ai::types::ThinkingLevel> {
                None
            }
            fn session_manager(&self) -> &dyn maho_tools::definition::ToolSessionManager {
                &self.0
            }
            fn goal_store_file(&self) -> Option<&std::path::Path> {
                None
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let context = NoGoal(SessionContext(dir.path().to_path_buf()));
        let result = execute_bash(
            manager.clone(),
            ToolCall {
                context: Some(&context),
                ..call(json!({"command":command,"timeout":10}))
            },
        )
        .await
        .unwrap();
        assert_eq!(text_of(&result), format!("{}|unset", dir.path().display()));
        maho_ext_api::process_env::delete("PI_SESSION_CWD");
        maho_ext_api::process_env::delete("PI_GOAL_STORE_FILE");
        crate::manager::teardown_shared(&manager).await.unwrap();
    }
    /// terminal-bash-tool-output.test.ts: ANSI sequences are stripped and spinner frames collapse.
    #[tokio::test]
    async fn foreground_result_strips_ansi_and_collapses_spinner_frames() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let script = "i=0; while [ $i -lt 2000 ]; do printf '\\r\\033[K\\033[36m*\\033[0m'; i=$((i+1)); done; printf '\\r\\033[K\\033[32mOK\\033[0m finished\\n'";
        let result = execute_bash(
            manager.clone(),
            call(json!({"command":script,"timeout":30})),
        )
        .await
        .unwrap();
        let text = text_of(&result);
        assert_eq!(text, "OK finished");
        assert!(!text.contains('\u{1b}'));
        crate::manager::teardown_shared(&manager).await.unwrap();
    }
    /// bash.ts error results add no leading blank lines when the command printed nothing.
    #[tokio::test]
    async fn silent_failures_and_timeouts_report_without_a_blank_prefix() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let failed = execute_bash(
            manager.clone(),
            call(json!({"command":"exit 3","timeout":10})),
        )
        .await
        .unwrap();
        assert_eq!(failed.is_error, Some(true));
        assert_eq!(text_of(&failed), "Command exited with code 3");
        let printed = execute_bash(
            manager.clone(),
            call(json!({"command":"printf 'partial'; exit 4","timeout":10})),
        )
        .await
        .unwrap();
        assert_eq!(text_of(&printed), "partial\n\nCommand exited with code 4");
        // The timeout is the behavior under test: a 0.2s process deadline on a blocked read.
        let timed_out = execute_bash(
            manager.clone(),
            call(json!({"command":"stty -echo; read value","timeout":0.2})),
        )
        .await
        .unwrap();
        assert_eq!(timed_out.is_error, Some(true));
        assert_eq!(text_of(&timed_out), "Command timed out after 0.2 seconds");
        let completed = execute_bash(
            manager.clone(),
            call(json!({"command":"true","timeout":10})),
        )
        .await
        .unwrap();
        assert_eq!(text_of(&completed), "(no output)");
        assert_eq!(completed.details.as_ref().unwrap()["status"], "completed");
        crate::manager::teardown_shared(&manager).await.unwrap();
    }
    /// "never hands a process deadline to an explicit background session": the timeout is ignored.
    #[tokio::test]
    async fn explicit_background_sessions_ignore_the_timeout() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let started = execute_bash(
            manager.clone(),
            call(
                json!({"command":"stty -echo; read value; printf done","run_in_background":true,"timeout":0.01}),
            ),
        )
        .await
        .unwrap();
        assert!(text_of(&started).starts_with("Command running in background with ID: bash_1"));
        assert_eq!(started.details.as_ref().unwrap()["background"], true);
        // Released only after the call returned: a 10 ms process deadline would long have fired.
        let exit = {
            let mut manager = manager.lock().unwrap();
            let runtime = manager.get("bash_1").unwrap();
            runtime.write(b"go\n").unwrap();
            runtime.subscribe_exit()
        };
        let exit = tokio::time::timeout(Duration::from_secs(5), wait_exit(exit))
            .await
            .unwrap()
            .unwrap();
        assert!(!exit.timed_out);
        assert_eq!(exit.exit_code, Some(0));
        crate::manager::teardown_shared(&manager).await.unwrap();
    }
    #[test]
    fn detached_sweep_waits_for_the_deadline_plus_kill_grace() {
        assert_eq!(detached_sweep_delay(None), None);
        assert_eq!(
            detached_sweep_delay(Some(900.0)),
            Some(Duration::from_millis(905_000))
        );
        assert_eq!(
            detached_sweep_delay(Some(0.0015)),
            Some(Duration::from_millis(5_001))
        );
        assert_eq!(detached_sweep_delay(Some(f64::INFINITY)), None);
        assert_eq!(detached_sweep_delay(Some(3_000_000.0)), None);
    }
    /// "auto-detaches ... and sweeps after its native grace": at the sweep deadline a still-running
    /// detached session is stopped (the paused clock jumps to the deadline once the runtime idles).
    #[tokio::test(start_paused = true)]
    async fn detached_sweep_stops_a_session_still_running_at_the_deadline() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let id = manager
            .lock()
            .unwrap()
            .create("read", crate::shared::test_sh_options("read value"))
            .unwrap();
        let exit = {
            let mut guard = manager.lock().unwrap();
            let runtime = guard.get(&id).unwrap();
            spawn_detached_sweep(
                manager.clone(),
                &id,
                runtime,
                tokio::time::Instant::now() + Duration::from_secs(905),
            );
            runtime.subscribe_exit()
        };
        let exit = wait_exit(exit).await.unwrap();
        assert!(exit.cancelled);
        crate::manager::teardown_shared(&manager).await.unwrap();
    }
    #[tokio::test]
    async fn huge_foreground_output_has_model_only_truncation_notice() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let result = execute_bash(
            manager.clone(),
            ToolCall {
                id: "huge",
                params: json!({"command":"printf '%0204800d' 0","timeout":5}),
                signal: Default::default(),
                on_update: None,
                context: None,
            },
        )
        .await
        .unwrap();
        assert!(
            result
                .content
                .iter()
                .map(|part| part.text.len())
                .sum::<usize>()
                < crate::output_format::TERMINAL_TOOL_MAX_BYTES * 2
        );
        let marker = result.content.last().unwrap();
        assert_eq!(marker.audience, Some(maho_ai::types::TextAudience::Model));
        assert!(marker.text.contains("earlier output dropped"));
        assert!(
            !result
                .content
                .iter()
                .filter(|part| part.audience.is_none())
                .any(|part| part.text.contains("earlier output dropped"))
        );
        manager.lock().unwrap().teardown().unwrap();
    }
    #[tokio::test]
    async fn pre_aborted_foreground_does_not_spawn() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let signal = maho_tools::definition::AbortSignal::default();
        signal.abort();
        let result = execute_bash(
            manager.clone(),
            ToolCall {
                id: "pre-aborted",
                params: json!({"command":"echo never-runs"}),
                signal,
                on_update: None,
                context: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(result.is_error, Some(true));
        assert!(result.content[0].text.contains("Command aborted"));
        assert_eq!(manager.lock().unwrap().size(), 0);
    }
    #[tokio::test]
    async fn abort_kills_a_shell_that_ignores_sigterm_after_ready_signal() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let running = manager.clone();
        let signal = maho_tools::definition::AbortSignal::default();
        let cancel = signal.clone();
        let (sender, mut ready) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            execute_bash(running,ToolCall {id:"abort",params:json!({"command":"stty -echo; trap '' TERM; read start; printf 'trap-ready\\n'; while :; do :; done","timeout":10}),signal,on_update:Some(Arc::new(move |update| {sender.send(update).unwrap();Ok(())})),context:None}).await
        });
        tokio::time::timeout(Duration::from_secs(5),async {
            // The first update is emitted only after the output subscription is installed, so
            // receiving it proves the observer is attached; only then trigger the shell so the ready
            // line cannot be produced (and missed) before we are watching for it.
            assert!(ready.recv().await.unwrap().content.is_empty(),"first update is the subscription marker");
            manager.lock().unwrap().get("bash_1").unwrap().write(b"go\n").unwrap();
            let mut observed=false;
            while let Some(update)=ready.recv().await {if update.content.iter().any(|part|matches!(part,maho_tools::definition::ToolContent::Text {text,..} if text.contains("trap-ready"))) {observed=true;break;}}
            // An event-stream disconnect before the ready line must fail the test, never fall through.
            assert!(observed,"the trap-ready output must be observed before aborting");
            let mut exit=manager.lock().unwrap().get("bash_1").unwrap().subscribe_exit();
            cancel.abort();
            let result=task.await.unwrap().unwrap();
            assert_eq!(result.is_error,Some(true));
            assert!(result.content.iter().any(|part|part.text.contains("Command aborted")));
            // The SIGKILLed shell must actually exit and be reaped by its exit waiter.
            tokio::time::timeout(Duration::from_secs(5),async {while exit.borrow_and_update().is_none() {exit.changed().await.unwrap();}}).await.unwrap();
            assert!(exit.borrow().is_some(),"the aborted shell must be killed and reaped");
        }).await.unwrap();
        manager.lock().unwrap().teardown().unwrap();
    }
    /// terminal-bash-abort.test.ts: a descendant that escaped the group kill (its own session)
    /// keeps the PTY slave open and chattering; abort and timeout must still release the tool.
    /// Linux only: `setsid` is a util-linux binary absent on macOS.
    /// The command records the escaped pid in `pid_file` so the test can kill it afterwards.
    #[cfg(target_os = "linux")]
    fn escaped_descendant_command(pid_file: &std::path::Path) -> String {
        format!(
            "setsid sh -c 'while :; do echo orphan-tick; sleep 0.05; done' & echo $! > '{}'; while :; do sleep 1; done",
            pid_file.display()
        )
    }
    #[cfg(target_os = "linux")]
    fn kill_escaped(pid_file: &std::path::Path) {
        if let Some(pid) = std::fs::read_to_string(pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn abort_releases_while_an_escaped_descendant_holds_the_pty_open() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("orphan.pid");
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let signal = maho_tools::definition::AbortSignal::default();
        let cancel = signal.clone();
        let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
        let running = manager.clone();
        let command = escaped_descendant_command(&pid_file);
        let task = tokio::spawn(async move {
            execute_bash(
                running,
                ToolCall {
                    id: "escaped-abort",
                    params: json!({"command": command, "timeout": 60}),
                    signal,
                    on_update: Some(Arc::new(move |update| {
                        sender.send(update).unwrap();
                        Ok(())
                    })),
                    context: None,
                },
            )
            .await
        });
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            // Abort only once the escaped descendant is proven alive and writing.
            while let Some(update) = updates.recv().await {
                if update.content.iter().any(|part| matches!(part, maho_tools::definition::ToolContent::Text { text, .. } if text.contains("orphan-tick"))) {
                    break;
                }
            }
            cancel.abort();
            task.await.unwrap().unwrap()
        })
        .await
        .expect("abort releases the tool while the PTY is held open");
        kill_escaped(&pid_file);
        assert_eq!(result.is_error, Some(true));
        assert!(text_of(&result).contains("Command aborted"));
        crate::manager::teardown_shared(&manager).await.unwrap();
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn timeout_releases_while_an_escaped_descendant_holds_the_pty_open() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("orphan.pid");
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            execute_bash(
                manager.clone(),
                call(json!({"command": escaped_descendant_command(&pid_file), "timeout": 1})),
            ),
        )
        .await
        .expect("timeout releases the tool while the PTY is held open")
        .unwrap();
        kill_escaped(&pid_file);
        assert_eq!(result.is_error, Some(true));
        assert!(
            text_of(&result).contains("Command timed out after 1 seconds"),
            "{}",
            text_of(&result)
        );
        crate::manager::teardown_shared(&manager).await.unwrap();
    }
    #[tokio::test]
    async fn subscribed_progress_reports_output_before_child_is_released() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let running = manager.clone();
        let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            execute_bash(running,ToolCall {id:"progress",params:json!({"command":"stty -echo; read start; printf 'progress-ready\\n'; read value; printf '%s' \"$value\"","timeout":5}),signal:Default::default(),on_update:Some(Arc::new(move |update| {sender.send(update).unwrap();Ok(())})),context:None}).await
        });
        tokio::time::timeout(Duration::from_secs(5),async {
            assert!(updates.recv().await.unwrap().content.is_empty());
            manager.lock().unwrap().get("bash_1").unwrap().write(b"begin\n").unwrap();
            let mut observed=false;
            while let Some(update)=updates.recv().await {
                assert!(matches!(update.content.first(),Some(maho_tools::definition::ToolContent::Text {..})));
                assert!(update.details.as_ref().unwrap()["progress"]["startedAt"].is_number());
                if update.content.iter().any(|part|matches!(part,maho_tools::definition::ToolContent::Text {text,..} if text.contains("progress-ready"))) {observed=true;break;}
            }
            assert!(observed,"the progress-ready output must be observed before releasing the child");
            manager.lock().unwrap().get("bash_1").unwrap().write(b"released\n").unwrap();let result=task.await.unwrap().unwrap();assert!(result.content[0].text.contains("released"));
        }).await.unwrap();
        manager.lock().unwrap().teardown().unwrap();
    }
    #[tokio::test]
    async fn session_context_metadata_reaches_real_pty_children() {
        let dir = tempfile::tempdir().unwrap();
        let context = SessionContext(dir.path().to_path_buf());
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let result=execute_bash(manager.clone(),ToolCall {id:"env",params:json!({"command":"printf '%s|%s|%s|%s|%s' \"$PI_SESSION_ID\" \"$PI_SESSION_CWD\" \"$PI_SESSION_FILE\" \"$PI_GOAL_STORE_FILE\" \"$PI_REASONING_LEVEL\"","timeout":5}),signal:Default::default(),on_update:None,context:Some(&context)}).await.unwrap();
        assert_eq!(
            result.content[0].text,
            format!(
                "terminal-session|{}|/tmp/session.jsonl|/tmp/goal.json|high",
                dir.path().display()
            )
        );
        manager.lock().unwrap().teardown().unwrap();
    }
    #[tokio::test]
    async fn foreground_window_detaches_live_read_without_killing_runtime() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let result = execute_bash_with_window(
            manager.clone(),
            ToolCall {
                id: "detach",
                params: json!({"command":"stty -echo; read value","timeout":10}),
                signal: Default::default(),
                on_update: None,
                context: None,
            },
            Some("0.001"),
        )
        .await
        .unwrap();
        assert_eq!(result.details.as_ref().unwrap()["auto_detached"], true);
        assert!(
            !manager
                .lock()
                .unwrap()
                .get("bash_1")
                .unwrap()
                .exited()
                .unwrap()
        );
        manager.lock().unwrap().teardown().unwrap();
    }
}
