use super::turn_log::TurnLog;
use serde_json::{Value, json};

pub fn persisted_history_turns(branch: &[Value], thread_id: &str) -> Vec<Value> {
    branch.iter().filter(|entry| entry["type"] == "message" && entry["message"]["role"] == "user").map(|entry| {
        let entry_id = entry["id"].as_str().unwrap_or_default();
        json!({"id":format!("history-{thread_id}-{entry_id}"),"items":[{"type":"userMessage","id":format!("{entry_id}:user"),"content":user_input_from_message(&entry["message"])}],"status":"completed","error":null})
    }).collect()
}
pub fn user_input_from_message(message: &Value) -> Vec<Value> {
    match &message["content"] {
        Value::String(text) => vec![json!({"type":"text","text":text,"text_elements":[]})],
        Value::Array(content) => content.iter().filter_map(|block| match block["type"].as_str() {
            Some("text") => Some(json!({"type":"text","text":block["text"],"text_elements":[]})),
            Some("image") => Some(json!({"type":"image","url":format!("data:{};base64,{}",block["mimeType"].as_str().unwrap_or_default(),block["data"].as_str().unwrap_or_default())})),
            _ => None,
        }).collect(),
        _ => Vec::new(),
    }
}
pub fn runtime_history_turns(turn_log: &mut TurnLog, thread_id: &str) -> Vec<Value> {
    turn_log.read_turns(thread_id).iter().map(|turn| json!({"id":turn.turn_id,"items":turn.items,"status":match turn.status { super::turn_log::TurnStatus::Running => "inProgress", super::turn_log::TurnStatus::Completed => "completed", super::turn_log::TurnStatus::Failed => "failed", super::turn_log::TurnStatus::Interrupted => "interrupted" },"error":turn.error.as_ref().map(|message| json!({"message":message,"codexErrorInfo":null,"additionalDetails":null}))})).collect()
}
