use serde_json::{Map, Value};
use crate::diagnostics::{DiagnosticDraft, diagnostic};
use crate::types::{CommandHookConfig, ExecutableHookHandler, HookDiagnostic, HookSourceMetadata, Severity, SupportedHookEvent};

pub struct HandlerParseContext<'a> {
    pub event: SupportedHookEvent,
    pub matcher: Option<String>,
    pub group_index: usize,
    pub handler_index: usize,
    pub source: &'a HookSourceMetadata,
    pub diagnostics: &'a mut Vec<HookDiagnostic>,
}

impl HandlerParseContext<'_> {
    fn add(&mut self, code: &str, path: &str, message: String, severity: Option<Severity>) {
        self.diagnostics.push(diagnostic(DiagnosticDraft {
            code, path: path.to_owned(), message,
            event: Some(self.event.as_str()), severity,
        }, self.source));
    }
}

pub fn parse_handler(handler: &Value, context: &mut HandlerParseContext<'_>) -> Option<ExecutableHookHandler> {
    let path = format!("hooks.{}[{}].hooks[{}]", context.event.as_str(), context.group_index, context.handler_index);
    let Some(handler) = handler.as_object() else {
        context.add("invalid_handler", &path, "Hook handler must be an object.".to_owned(), None);
        return None;
    };
    if handler.get("type").and_then(Value::as_str) != Some("command") {
        context.add("unsupported_handler_type", &format!("{path}.type"), "Only type: \"command\" hook handlers are executable in builtin hooks v1.".to_owned(), Some(Severity::Warning));
        return None;
    }
    if has_unsupported_command_shape(handler, &path, context) { return None; }
    let Some(command) = handler.get("command").and_then(Value::as_str) else {
        context.add("invalid_command", &format!("{path}.command"), "Command hook command must be a string.".to_owned(), None);
        return None;
    };
    let windows = handler.get("commandWindows").filter(|v| !v.is_null()).or_else(|| handler.get("command_windows"));
    if windows.is_some_and(|v| !v.is_string()) {
        context.add("invalid_command_windows", &format!("{path}.commandWindows"), "commandWindows must be a string.".to_owned(), None);
        return None;
    }
    let timeout = handler.get("timeout");
    if timeout.is_some_and(|v| !v.as_f64().is_some_and(|v| v.is_finite() && v > 0.0)) {
        context.add("invalid_timeout", &format!("{path}.timeout"), "Command hook timeout must be a finite number greater than 0.".to_owned(), None);
        return None;
    }
    let status = handler.get("statusMessage");
    if status.is_some_and(|v| !v.is_string()) {
        context.add("invalid_status_message", &format!("{path}.statusMessage"), "Command hook statusMessage must be a string.".to_owned(), None);
        return None;
    }
    Some(ExecutableHookHandler {
        config: CommandHookConfig { kind: "command".to_owned(), command: command.to_owned(), command_windows: windows.and_then(Value::as_str).map(str::to_owned), timeout: timeout.and_then(Value::as_f64), status_message: status.and_then(Value::as_str).map(str::to_owned) },
        event: context.event, group_index: context.group_index, handler_index: context.handler_index,
        source: context.source.clone(), matcher: context.matcher.clone(),
    })
}

fn has_unsupported_command_shape(handler: &Map<String, Value>, path: &str, context: &mut HandlerParseContext<'_>) -> bool {
    let mut unsupported = false;
    for field in ["if", "shell", "asyncRewake", "terminalSequence", "continueOnBlock"] {
        if handler.contains_key(field) {
            unsupported = true;
            context.add("unsupported_field", &format!("{path}.{field}"), format!("Command hook field {field} is not executable in builtin hooks v1."), Some(Severity::Warning));
        }
    }
    if handler.get("async").and_then(Value::as_bool) == Some(true) {
        unsupported = true;
        context.add("unsupported_async_handler", &format!("{path}.async"), "Async hook handlers are not executable in builtin hooks v1.".to_owned(), Some(Severity::Warning));
    }
    if handler.contains_key("args") || handler.get("command").is_some_and(Value::is_object) {
        unsupported = true;
        context.add("unsupported_command_variant", &format!("{path}.command"), "Exec-form command hooks are not executable in builtin hooks v1.".to_owned(), Some(Severity::Warning));
    }
    unsupported
}
