use std::sync::Arc;

use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ToolCallEvent};
use serde_json::{Value, json};

use crate::gate_rule::match_gate;

const QUESTION_TOOLS: [&str; 2] = ["ask_user_question", "request_user_input"];

fn as_tool_call_event(payload: &mut ExtensionEvent) -> Option<&mut ToolCallEvent> {
    let ExtensionEvent::ToolCall(event) = payload else { return None };
    event.input.is_object().then_some(event)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct QuestionGatesComponent;

pub const fn create_question_gates_component() -> QuestionGatesComponent {
    QuestionGatesComponent
}

impl Extension for QuestionGatesComponent {
    fn register(&self, api: &mut ExtensionApi) {
        // Upstream logs on the optional `ctx.logger.debug?.` channel; the native `ComponentLogger`
        // has no debug channel, and hosts without one drop the line, so the sink here discards it.
        api.on(EventKind::ToolCall, Arc::new(|payload, _ctx| {
            Box::pin(async move {
                force_required(payload, &mut |_, _| {});
                Ok(EventResult::None)
            })
        }));
    }
}

/// `debug` is the optional `ctx.logger.debug?.` channel: called once per forced gate.
fn force_required(payload: &mut ExtensionEvent, debug: &mut dyn FnMut(&str, &Value)) {
    let Some(event) = as_tool_call_event(payload) else { return };
    if !QUESTION_TOOLS.contains(&event.tool_name.as_str()) || event.input.get("required") == Some(&Value::Bool(true)) {
        return;
    }
    let Some(gate) = match_gate(&event.input) else { return };
    let mut details = json!({"tool": event.tool_name, "rule": gate.rule.as_str()});
    if let Some(header) = gate.header {
        details["header"] = Value::String(header.to_owned());
    }
    event.input["required"] = Value::Bool(true);
    debug("omo-senpi question-gates forced required", &details);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate_rule::PINNED_GATE_HEADERS;
    use maho_ext_ask_user::schema::{AskUserVariant, to_canonical};

    #[derive(Default)]
    struct DebugLog(Vec<(String, Value)>);

    impl DebugLog {
        fn record(&mut self, message: &str, details: &Value) {
            self.0.push((message.to_owned(), details.clone()));
        }
    }

    fn fork(header: &str, question: &str, labels: &[&str]) -> Value {
        json!({"header": header, "multiSelect": false, "question": question,
            "options": labels.iter().map(|label| json!({"label": label, "description": label})).collect::<Vec<_>>()})
    }

    fn recorded_glm_gate_input() -> Value {
        json!({"questions": [fork("Approval",
            "Approve this plan approach (todo.json in cwd, load-on-start / write-on-add)?",
            &["Approve", "Approve, skip review", "Change approach"])], "waitForAnswer": true})
    }

    fn dispatch(tool: &str, input: Value, log: &mut DebugLog) -> Value {
        let mut payload = ExtensionEvent::ToolCall(ToolCallEvent {
            tool_call_id: "call-1".into(), tool_name: tool.into(), input,
        });
        force_required(&mut payload, &mut |message, details| log.record(message, details));
        let ExtensionEvent::ToolCall(event) = payload else { panic!("tool call preserved") };
        event.input
    }

    #[test]
    fn recorded_gate_forces_required_when_omitted_or_false() {
        for required in [None, Some(false)] {
            let mut input = recorded_glm_gate_input();
            if let Some(required) = required { input["required"] = json!(required); }
            let result = dispatch("ask_user_question", input, &mut DebugLog::default());
            assert_eq!(result["required"], true);
        }
    }

    #[test]
    fn exact_headers_force_required_and_log_header_rule() {
        for header in ["Approval", "Authorize", "authorise", "approval", "승인", "권한"] {
            let mut log = DebugLog::default();
            let input = json!({"questions": [fork(header, "Proceed with the irreversible step?", &["Proceed", "Hold"])], "waitForAnswer": true});
            let result = dispatch("ask_user_question", input, &mut log);
            assert_eq!(result["required"], true);
            assert_eq!(log.0.len(), 1);
            assert_eq!(log.0[0].0, "omo-senpi question-gates forced required");
            assert_eq!(log.0[0].1, json!({"tool": "ask_user_question", "header": header, "rule": "header"}));
        }
    }

    #[test]
    fn codex_label_gate_forces_required_and_logs_label_rule() {
        let mut log = DebugLog::default();
        let input = json!({"questions": [fork("Next step", "Ready to write the plan?", &["Approve plan", "Change approach"])], "wait_for_answer": true});
        let result = dispatch("request_user_input", input, &mut log);
        assert_eq!(result["required"], true);
        assert_eq!(log.0[0].1,
            json!({"tool": "request_user_input", "header": "Next step", "rule": "label"}));
    }

    #[test]
    fn question_prose_and_partial_headers_do_not_force_required() {
        for header in ["Review flow", "Approvals log", "Pre-approval", "Approval UX", "권한 모델", "승인자", "Storage"] {
            let mut log = DebugLog::default();
            let input = json!({"questions": [fork(header,
                "Should the approve step require two reviewers, and who may approve hotfixes?",
                &["Two reviewers", "One reviewer", "Pre-approved list"])], "waitForAnswer": true});
            let result = dispatch("ask_user_question", input.clone(), &mut log);
            assert_eq!(result, input);
            assert!(log.0.is_empty());
        }
    }

    #[test]
    fn already_required_gate_and_unrelated_tool_are_unchanged() {
        let mut required = recorded_glm_gate_input();
        required["required"] = json!(true);
        for (tool, input) in [("ask_user_question", required), ("todo", recorded_glm_gate_input())] {
            let mut log = DebugLog::default();
            let result = dispatch(tool, input.clone(), &mut log);
            assert_eq!(result, input);
            assert!(log.0.is_empty());
        }
    }

    #[test]
    fn forced_gate_debug_details_exclude_question_and_options() {
        let mut log = DebugLog::default();
        dispatch("ask_user_question", recorded_glm_gate_input(), &mut log);
        let entries = &log.0;
        assert_eq!(entries.len(), 1);
        let serialized = serde_json::to_string(entries).expect("debug log serializes");
        assert!(!serialized.contains("todo.json") && !serialized.contains("plan-reviewer"));
        assert_eq!(entries[0].1, json!({"tool": "ask_user_question", "header": "Approval", "rule": "header"}));
    }

    #[test]
    fn missing_header_is_omitted_from_debug_details() {
        let mut log = DebugLog::default();
        dispatch("ask_user_question", json!({"questions": [{"options": [{"label": "Approve"}]}]}), &mut log);
        assert_eq!(log.0[0].1,
            json!({"tool": "ask_user_question", "rule": "label"}));
    }

    #[test]
    fn malformed_payloads_are_not_modified() {
        for input in [Value::Null, json!([]), json!(false)] {
            let mut log = DebugLog::default();
            let result = dispatch("ask_user_question", input.clone(), &mut log);
            assert_eq!(result, input);
            assert!(log.0.is_empty());
        }
        let mut event = ExtensionEvent::AgentStart;
        force_required(&mut event, &mut |_, _| panic!("no debug line for a non tool_call event"));
        assert!(matches!(event, ExtensionEvent::AgentStart));
    }

    #[test]
    fn pinned_headers_pass_native_question_schema_but_long_header_fails() {
        for header in PINNED_GATE_HEADERS {
            let input = json!({"questions": [fork(header, "Proceed?", &["Approve", "Change approach"])], "waitForAnswer": true});
            let result = to_canonical(AskUserVariant::Claude, &input, "call-1".into(), None);
            assert!(result.is_ok(), "{header}");
        }
        let input = json!({"questions": [fork("Authorization", "Proceed?", &["Approve", "Change approach"])], "waitForAnswer": true});
        let result = to_canonical(AskUserVariant::Claude, &input, "call-1".into(), None);
        assert!(result.is_err());
    }
}
