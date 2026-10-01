use serde_json::{Map, Value};
use crate::diagnostics::{DiagnosticDraft, diagnostic};
use crate::types::{HookDiagnostic, HookSourceMetadata, Severity, SupportedHookEvent};

pub struct HookOutputParseInput<'a> {
    pub event: SupportedHookEvent,
    pub exit_code: i32,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub source: &'a HookSourceMetadata,
}

#[derive(Debug)]
pub struct ParsedHookOutput {
    pub output: Map<String, Value>,
    pub diagnostics: Vec<HookDiagnostic>,
}

struct ParseState<'a> {
    input: HookOutputParseInput<'a>,
    parsed: ParsedHookOutput,
}

impl ParseState<'_> {
    fn add(&mut self, code: &str, path: &str, message: String, severity: Option<Severity>) {
        self.parsed.diagnostics.push(diagnostic(DiagnosticDraft {
            code, path: path.to_owned(), message,
            event: Some(self.input.event.as_str()), severity,
        }, self.input.source));
    }

    fn copy_text(&mut self, value: Option<&Value>, field: &str) {
        if let Some(value) = value.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
            self.parsed.output.insert(field.to_owned(), Value::String(value.to_owned()));
        }
    }

    fn warning(&mut self, path: &str, message: String) {
        self.add("unsupported_field", path, message, Some(Severity::Warning));
    }

    fn decision(&mut self, value: &str) {
        self.parsed.output.insert("decision".to_owned(), Value::String(value.to_owned()));
    }
}

pub fn parse_hook_output(input: HookOutputParseInput<'_>) -> ParsedHookOutput {
    let mut state = ParseState { input, parsed: ParsedHookOutput { output: Map::new(), diagnostics: Vec::new() } };
    if state.input.exit_code == 2 {
        state.decision("block");
        let stderr = state.input.stderr;
        state.copy_text(Some(&Value::String(stderr.to_owned())), "reason");
        return state.parsed;
    }
    let stdout = state.input.stdout.trim();
    if stdout.is_empty() { return state.parsed; }
    let parsed = match serde_json::from_str::<Value>(stdout) {
        Ok(Value::Object(parsed)) => parsed,
        Ok(_) => {
            state.add("invalid_root", "stdout", "Hook stdout JSON must be an object.".to_owned(), None);
            return state.parsed;
        }
        Err(_) => {
            state.add("invalid_root", "stdout", "Hook stdout must be valid JSON.".to_owned(), None);
            return state.parsed;
        }
    };
    parse_universal(&parsed, &mut state);
    let specific = match parsed.get("hookSpecificOutput") {
        None => None,
        Some(Value::Object(value)) => {
            if let Some(name) = value.get("hookEventName")
                && name.as_str() != Some(state.input.event.as_str()) {
                    let name = name.as_str().map(str::to_owned).unwrap_or_else(|| name.to_string());
                    state.add("invalid_event_config", "stdout.hookSpecificOutput.hookEventName",
                        format!("Hook output event {name} does not match {}.", state.input.event.as_str()), None);
                    return state.parsed;
            }
            Some(value)
        }
        Some(_) => {
            state.add("invalid_event_config", "stdout.hookSpecificOutput", "Hook hookSpecificOutput field must be an object.".to_owned(), None);
            return state.parsed;
        }
    };
    parse_event(&parsed, specific, &mut state);
    state.parsed
}

fn parse_universal(parsed: &Map<String, Value>, state: &mut ParseState<'_>) {
    if let Some(value) = parsed.get("continue").and_then(Value::as_bool) {
        state.parsed.output.insert("continue".to_owned(), Value::Bool(value));
        if state.input.event == SupportedHookEvent::Stop && !value { state.decision("block"); }
    }
    state.copy_text(parsed.get("stopReason"), "stopReason");
    if let Some(value) = parsed.get("suppressOutput").and_then(Value::as_bool) {
        state.parsed.output.insert("suppressOutput".to_owned(), Value::Bool(value));
    }
    if parsed.contains_key("systemMessage") {
        match state.input.event {
            SupportedHookEvent::PreToolUse | SupportedHookEvent::PostToolUse | SupportedHookEvent::UserPromptSubmit | SupportedHookEvent::SessionStart | SupportedHookEvent::Stop => state.copy_text(parsed.get("systemMessage"), "systemMessage"),
            SupportedHookEvent::Notification | SupportedHookEvent::PreCompact | SupportedHookEvent::PostCompact => state.warning("stdout.systemMessage", "Hook systemMessage is not supported for this event.".to_owned()),
        }
    }
}

