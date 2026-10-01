use serde_json::Value;
use crate::diagnostics::{DiagnosticDraft, diagnostic};
use crate::handler::{HandlerParseContext, parse_handler};
use crate::types::{HookSourceMetadata, ParsedHookConfig, Severity, SupportedHookEvent, UNSUPPORTED_KNOWN_HOOK_EVENTS};

pub fn parse_hook_config(input: &Value, source: &HookSourceMetadata) -> ParsedHookConfig {
    let mut parsed = ParsedHookConfig::default();
    if !input.is_object() {
        add(&mut parsed, source, DiagnosticDraft { code: "invalid_root", message: "Hook config must be an object.".to_owned(), path: "$".to_owned(), event: None, severity: None });
        return parsed;
    }
    let Some(hooks) = input.get("hooks").and_then(Value::as_object) else {
        add(&mut parsed, source, DiagnosticDraft { code: "invalid_hooks", message: "Hook config must contain an object hooks field.".to_owned(), path: "hooks".to_owned(), event: None, severity: None });
        return parsed;
    };
    for (event_name, groups) in hooks {
        let Ok(event) = serde_json::from_value::<SupportedHookEvent>(Value::String(event_name.clone())) else {
            add(&mut parsed, source, DiagnosticDraft {
                code: if UNSUPPORTED_KNOWN_HOOK_EVENTS.contains(&event_name.as_str()) { "unsupported_event" } else { "unknown_event" },
                message: format!("Hook event {event_name} is not executable in builtin hooks v1."),
                path: format!("hooks.{event_name}"), event: Some(event_name), severity: Some(Severity::Warning),
            });
            continue;
        };
        parse_supported_event(event, groups, source, &mut parsed);
    }
    parsed
}

fn add(parsed: &mut ParsedHookConfig, source: &HookSourceMetadata, draft: DiagnosticDraft<'_>) {
    parsed.diagnostics.push(diagnostic(draft, source));
}

