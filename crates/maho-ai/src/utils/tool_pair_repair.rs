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
    use crate::types::{AssistantMessage, ToolCall};

    fn assistant(calls: &[(&str, bool)], stop: StopReason) -> Message {
        let mut message: AssistantMessage = serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": [], "api": "a", "provider": "p", "model": "m",
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "stopReason": "toolUse", "timestamp": 10
        }))
        .expect("assistant");
        message.stop_reason = stop;
        message.content = calls
            .iter()
            .map(|(id, incomplete)| {
                ContentBlock::ToolCall(ToolCall {
                    id: (*id).into(),
                    name: "read".into(),
                    incomplete: incomplete.then_some(true),
                    ..ToolCall::default()
                })
            })
            .collect();
        Message::Assistant(Box::new(message))
    }

    fn result(id: &str) -> Message {
        Message::ToolResult(ToolResultMessage {
            tool_call_id: id.into(),
            tool_name: "read".into(),
            content: vec![ContentBlock::text("ok")],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 1,
        })
    }

    fn text_of(message: &Message) -> (String, bool, i64) {
        let Message::ToolResult(r) = message else { panic!("not a tool result") };
        let ContentBlock::Text(t) = &r.content[0] else { panic!("not text") };
        (t.text.clone(), r.is_error, r.timestamp)
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
}
