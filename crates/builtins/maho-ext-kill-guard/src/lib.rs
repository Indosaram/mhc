//! Port of `packages/coding-agent/src/core/extensions/builtin/kill-guard/index.ts` (senpi#5349).

use maho_core::kill_guard::{
    ActivateKillGuardRuntimeOptions, KillGuardHolder, KillGuardSetting, KillGuardShell,
    KillGuardVantage, KillGuardVerdict, activate_kill_guard_runtime, host_kill_guard_context,
    judge_command, kill_guard_protected_pids, read_kill_guard_setting, release_kill_guard_runtime,
};
use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ExtensionFailure, JsonValue,
    NotificationType, ToolCallEventResult,
};
use std::sync::{Arc, Mutex};

fn is_submit_key(key: &str) -> bool {
    matches!(key.trim().to_ascii_lowercase().as_str(), "enter" | "return")
}

fn is_backspace(ch: char) -> bool {
    ch == '\x08' || ch == '\x7f'
}

fn is_line_break(ch: char) -> bool {
    ch == '\r' || ch == '\n'
}

fn is_control(ch: char) -> bool {
    let code = ch as u32;
    code < 0x20 || (0x7f..=0x9f).contains(&code)
}

/// The command text of a `bash` or `monitor` call, or `None` for any other tool.
pub fn command_of<'a>(tool_name: &str, input: &'a JsonValue) -> Option<&'a str> {
    if tool_name != "bash" && tool_name != "monitor" {
        return None;
    }
    input.get("command").and_then(JsonValue::as_str)
}

/// The lines a `bash_input` call submits when it is plain typing: printable text and backspace,
/// Enter from `submit`, a line break in the text or an `enter` key, and no other named key.
pub fn plain_submitted_lines(input: &JsonValue) -> Option<Vec<String>> {
    let keys_count = match input.get("keys").and_then(JsonValue::as_array) {
        Some(keys) => {
            if !keys
                .iter()
                .all(|key| key.as_str().is_some_and(is_submit_key))
            {
                return None;
            }
            keys.len()
        }
        None => 0,
    };
    let text = input.get("input").and_then(JsonValue::as_str).unwrap_or("");
    let submit = input
        .get("submit")
        .and_then(JsonValue::as_bool)
        .unwrap_or(!text.is_empty());

    let mut combined = String::with_capacity(text.len() + usize::from(submit) + keys_count);
    combined.push_str(text);
    if submit {
        combined.push('\r');
    }
    for _ in 0..keys_count {
        combined.push('\r');
    }

    let mut lines = Vec::new();
    let mut line = String::new();
    for ch in combined.chars() {
        if is_line_break(ch) {
            lines.push(std::mem::take(&mut line));
        } else if is_backspace(ch) {
            line.pop();
        } else if is_control(ch) {
            return None;
        } else {
            line.push(ch);
        }
    }
    Some(lines)
}

fn refusal(command: &str) -> Option<ToolCallEventResult> {
    // The PTY bash tool, the core bash tool and monitors start the command in a fresh session and process group.
    let verdict = judge_command(
        command,
        &host_kill_guard_context(KillGuardVantage::FreshSession, KillGuardShell::Bash),
    );
    match verdict {
        KillGuardVerdict::Refuse { message, .. } => Some(ToolCallEventResult {
            block: Some(true),
            reason: Some(message),
            terminate: None,
        }),
        KillGuardVerdict::Allow | KillGuardVerdict::Defer { .. } => None,
    }
}

struct State {
    enabled: bool,
    holder: KillGuardHolder,
}

impl State {
    fn new() -> Self {
        Self {
            enabled: true,
            holder: KillGuardHolder::new(),
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        release_kill_guard_runtime(self.holder);
    }
}

/// Refuses an agent shell command that would signal every process, a protected process (pid 1,
/// the agent, its ancestors, the user's service manager) or a process group holding one (senpi#5349).
#[derive(Default)]
pub struct KillGuardExtension;

impl Extension for KillGuardExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let state = Arc::new(Mutex::new(State::new()));

        let start_state = Arc::clone(&state);
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                let state = Arc::clone(&start_state);
                let cwd = ctx.cwd.clone();
                let agent_dir = ctx.agent_dir.clone();
                let ui = Arc::clone(&ctx.ui);
                Box::pin(async move {
                    let enabled =
                        read_kill_guard_setting(&cwd, Some(&agent_dir)) == KillGuardSetting::On;
                    let holder = {
                        let mut guard = state
                            .lock()
                            .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                        guard.enabled = enabled;
                        guard.holder
                    };
                    if !enabled {
                        // Only this session's hold: another session in the process keeps its own until it reads the setting.
                        release_kill_guard_runtime(holder);
                        ui.notify(
                            "kill guard off: agent commands may signal any process (\"terminal.killGuard\": \"off\")",
                            NotificationType::Warning,
                        );
                        return Ok(EventResult::None);
                    }
                    if let Err(error) = activate_kill_guard_runtime(ActivateKillGuardRuntimeOptions {
                        holder,
                        agent_dir: &agent_dir,
                        protected_pids: kill_guard_protected_pids,
                        platform: None,
                    }) {
                        ui.notify(
                            &format!(
                                "[senpi:kill-guard] the runtime shims could not be installed ({error}); signals whose targets are only known when a command runs are not judged"
                            ),
                            NotificationType::Warning,
                        );
                    }
                    Ok(EventResult::None)
                })
            }),
        );

        let shutdown_state = Arc::clone(&state);
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, _| {
                let state = Arc::clone(&shutdown_state);
                Box::pin(async move {
                    let holder = state
                        .lock()
                        .map_err(|error| ExtensionFailure::new(error.to_string()))?
                        .holder;
                    release_kill_guard_runtime(holder);
                    Ok(EventResult::None)
                })
            }),
        );

        let tool_state = Arc::clone(&state);
        api.on(
            EventKind::ToolCall,
            Arc::new(move |event, _| {
                let state = Arc::clone(&tool_state);
                let result = (|| -> Result<EventResult, ExtensionFailure> {
                    let enabled = state
                        .lock()
                        .map_err(|error| ExtensionFailure::new(error.to_string()))?
                        .enabled;
                    if !enabled {
                        return Ok(EventResult::None);
                    }
                    let ExtensionEvent::ToolCall(call) = event else {
                        return Ok(EventResult::None);
                    };
                    if call.tool_name == "bash_input" {
                        let Some(lines) = plain_submitted_lines(&call.input) else {
                            return Ok(EventResult::None);
                        };
                        if lines.is_empty() {
                            return Ok(EventResult::None);
                        }
                        let joined = lines.join("\n");
                        return Ok(
                            refusal(&joined).map_or(EventResult::None, EventResult::ToolCall)
                        );
                    }
                    let Some(command) = command_of(&call.tool_name, &call.input) else {
                        return Ok(EventResult::None);
                    };
                    Ok(refusal(command).map_or(EventResult::None, EventResult::ToolCall))
                })();
                Box::pin(async move { result })
            }),
        );
    }
}
