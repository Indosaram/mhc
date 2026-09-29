//! Port of senpi packages/ai/src/utils/drop-failed-assistant-turns.ts.

use crate::types::{ContentBlock, Message, StopReason};
use std::collections::HashSet;

fn is_failed_stop(stop_reason: StopReason) -> bool {
    matches!(stop_reason, StopReason::Error | StopReason::Aborted)
}

/// Drop assistant turns that ended in "error"/"aborted", plus tool results whose id was declared only
/// by dropped assistants. Every other message and the order are preserved.
pub fn drop_failed_assistant_turns(messages: &[Message]) -> Vec<Message> {
    let mut kept_ids = HashSet::new();
    let mut failed_ids = HashSet::new();
    for message in messages {
        if let Message::Assistant(assistant) = message {
            let declared = if is_failed_stop(assistant.stop_reason) { &mut failed_ids } else { &mut kept_ids };
            for block in &assistant.content {
                if let ContentBlock::ToolCall(call) = block {
                    declared.insert(call.id.clone());
                }
            }
        }
    }
    for id in &kept_ids {
        failed_ids.remove(id);
    }
    messages
        .iter()
        .filter(|message| match message {
            Message::Assistant(assistant) => !is_failed_stop(assistant.stop_reason),
            Message::ToolResult(result) => !failed_ids.contains(&result.tool_call_id),
            _ => true,
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn messages(value: serde_json::Value) -> Vec<Message> {
        serde_json::from_value(value).expect("messages")
    }

    fn roles(messages: &[Message]) -> Vec<&'static str> {
        messages.iter().map(Message::role).collect()
    }

    fn assistant(stop: &str, tool_call_id: Option<&str>) -> serde_json::Value {
        let content = match tool_call_id {
            Some(id) => json!([{"type": "toolCall", "id": id, "name": "t", "arguments": {}}]),
            None => json!([{"type": "text", "text": "ok"}]),
        };
        json!({"role": "assistant", "content": content, "api": "a", "provider": "p", "model": "m",
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "stopReason": stop, "timestamp": 1})
    }

    fn tool_result(id: &str) -> serde_json::Value {
        json!({"role": "toolResult", "toolCallId": id, "toolName": "t", "content": [{"type": "text", "text": "r"}], "isError": false, "timestamp": 2})
    }

    #[test]
    fn drops_a_failed_assistant_turn_and_its_orphaned_tool_result() {
        let list = messages(json!([
            {"role": "user", "content": "hi", "timestamp": 0},
            assistant("error", Some("call_1")),
            tool_result("call_1"),
        ]));
        let kept = drop_failed_assistant_turns(&list);
        assert_eq!(roles(&kept), vec!["user"]);
    }

    #[test]
    fn keeps_a_tool_result_whose_call_id_is_redeclared_by_a_kept_assistant() {
        let list = messages(json!([
            assistant("error", Some("call_1")),
            assistant("stop", Some("call_1")),
            tool_result("call_1"),
        ]));
        let kept = drop_failed_assistant_turns(&list);
        assert_eq!(roles(&kept), vec!["assistant", "toolResult"]);
    }

    #[test]
    fn keeps_a_successful_turn_and_its_tool_result() {
        let list = messages(json!([assistant("stop", Some("call_1")), tool_result("call_1")]));
        let kept = drop_failed_assistant_turns(&list);
        assert_eq!(roles(&kept), vec!["assistant", "toolResult"]);
    }

    #[test]
    fn drops_an_aborted_turn_the_same_as_an_errored_one() {
        let list = messages(json!([assistant("aborted", None)]));
        assert!(drop_failed_assistant_turns(&list).is_empty());
    }
}
