use serde_json::Value;

use crate::internal_initiator_marker::{
    MessageLike, TextPartLike, has_internal_initiator_marker,
    is_synthetic_or_internal_user_message, is_terminal_no_reply_user_message,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageFinishValue {
    True,
    Reason(String),
}

pub fn message_role(message: &Value) -> Option<&str> {
    if let Some(role) = message
        .get("info")
        .and_then(Value::as_object)
        .and_then(|info| info.get("role"))
        .and_then(Value::as_str)
    {
        return Some(role);
    }
    message.get("role").and_then(Value::as_str)
}

pub fn message_finish(message: &Value) -> Option<MessageFinishValue> {
    let check_finish = |v: &Value| -> Option<MessageFinishValue> {
        if v.as_bool() == Some(true) {
            return Some(MessageFinishValue::True);
        }
        if let Some(s) = v.as_str()
            && !s.is_empty()
        {
            return Some(MessageFinishValue::Reason(s.to_string()));
        }
        None
    };

    if let Some(info) = message.get("info").and_then(Value::as_object)
        && let Some(f) = info.get("finish")
        && let Some(res) = check_finish(f)
    {
        return Some(res);
    }
    message.get("finish").and_then(check_finish)
}

pub fn message_completed(message: &Value) -> bool {
    let time = message
        .get("info")
        .and_then(Value::as_object)
        .and_then(|info| info.get("time"))
        .and_then(Value::as_object);
    let Some(completed) = time.and_then(|t| t.get("completed")) else {
        return false;
    };
    if completed.is_number() {
        return true;
    }
    completed.as_str().is_some_and(|s| !s.is_empty())
}

pub fn message_has_terminal_error(message: &Value) -> bool {
    if let Some(info) = message.get("info").and_then(Value::as_object)
        && let Some(err) = info.get("error")
        && !err.is_null()
    {
        return true;
    }
    message.get("error").is_some_and(|err| !err.is_null())
}

fn to_internal_initiator_message_like(message: &Value) -> Option<MessageLike> {
    if !message.is_object() {
        return None;
    }

    let mut result = MessageLike::default();
    if let Some(role) = message
        .get("info")
        .and_then(Value::as_object)
        .and_then(|info| info.get("role"))
        .and_then(Value::as_str)
    {
        result.info_role = Some(role.to_string());
    }
    if let Some(role) = message.get("role").and_then(Value::as_str) {
        result.role = Some(role.to_string());
    }
    if let Some(parts) = message.get("parts").and_then(Value::as_array) {
        let text_parts = parts
            .iter()
            .map(|part| {
                let part_type = part.get("type").and_then(Value::as_str).map(str::to_string);
                let text = part.get("text").and_then(Value::as_str).map(str::to_string);
                let synthetic = part
                    .get("synthetic")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                TextPartLike {
                    part_type,
                    text,
                    synthetic,
                }
            })
            .collect();
        result.parts = Some(text_parts);
    }
    Some(result)
}

pub fn message_is_synthetic_or_internal_user(message: &Value) -> bool {
    to_internal_initiator_message_like(message)
        .as_ref()
        .is_some_and(is_synthetic_or_internal_user_message)
}

pub fn message_is_terminal_no_reply_user(message: &Value) -> bool {
    to_internal_initiator_message_like(message)
        .as_ref()
        .is_some_and(is_terminal_no_reply_user_message)
}

pub fn message_has_internal_initiator_marker(message: &Value) -> bool {
    let Some(parts) = message.get("parts").and_then(Value::as_array) else {
        return false;
    };
    parts.iter().any(|part| {
        part.get("type").and_then(Value::as_str) == Some("text")
            && part
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(has_internal_initiator_marker)
    })
}

fn part_tool_name(part: &Value) -> Option<&str> {
    if let Some(name) = part.get("name").and_then(Value::as_str) {
        return Some(name);
    }
    if let Some(tool) = part.get("tool").and_then(Value::as_str) {
        return Some(tool);
    }
    part.get("toolName").and_then(Value::as_str)
}

fn part_is_tool_call(part: &Value) -> bool {
    matches!(
        part.get("type").and_then(Value::as_str),
        Some("tool" | "tool_use" | "tool-call" | "tool-invocation")
    )
}

fn part_is_question_tool(part: &Value) -> bool {
    if !part_is_tool_call(part) {
        return false;
    }
    let Some(tool_name) = part_tool_name(part) else {
        return false;
    };
    let lower = tool_name.to_lowercase();
    matches!(
        lower.as_str(),
        "question" | "ask_user_question" | "askuserquestion"
    )
}

fn part_is_unanswered_question_tool(part: &Value) -> bool {
    if !part_is_question_tool(part) {
        return false;
    }
    let Some(state) = part.get("state").and_then(Value::as_object) else {
        return true;
    };
    state.get("status").and_then(Value::as_str) != Some("completed")
}

fn part_is_waiting_on_tool(part: &Value) -> bool {
    if !part_is_tool_call(part) {
        return false;
    }
    let Some(state) = part.get("state").and_then(Value::as_object) else {
        return false;
    };
    matches!(
        state.get("status").and_then(Value::as_str),
        Some("pending" | "running")
    )
}

fn part_is_unresolved_tool(part: &Value) -> bool {
    if !part_is_tool_call(part) {
        return false;
    }
    let Some(state) = part.get("state").and_then(Value::as_object) else {
        return true;
    };
    state.get("status").and_then(Value::as_str) != Some("completed")
}

fn part_has_substantive_assistant_output(part: &Value) -> bool {
    match part.get("type").and_then(Value::as_str) {
        Some("step-start" | "step-finish") => false,
        Some("text") => part
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty()),
        Some(other) => !other.is_empty(),
        None => false,
    }
}

pub fn message_has_question_tool(message: &Value) -> bool {
    message
        .get("parts")
        .and_then(Value::as_array)
        .is_some_and(|parts| parts.iter().any(part_is_unanswered_question_tool))
}

pub fn message_has_waiting_tool(message: &Value) -> bool {
    message
        .get("parts")
        .and_then(Value::as_array)
        .is_some_and(|parts| parts.iter().any(part_is_waiting_on_tool))
}

pub fn message_has_unresolved_tool(message: &Value) -> bool {
    message
        .get("parts")
        .and_then(Value::as_array)
        .is_some_and(|parts| parts.iter().any(part_is_unresolved_tool))
}

pub fn message_has_substantive_assistant_output(message: &Value) -> bool {
    message
        .get("parts")
        .and_then(Value::as_array)
        .is_some_and(|parts| parts.iter().any(part_has_substantive_assistant_output))
}