fn coalesce<'a>(specific: Option<&'a Map<String, Value>>, parsed: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    specific.and_then(|s| s.get(key)).filter(|v| !v.is_null()).or_else(|| parsed.get(key))
}

fn parse_event(parsed: &Map<String, Value>, specific: Option<&Map<String, Value>>, state: &mut ParseState<'_>) {
    use SupportedHookEvent as E;
    match state.input.event {
        E::PreToolUse => {
            let decision = specific.and_then(|s| s.get("permissionDecision")).filter(|v| !v.is_null()).or_else(|| parsed.get("decision")).and_then(Value::as_str);
            match decision {
                Some("allow" | "approve" | "ask") => { if let Some(decision) = decision { state.decision(decision); } }
                Some("deny" | "block") => state.decision("deny"),
                _ => {}
            }
            let reason = specific.and_then(|s| s.get("permissionDecisionReason")).filter(|v| !v.is_null()).or_else(|| parsed.get("reason"));
            state.copy_text(reason, "reason");
            state.copy_text(coalesce(specific, parsed, "additionalContext"), "additionalContext");
            if let Some(value) = coalesce(specific, parsed, "updatedInput") {
                if specific.and_then(|s| s.get("permissionDecision")).and_then(Value::as_str) == Some("allow") {
                    state.parsed.output.insert("updatedInput".to_owned(), value.clone());
                } else {
                    state.warning("stdout.hookSpecificOutput.updatedInput", "PreToolUse updatedInput is only applied when permissionDecision is allow.".to_owned());
                }
            }
        }
        E::PostToolUse | E::UserPromptSubmit => {
            if let Some(decision) = parsed.get("decision") {
                if decision.as_str() == Some("block") { state.decision("block"); }
                else { state.warning("stdout.decision", format!("{} only supports decision block.", state.input.event.as_str())); }
            }
            state.copy_text(parsed.get("reason"), "reason");
            state.copy_text(coalesce(specific, parsed, "additionalContext"), "additionalContext");
            if state.input.event == E::PostToolUse {
                if let Some(value) = coalesce(specific, parsed, "updatedToolOutput") { state.parsed.output.insert("updatedToolOutput".to_owned(), value.clone()); }
            } else {
                for field in ["prompt", "updatedPrompt", "replacementPrompt"] {
                    if specific.is_some_and(|s| s.contains_key(field)) { state.warning(&format!("stdout.hookSpecificOutput.{field}"), "UserPromptSubmit prompt replacement is not supported.".to_owned()); }
                }
            }
        }
        E::Stop => {
            if let Some(decision) = parsed.get("decision") {
                if decision.as_str() == Some("block") { state.decision("block"); }
                else if decision.as_str() != Some("continue") { state.warning("stdout.decision", "Stop only supports decision block or continue.".to_owned()); }
            }
            state.copy_text(parsed.get("reason"), "reason");
            state.copy_text(coalesce(specific, parsed, "additionalContext"), "additionalContext");
        }
        E::Notification | E::SessionStart => {
            state.copy_text(coalesce(specific, parsed, "additionalContext"), "additionalContext");
            if parsed.contains_key("decision") { state.warning("stdout.decision", format!("{} does not support decisions.", state.input.event.as_str())); }
        }
        E::PreCompact | E::PostCompact => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{HookDiscoveryTiming, HookSourceScope};
    use serde_json::json;

    fn parse(event: SupportedHookEvent, stdout: &str, exit_code: i32, stderr: &str) -> ParsedHookOutput {
        parse_hook_output(HookOutputParseInput { event, stdout, exit_code, stderr, source: &HookSourceMetadata {
            scope: HookSourceScope::Project, source_path: "/repo/.senpi/hooks.json".to_owned(), display_order: 3,
            discovered_at: HookDiscoveryTiming::Runtime, plugin_root: None, manifest_path: None, plugin_env: None,
        } })
    }

    #[test]
    fn empty_success_has_no_decision() {
        let result = parse(SupportedHookEvent::PreToolUse, "", 0, "");
        assert!(result.output.is_empty()); assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn malformed_json_and_exit_two_block() {
        let result = parse(SupportedHookEvent::PostToolUse, "{not json", 0, "");
        assert!(result.output.is_empty()); assert_eq!(result.diagnostics[0].code, "invalid_root");
        let result = parse(SupportedHookEvent::PostToolUse, "{not json", 2, "policy blocked");
        assert_eq!(Value::Object(result.output), json!({"decision":"block","reason":"policy blocked"})); assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn pre_tool_allow_deny_and_mismatched_event() {
        let input = json!({"continue":true,"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","permissionDecisionReason":"safe","updatedInput":{"command":"printf ok"},"additionalContext":"lint gate passed"}});
        let result = parse(SupportedHookEvent::PreToolUse, &input.to_string(), 0, "");
        assert_eq!(Value::Object(result.output), json!({"continue":true,"decision":"allow","reason":"safe","updatedInput":{"command":"printf ok"},"additionalContext":"lint gate passed"})); assert!(result.diagnostics.is_empty());
        for decision in ["deny", "approve"] {
            let input = json!({"hookSpecificOutput":{"permissionDecision":decision,"updatedInput":{"command":"ignored"}}});
            let result = parse(SupportedHookEvent::PreToolUse, &input.to_string(), 0, "");
            assert_eq!(Value::Object(result.output), json!({"decision":decision})); assert_eq!(result.diagnostics[0].code,"unsupported_field");
        }
        let result = parse(SupportedHookEvent::PreToolUse, r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse"}}"#, 0, "");
        assert!(result.output.is_empty()); assert_eq!(result.diagnostics[0].code,"invalid_event_config");
    }

    #[test]
    fn post_tool_context_block_and_output() {
        let input = json!({"decision":"block","reason":"redacted output","hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"tool result contained secrets","updatedToolOutput":"redacted"}});
        let result = parse(SupportedHookEvent::PostToolUse, &input.to_string(), 0, "");
        assert_eq!(Value::Object(result.output),json!({"decision":"block","reason":"redacted output","additionalContext":"tool result contained secrets","updatedToolOutput":"redacted"})); assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn prompt_block_rejects_replacement() {
        let input = json!({"decision":"block","reason":"missing ticket","hookSpecificOutput":{"additionalContext":"Use ticket SION-123","prompt":"replacement"}});
        let result = parse(SupportedHookEvent::UserPromptSubmit, &input.to_string(), 0, "");
        assert_eq!(Value::Object(result.output),json!({"decision":"block","reason":"missing ticket","additionalContext":"Use ticket SION-123"})); assert_eq!(result.diagnostics[0].path,"stdout.hookSpecificOutput.prompt");
    }

    #[test]
    fn stop_session_context_and_universal_fields() {
        let input = json!({"continue":false,"stopReason":"quality gate failed","suppressOutput":true,"systemMessage":"Tell the user the gate failed","hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"rerun npm run check"}});
        let result = parse(SupportedHookEvent::Stop,&input.to_string(),0,"");
        assert_eq!(Value::Object(result.output),json!({"continue":false,"decision":"block","stopReason":"quality gate failed","suppressOutput":true,"systemMessage":"Tell the user the gate failed","additionalContext":"rerun npm run check"})); assert!(result.diagnostics.is_empty());
        let result = parse(SupportedHookEvent::SessionStart,r#"{"systemMessage":"Project instructions loaded","hookSpecificOutput":{"additionalContext":"Prefer strict hooks."}}"#,0,"");
        assert_eq!(Value::Object(result.output),json!({"systemMessage":"Project instructions loaded","additionalContext":"Prefer strict hooks."}));
        let result = parse(SupportedHookEvent::PreCompact,r#"{"systemMessage":"not representable here"}"#,0,"");
        assert!(result.output.is_empty()); assert_eq!(result.diagnostics[0].code,"unsupported_field");
    }
}
