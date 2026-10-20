use maho_ai::types::{ContentBlock, Message, StopReason, ToolResultMessage};
use std::collections::HashSet;
pub const TOOL_RESULT_PLACEHOLDER: &str = "Tool output unavailable (context compacted)";
pub fn repair_orphaned_tool_results(messages: &[Message], now: i64) -> Vec<Message> {
    let mut calls = HashSet::new();
    let mut results = HashSet::new();
    for message in messages {
        match message {
            Message::Assistant(assistant) => {
                for block in &assistant.content {
                    if let ContentBlock::ToolCall(call) = block {
                        calls.insert(call.id.clone());
                    }
                }
            }
            Message::ToolResult(result) => {
                results.insert(result.tool_call_id.clone());
            }
            Message::User(_) | Message::System(_) | Message::ConfigurationUpdate(_) => {}
        }
    }
    let mut dangling: HashSet<_> = calls.difference(&results).cloned().collect();
    let mut output = Vec::new();
    for message in messages {
        if let Message::ToolResult(result) = message
            && !calls.contains(&result.tool_call_id)
        {
            let mut result = result.clone();
            result.content = vec![ContentBlock::text(TOOL_RESULT_PLACEHOLDER)];
            output.push(Message::ToolResult(result));
            continue;
        }
        output.push(message.clone());
        if let Message::Assistant(assistant) = message {
            if matches!(
                assistant.stop_reason,
                StopReason::Error | StopReason::Aborted
            ) {
                continue;
            }
            for block in &assistant.content {
                let ContentBlock::ToolCall(call) = block else {
                    continue;
                };
                if !dangling.remove(&call.id) {
                    continue;
                }
                let incomplete = call.incomplete == Some(true);
                let text = if incomplete {
                    match &call.error_message {
                        Some(error) => format!(
                            "{error}{} Re-issue the tool call with complete arguments.",
                            if error.ends_with('.') { "" } else { "." }
                        ),
                        None => format!(
                            "Tool call \"{}\" was not executed: the response ended before the tool call was complete. Re-issue the tool call with complete arguments.",
                            call.name
                        ),
                    }
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
                    nested_calls: None,
                    is_error: incomplete,
                    timestamp: if assistant.timestamp != 0 {
                        assistant.timestamp.saturating_add(1)
                    } else {
                        now
                    },
                }));
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn orphan_repair_retains_nested_calls_and_tool_error_metadata() {
        // Given an orphan carrying producer-owned nested-call records.
        let message: Message = serde_json::from_value(json!({
            "role":"toolResult", "toolCallId":"orphan", "toolName":"eval",
            "content":[{"type":"text","text":"original"}], "isError":true,
            "details":{"code":"C133"}, "timestamp":3,
            "nestedCalls":{"calls":[{"id":"child","name":"read","arguments":{"path":"a"},
                "argumentsBytes":12,"status":"error","durationMs":4,"error":"failed"}],"complete":false}
        })).expect("tool result fixture");
        let Message::ToolResult(original) = &message else { panic!("tool result") };

        // When only the orphan's visible content is replaced.
        let repaired = repair_orphaned_tool_results(&[message.clone()], 9);

        // Then nested records and C133 metadata survive without fabrication.
        let Message::ToolResult(result) = &repaired[0] else { panic!("tool result") };
        assert!(original.nested_calls.is_some());
        assert_eq!(result.nested_calls, original.nested_calls);
        assert_eq!(result.details, original.details);
        assert_eq!(result.is_error, original.is_error);
        assert_eq!(result.content, vec![ContentBlock::text(TOOL_RESULT_PLACEHOLDER)]);
    }

    #[test]
    fn synthesized_result_does_not_invent_nested_calls() {
        // Given an incomplete tool call whose execution never started.
        let message: Message = serde_json::from_value(json!({
            "role":"assistant","content":[{"type":"toolCall","id":"call","name":"eval",
                "arguments":{},"incomplete":true,"errorMessage":"C133"}],
            "api":"faux","provider":"faux","model":"faux",
            "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,
                "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
            "stopReason":"toolUse","timestamp":2
        })).expect("assistant fixture");

        // When pair repair synthesizes the missing result.
        let repaired = repair_orphaned_tool_results(&[message], 9);

        // Then it remains an error with no invented execution history.
        let Message::ToolResult(result) = &repaired[1] else { panic!("tool result") };
        assert!(result.is_error);
        assert!(result.nested_calls.is_none());
        assert_eq!(result.tool_call_id, "call");
    }
}
