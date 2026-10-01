//! Port of senpi packages/ai/src/utils/tool-pair-repair.ts.

use crate::types::{ContentBlock, Message, StopReason, ToolResultMessage};
use crate::utils::diagnostics::now_ms;
use std::collections::HashSet;

pub const TOOL_RESULT_PLACEHOLDER: &str = "Tool output unavailable (context compacted)";

fn incomplete_tool_call_retry_text(name: &str, error_message: Option<&str>) -> String {
    match error_message {
        Some(error) => format!("{error}{} Re-issue the tool call with complete arguments.", if error.ends_with('.') { "" } else { "." }),
        None => format!(
            "Tool call \"{name}\" was not executed: the response ended before the tool call was complete. Re-issue the tool call with complete arguments."
        ),
    }
}

/// Replaces results without a call by a placeholder and synthesizes results for dangling calls.
pub fn repair_orphaned_tool_results(messages: &[Message]) -> Vec<Message> {
    let mut tool_call_ids: Vec<String> = Vec::new();
    let mut tool_result_ids = HashSet::new();
    for message in messages {
        match message {
            Message::Assistant(assistant) => {
                for block in &assistant.content {
                    if let ContentBlock::ToolCall(call) = block
                        && !tool_call_ids.contains(&call.id)
                    {
                        tool_call_ids.push(call.id.clone());
                    }
                }
            }
            Message::ToolResult(result) => {
                tool_result_ids.insert(result.tool_call_id.clone());
            }
            _ => {}
        }
    }
    let known: HashSet<&String> = tool_call_ids.iter().collect();
    let mut dangling: HashSet<String> = tool_call_ids.iter().filter(|id| !tool_result_ids.contains(*id)).cloned().collect();
    let mut output = Vec::with_capacity(messages.len());
    for message in messages {
        if let Message::ToolResult(result) = message {
            if known.contains(&result.tool_call_id) {
                output.push(message.clone());
            } else {
                let mut replaced = result.clone();
                replaced.content = vec![ContentBlock::text(TOOL_RESULT_PLACEHOLDER)];
                output.push(Message::ToolResult(replaced));
            }
            continue;
        }
        output.push(message.clone());
        let Message::Assistant(assistant) = message else { continue };
        if matches!(assistant.stop_reason, StopReason::Error | StopReason::Aborted) {
            continue;
        }
        for block in &assistant.content {
            let ContentBlock::ToolCall(call) = block else { continue };
            if !dangling.contains(&call.id) {
                continue;
            }
            let incomplete = call.incomplete == Some(true);
            let text = if incomplete {
                incomplete_tool_call_retry_text(&call.name, call.error_message.as_deref())
            } else {
                TOOL_RESULT_PLACEHOLDER.to_owned()
            };
            output.push(Message::ToolResult(ToolResultMessage {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                content: vec![ContentBlock::text(text)],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: incomplete,
                timestamp: if assistant.timestamp != 0 { assistant.timestamp + 1 } else { now_ms() },
            }));
            dangling.remove(&call.id);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AssistantMessage, TextContent, ToolCall, UserContent, UserMessage};

    fn user(text: &str, timestamp: i64) -> Message {
        Message::User(UserMessage { content: UserContent::Text(text.into()), timestamp })
    }

    fn assistant_with_call(id: &str, name: &str, timestamp: i64) -> Message {
        assistant_impl(&[(id, name, false, None)], StopReason::ToolUse, timestamp)
    }

    fn assistant_with_flagged_call(id: &str, name: &str, timestamp: i64, error_message: Option<&str>) -> Message {
        assistant_impl(&[(id, name, true, error_message)], StopReason::ToolUse, timestamp)
    }

    fn assistant_impl(calls: &[(&str, &str, bool, Option<&str>)], stop: StopReason, timestamp: i64) -> Message {
        let mut message: AssistantMessage = serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": [], "api": "anthropic-messages", "provider": "anthropic", "model": "claude-sonnet-4-5",
            "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "stopReason": "toolUse", "timestamp": 10
        }))
        .expect("assistant");
        message.stop_reason = stop;
        message.timestamp = timestamp;
        message.content = calls
            .iter()
            .map(|(id, name, incomplete, error_message)| {
                ContentBlock::ToolCall(ToolCall {
                    id: (*id).into(),
                    name: (*name).into(),
                    incomplete: incomplete.then_some(true),
                    error_message: error_message.map(|s| s.to_owned()),
                    ..ToolCall::default()
                })
            })
            .collect();
        Message::Assistant(Box::new(message))
    }

    fn tool_result(id: &str, name: &str, timestamp: i64, text: &str) -> Message {
        Message::ToolResult(ToolResultMessage {
            tool_call_id: id.into(),
            tool_name: name.into(),
            content: vec![ContentBlock::text(text)],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp,
        })
    }

    fn assistant(calls: &[(&str, bool)], stop: StopReason) -> Message {
        let mapped: Vec<(&str, &str, bool, Option<&str>)> = calls.iter().map(|(id, incomplete)| (*id, "read", *incomplete, None)).collect();
        assistant_impl(&mapped, stop, 10)
    }

    fn result(id: &str) -> Message {
        tool_result(id, "read", 1, "ok")
    }

    fn text_of(message: &Message) -> (String, bool, i64) {
        let Message::ToolResult(r) = message else { panic!("not a tool result") };
        let ContentBlock::Text(t) = &r.content[0] else { panic!("not text") };
        (t.text.clone(), r.is_error, r.timestamp)
    }

    fn result_text(message: &Message) -> String {
        let Message::ToolResult(r) = message else { panic!("not a tool result") };
        r.content.iter().find_map(|b| match b { ContentBlock::Text(TextContent { text, .. }) => Some(text.clone()), _ => None }).unwrap_or_default()
    }

    #[test]
    fn synthesizes_dangling_results_and_replaces_orphans() {
        let repaired = repair_orphaned_tool_results(&[assistant(&[("a", false), ("b", true)], StopReason::ToolUse), result("x")]);
        assert_eq!(repaired.len(), 4);
        assert_eq!(text_of(&repaired[1]), (TOOL_RESULT_PLACEHOLDER.into(), false, 11));
        assert_eq!(
            text_of(&repaired[2]).0,
            "Tool call \"read\" was not executed: the response ended before the tool call was complete. Re-issue the tool call with complete arguments."
        );
        assert!(text_of(&repaired[2]).1);
        assert_eq!(text_of(&repaired[3]).0, TOOL_RESULT_PLACEHOLDER);
    }

    #[test]
    fn leaves_failed_turns_and_paired_calls_alone() {
        let repaired = repair_orphaned_tool_results(&[assistant(&[("a", false)], StopReason::Error)]);
        assert_eq!(repaired.len(), 1);
        let paired = repair_orphaned_tool_results(&[assistant(&[("a", false)], StopReason::ToolUse), result("a")]);
        assert_eq!(text_of(&paired[1]).0, "ok");
        assert_eq!(incomplete_tool_call_retry_text("x", Some("Bad args.")), "Bad args. Re-issue the tool call with complete arguments.");
    }

    // tool-pair-repair.test.ts: "returns identical structure for valid tool pairs"
    #[test]
    fn returns_identical_structure_for_valid_tool_pairs() {
        let messages = vec![user("list files", 1), assistant_with_call("call-1", "ls", 2), tool_result("call-1", "ls", 3, "done")];
        let repaired = repair_orphaned_tool_results(&messages);
        assert_eq!(repaired, messages);
    }

    // tool-pair-repair.test.ts: "replaces orphan tool result content with placeholder"
    #[test]
    fn replaces_orphan_tool_result_content_with_placeholder() {
        let messages = vec![user("run", 1), tool_result("missing", "ls", 2, "real output")];
        let repaired = repair_orphaned_tool_results(&messages);
        assert_eq!(repaired.len(), 2);
        let Message::ToolResult(r) = &repaired[1] else { panic!("tool result") };
        assert_eq!(r.tool_call_id, "missing");
        assert_eq!(result_text(&repaired[1]), TOOL_RESULT_PLACEHOLDER);
    }

    // tool-pair-repair.test.ts: "inserts synthetic tool result for dangling assistant tool call"
    #[test]
    fn inserts_synthetic_tool_result_for_dangling_assistant_tool_call() {
        let messages = vec![user("run", 1), assistant_with_call("call-2", "pwd", 2)];
        let repaired = repair_orphaned_tool_results(&messages);
        assert_eq!(repaired.len(), 3);
        let Message::ToolResult(r) = &repaired[2] else { panic!("tool result") };
        assert_eq!(r.tool_call_id, "call-2");
        assert_eq!(r.tool_name, "pwd");
        assert_eq!(result_text(&repaired[2]), TOOL_RESULT_PLACEHOLDER);
        assert!(!r.is_error);
    }

    // tool-pair-repair.test.ts: "handles mixed orphan tool results and dangling tool calls"
    #[test]
    fn handles_mixed_orphan_tool_results_and_dangling_tool_calls() {
        let messages = vec![user("run", 1), assistant_with_call("call-3", "ls", 2), tool_result("orphan", "cat", 3, "old output")];
        let repaired = repair_orphaned_tool_results(&messages);
        assert_eq!(repaired.len(), 4);
        let has_orphan_placeholder = repaired.iter().any(|m| {
            matches!(m, Message::ToolResult(r) if r.tool_call_id == "orphan" && result_text(m) == TOOL_RESULT_PLACEHOLDER)
        });
        let has_call3_placeholder = repaired.iter().any(|m| {
            matches!(m, Message::ToolResult(r) if r.tool_call_id == "call-3" && r.tool_name == "ls" && result_text(m) == TOOL_RESULT_PLACEHOLDER)
        });
        assert!(has_orphan_placeholder);
        assert!(has_call3_placeholder);
    }

    // tool-pair-repair.test.ts: "synthesizes an isError:true retry-diagnostic result for a flagged dangling tool call (case a)"
    #[test]
    fn synthesizes_an_is_error_true_retry_diagnostic_result_for_a_flagged_dangling_tool_call_case_a() {
        let messages = vec![user("run", 1), assistant_with_flagged_call("call-flag", "bash", 2, None)];
        let repaired = repair_orphaned_tool_results(&messages);
        assert_eq!(repaired.len(), 3);
        let Message::ToolResult(synth) = &repaired[2] else { panic!("tool result") };
        assert_eq!(synth.tool_call_id, "call-flag");
        assert_eq!(synth.tool_name, "bash");
        assert!(synth.is_error);
        assert_eq!(result_text(&repaired[2]), incomplete_tool_call_retry_text("bash", None));
        let error_results: Vec<_> = repaired.iter().filter(|m| matches!(m, Message::ToolResult(r) if r.is_error)).collect();
        assert_eq!(error_results.len(), 1);
    }

    // tool-pair-repair.test.ts: "is idempotent: a second repair pass deep-equals the first pass (case b)"
    #[test]
    fn is_idempotent_a_second_repair_pass_deep_equals_the_first_pass_case_b() {
        let messages = vec![user("run", 1), assistant_with_flagged_call("call-flag", "bash", 2, None)];
        let once = repair_orphaned_tool_results(&messages);
        let twice = repair_orphaned_tool_results(&once);
        assert_eq!(twice, once);
    }

    // tool-pair-repair.test.ts: "keeps legacy non-flagged dangling calls as isError:false with the placeholder (case c)"
    #[test]
    fn keeps_legacy_non_flagged_dangling_calls_as_is_error_false_with_the_placeholder_case_c() {
        let messages = vec![user("run", 1), assistant_with_call("call-legacy", "pwd", 2)];
        let repaired = repair_orphaned_tool_results(&messages);
        assert_eq!(repaired.len(), 3);
        let Message::ToolResult(r) = &repaired[2] else { panic!("tool result") };
        assert_eq!(r.tool_call_id, "call-legacy");
        assert_eq!(r.tool_name, "pwd");
        assert_eq!(result_text(&repaired[2]), TOOL_RESULT_PLACEHOLDER);
        assert!(!r.is_error);
    }

    // tool-pair-repair.test.ts: "leaves a flagged tool call untouched when a real toolResult already exists (case d)"
    #[test]
    fn leaves_a_flagged_tool_call_untouched_when_a_real_tool_result_already_exists_case_d() {
        let messages = vec![
            user("run", 1),
            assistant_with_flagged_call("call-flag-real", "bash", 2, None),
            tool_result("call-flag-real", "bash", 3, "real output"),
        ];
        let repaired = repair_orphaned_tool_results(&messages);
        assert_eq!(repaired, messages);
        let Message::ToolResult(real_result) = &repaired[2] else { panic!("tool result") };
        assert!(!real_result.is_error);
        assert_eq!(result_text(&repaired[2]), "real output");
    }

    // tool-pair-repair.test.ts: "appends the retry instruction to the tool call's errorMessage without a duplicate period"
    #[test]
    fn appends_the_retry_instruction_to_the_tool_calls_error_message_without_a_duplicate_period() {
        let messages = vec![user("run", 1), assistant_with_flagged_call("call-err", "bash", 2, Some("custom truncation reason."))];
        let repaired = repair_orphaned_tool_results(&messages);
        assert_eq!(repaired.len(), 3);
        let Message::ToolResult(synth) = &repaired[2] else { panic!("tool result") };
        assert!(synth.is_error);
        assert_eq!(result_text(&repaired[2]), "custom truncation reason. Re-issue the tool call with complete arguments.");
    }

    // tool-pair-repair.test.ts: "does not synthesize a result for a dangling call of an errored assistant"
    #[test]
    fn does_not_synthesize_a_result_for_a_dangling_call_of_an_errored_assistant() {
        let mapped: Vec<(&str, &str, bool, Option<&str>)> = vec![("call-dead", "edit", false, None)];
        let messages = vec![user("run", 1), assistant_impl(&mapped, StopReason::Error, 2)];
        let repaired = repair_orphaned_tool_results(&messages);
        assert_eq!(repaired, messages);
    }

    // tool-pair-repair.test.ts: "does not synthesize a result for a dangling call of an aborted assistant"
    #[test]
    fn does_not_synthesize_a_result_for_a_dangling_call_of_an_aborted_assistant() {
        let mapped: Vec<(&str, &str, bool, Option<&str>)> = vec![("call-dead", "edit", false, None)];
        let messages = vec![user("run", 1), assistant_impl(&mapped, StopReason::Aborted, 2)];
        let repaired = repair_orphaned_tool_results(&messages);
        assert_eq!(repaired, messages);
    }

    // tool-pair-repair.test.ts: "still synthesizes for a kept assistant re-declaring an id an errored assistant used"
    #[test]
    fn still_synthesizes_for_a_kept_assistant_re_declaring_an_id_an_errored_assistant_used() {
        let mapped: Vec<(&str, &str, bool, Option<&str>)> = vec![("call-reused", "edit", false, None)];
        let messages = vec![
            user("run", 1),
            assistant_impl(&mapped, StopReason::Error, 2),
            assistant_with_call("call-reused", "edit", 3),
        ];
        let repaired = repair_orphaned_tool_results(&messages);
        assert_eq!(repaired.len(), 4);
        let Message::ToolResult(r) = &repaired[3] else { panic!("tool result") };
        assert_eq!(r.tool_call_id, "call-reused");
        assert_eq!(r.tool_name, "edit");
        assert_eq!(result_text(&repaired[3]), TOOL_RESULT_PLACEHOLDER);
        assert!(!r.is_error);
    }
}
