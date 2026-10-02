use maho_ai::types::{ContentBlock, Message, StopReason};
use std::collections::HashSet;

pub fn mark_failed_turn_fragments(messages: &[Message]) -> Vec<bool> {
    let mut kept_ids = HashSet::new();
    let mut failed_ids = HashSet::new();
    for message in messages {
        if let Message::Assistant(assistant) = message {
            let ids = if matches!(assistant.stop_reason, StopReason::Error | StopReason::Aborted) { &mut failed_ids } else { &mut kept_ids };
            for block in &assistant.content {
                if let ContentBlock::ToolCall(call) = block { ids.insert(&call.id); }
            }
        }
    }
    failed_ids.retain(|id|!kept_ids.contains(id));
    messages.iter().map(|message|match message {
        Message::Assistant(assistant) => matches!(assistant.stop_reason, StopReason::Error | StopReason::Aborted),
        Message::ToolResult(result) => failed_ids.contains(&result.tool_call_id),
        _ => false,
    }).collect()
}
