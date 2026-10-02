use std::collections::HashSet;
use maho_ai::types::{ContentBlock,Message,StopReason,ToolResultMessage};
pub const TOOL_RESULT_PLACEHOLDER:&str="Tool output unavailable (context compacted)";
pub fn repair_orphaned_tool_results(messages:&[Message],now:i64)->Vec<Message> {
    let mut calls=HashSet::new();let mut results=HashSet::new();
    for message in messages {
        match message {
            Message::Assistant(assistant)=>{for block in &assistant.content {if let ContentBlock::ToolCall(call)=block {calls.insert(call.id.clone());}}}
            Message::ToolResult(result)=>{results.insert(result.tool_call_id.clone());}
            Message::User(_) | Message::ConfigurationUpdate(_)=>{}
        }
    }
    let mut dangling:HashSet<_>=calls.difference(&results).cloned().collect();
    let mut output=Vec::new();
    for message in messages {
        if let Message::ToolResult(result)=message && !calls.contains(&result.tool_call_id) {
            let mut result=result.clone();result.content=vec![ContentBlock::text(TOOL_RESULT_PLACEHOLDER)];output.push(Message::ToolResult(result));continue;
        }
        output.push(message.clone());
        if let Message::Assistant(assistant)=message {
            if matches!(assistant.stop_reason,StopReason::Error|StopReason::Aborted) {continue;}
            for block in &assistant.content {
                let ContentBlock::ToolCall(call)=block else {continue;};
                if !dangling.remove(&call.id) {continue;}
                let incomplete=call.incomplete==Some(true);
                let text=if incomplete {
                    match &call.error_message {
                        Some(error)=>format!("{error}{} Re-issue the tool call with complete arguments.",if error.ends_with('.') {""} else {"."}),
                        None=>format!("Tool call \"{}\" was not executed: the response ended before the tool call was complete. Re-issue the tool call with complete arguments.",call.name),
                    }
                } else {TOOL_RESULT_PLACEHOLDER.to_owned()};
                output.push(Message::ToolResult(ToolResultMessage {tool_call_id:call.id.clone(),tool_name:call.name.clone(),content:vec![ContentBlock::text(text)],details:None,usage:None,added_tool_names:None,is_error:incomplete,timestamp:if assistant.timestamp!=0 {assistant.timestamp.saturating_add(1)} else {now}}));
            }
        }
    }
    output
}