fn parse_supported_event(event: SupportedHookEvent, groups: &Value, source: &HookSourceMetadata, parsed: &mut ParsedHookConfig) {
    let Some(groups) = groups.as_array() else {
        add(parsed, source, DiagnosticDraft { code: "invalid_event_config", message: "Hook event entries must be an array.".to_owned(), path: format!("hooks.{}", event.as_str()), event: Some(event.as_str()), severity: None });
        return;
    };
    for (group_index, group) in groups.iter().enumerate() {
        let path = format!("hooks.{}[{group_index}]", event.as_str());
        let Some(group) = group.as_object() else {
            add(parsed, source, DiagnosticDraft { code: "invalid_handler_group", message: "Hook matcher group must be an object.".to_owned(), path, event: Some(event.as_str()), severity: None });
            continue;
        };
        let matcher = match group.get("matcher") {
            None => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => {
                add(parsed, source, DiagnosticDraft { code: "invalid_matcher", message: "Hook matcher must be a string.".to_owned(), path: format!("{path}.matcher"), event: Some(event.as_str()), severity: None });
                None
            }
        };
        let Some(hooks) = group.get("hooks").and_then(Value::as_array) else {
            add(parsed, source, DiagnosticDraft { code: "invalid_handler_list", message: "Hook matcher group hooks must be an array.".to_owned(), path: format!("{path}.hooks"), event: Some(event.as_str()), severity: None });
            continue;
        };
        for (handler_index, handler) in hooks.iter().enumerate() {
            let mut context = HandlerParseContext { event, matcher: matcher.clone(), group_index, handler_index, source, diagnostics: &mut parsed.diagnostics };
            if let Some(handler) = parse_handler(handler, &mut context) { parsed.executable_handlers.push(handler); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{HookDiscoveryTiming, HookSourceScope};
    use serde_json::{Map, json};

    fn source() -> HookSourceMetadata {
        HookSourceMetadata { scope: HookSourceScope::Project, source_path: "/repo/.senpi/hooks.json".to_owned(), display_order: 7, discovered_at: HookDiscoveryTiming::PreSession, plugin_root: None, manifest_path: None }
    }

    #[test]
    fn supported_envelopes_preserve_event_order() {
        let names = ["PreToolUse","PostToolUse","UserPromptSubmit","SessionStart","PreCompact","PostCompact","Stop","Notification"];
        let hooks: Map<String, Value> = names.iter().map(|event| ((*event).to_owned(), json!([{"matcher":"Bash|Edit","hooks":[{"type":"command","command":format!("node hooks/{event}.mjs"),"timeout":30,"statusMessage":format!("Running {event}")}]}]))).collect();
        let result = parse_hook_config(&json!({"hooks":hooks}), &source());
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.executable_handlers.iter().map(|h|h.event.as_str()).collect::<Vec<_>>(),names);
        for handler in result.executable_handlers {
            assert_eq!(handler.matcher.as_deref(),Some("Bash|Edit"));
            assert_eq!(handler.config.kind,"command");
            assert_eq!(handler.config.command,format!("node hooks/{}.mjs",handler.event.as_str()));
            assert_eq!(handler.config.timeout,Some(30.0));
            assert_eq!(handler.config.status_message,Some(format!("Running {}",handler.event.as_str())));
            assert_eq!(handler.source,source());
        }
    }

    #[test]
    fn windows_command_aliases() {
        let result = parse_hook_config(&json!({"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"posix","commandWindows":"camel"},{"type":"command","command":"posix-two","command_windows":"snake"}]}]}}), &source());
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.executable_handlers.iter().map(|h|h.config.command_windows.as_deref()).collect::<Vec<_>>(),[Some("camel"),Some("snake")]);
    }

    #[test]
    fn unsupported_shapes_are_diagnostic_only() {
        let mut hooks: Map<String, Value> = UNSUPPORTED_KNOWN_HOOK_EVENTS.iter().map(|name|((*name).to_owned(),json!([{"hooks":[{"type":"command","command":"node hook"}]}]))).collect();
        hooks.insert("FutureEvent".to_owned(),json!([]));
        let mut handlers = Vec::new();
        for field in ["if","shell","asyncRewake","terminalSequence","continueOnBlock"] {
            let mut handler = json!({"type":"command","command":"node hook"});
            handler[field] = json!(true); handlers.push(handler);
        }
        handlers.extend([json!({"type":"command","command":{"program":"node"},"args":[]}),json!({"type":"command","command":"node hook","async":true})]);
        for kind in ["prompt","agent","http","mcp_tool"] { handlers.push(json!({"type":kind})); }
        hooks.insert("PreToolUse".to_owned(),json!([{"matcher":"*","hooks":handlers}]));
        let result = parse_hook_config(&json!({"hooks":hooks}),&source());
        assert!(result.executable_handlers.is_empty());
        for code in ["unsupported_event","unknown_event","unsupported_field","unsupported_command_variant","unsupported_handler_type","unsupported_async_handler"] { assert!(result.diagnostics.iter().any(|d|d.code==code)); }
        for event in UNSUPPORTED_KNOWN_HOOK_EVENTS.iter().copied().chain(["FutureEvent"]) { assert!(result.diagnostics.iter().any(|d|d.path==format!("hooks.{event}"))); }
    }

    #[test]
    fn malformed_command_is_not_executable() {
        let result = parse_hook_config(&json!({"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":123}]}]}}),&source());
        assert!(result.executable_handlers.is_empty());
        assert_eq!(result.diagnostics[0].code,"invalid_command");
        assert_eq!(result.diagnostics[0].path,"hooks.PreToolUse[0].hooks[0].command");
    }

    #[test]
    fn invalid_timeouts_are_not_executable() {
        // JSON represents non-finite JS numbers as null; both are rejected at the boundary.
        let timeouts = [json!(0),json!(-1),Value::Null,Value::Null,Value::Null];
        let handlers: Vec<_> = timeouts.into_iter().map(|timeout|json!({"type":"command","command":"node hook","timeout":timeout})).collect();
        let result = parse_hook_config(&json!({"hooks":{"PreToolUse":[{"hooks":handlers}]}}),&source());
        assert!(result.executable_handlers.is_empty()); assert_eq!(result.diagnostics.len(),5);
        for (index, diagnostic) in result.diagnostics.iter().enumerate() { assert_eq!(diagnostic.code,"invalid_timeout"); assert_eq!(diagnostic.path,format!("hooks.PreToolUse[0].hooks[{index}].timeout")); }
    }
}
