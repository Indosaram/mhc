use serde_json::{Value, json};

use crate::logger::log;

use super::message_inspection_error::is_prompt_message_inspection_aborted_str;
use super::prompt_message_state::{
    MessageFinishValue, message_completed, message_finish, message_has_internal_initiator_marker,
    message_has_question_tool, message_has_substantive_assistant_output,
    message_has_terminal_error, message_has_unresolved_tool, message_has_waiting_tool,
    message_is_synthetic_or_internal_user, message_is_terminal_no_reply_user, message_role,
};
use super::timing::{Clock, with_dispatch_timeout};
use super::types::{PromptGateClient, PromptSessionName};

pub fn get_prompt_query(input: &Value) -> Value {
    let Some(query) = input.get("query").and_then(Value::as_object) else {
        return json!({ "directory": "" });
    };
    let directory = query.get("directory").and_then(Value::as_str).unwrap_or("");
    let limit = query.get("limit").and_then(Value::as_u64);
    if let Some(lim) = limit {
        json!({ "directory": directory, "limit": lim })
    } else {
        json!({ "directory": directory })
    }
}

pub fn get_messages_data(response: &Value) -> Vec<Value> {
    if let Some(data) = response.get("data").and_then(Value::as_array) {
        return data.clone();
    }
    if let Some(arr) = response.as_array() {
        return arr.clone();
    }
    Vec::new()
}

pub fn latest_assistant_turn_has_unanswered_question(messages: &[Value]) -> bool {
    for message in messages.iter().rev() {
        let role = message_role(message);
        if role == Some("assistant") {
            return message_has_question_tool(message);
        }
        if role == Some("user") {
            if message_is_synthetic_or_internal_user(message) {
                continue;
            }
            return false;
        }
    }
    false
}

#[allow(unused_assignments)]
pub fn latest_assistant_turn_blocks_internal_prompt(messages: &[Value]) -> bool {
    let mut saw_assistant_after_latest_user = false;
    for message in messages.iter().rev() {
        let role = message_role(message);
        if role == Some("assistant") {
            saw_assistant_after_latest_user = true;
            if message_has_question_tool(message) {
                return true;
            }
            let finish = message_finish(message);
            if finish == Some(MessageFinishValue::Reason("tool-calls".to_string())) {
                let has_parts = message.get("parts").and_then(Value::as_array).is_some();
                return !message.is_object() || !has_parts || message_has_unresolved_tool(message);
            }
            let is_finish_unknown = match &finish {
                None => true,
                Some(MessageFinishValue::Reason(reason)) => reason == "unknown",
                _ => false,
            };
            if is_finish_unknown && !message_has_substantive_assistant_output(message) {
                return !(message_completed(message) && message_has_terminal_error(message));
            }
            if message_completed(message) {
                return false;
            }
            if finish == Some(MessageFinishValue::True) {
                return false;
            }
            if is_finish_unknown {
                return true;
            }
            let has_parts = message.get("parts").and_then(Value::as_array).is_some();
            if !message.is_object() || !has_parts {
                return finish == Some(MessageFinishValue::Reason("tool-calls".to_string()));
            }
            return message_has_waiting_tool(message);
        }
        if role == Some("user") {
            if message_is_synthetic_or_internal_user(message) {
                if message_is_terminal_no_reply_user(message) {
                    continue;
                }
                if !saw_assistant_after_latest_user {
                    if message_has_internal_initiator_marker(message) {
                        continue;
                    }
                    return true;
                }
                continue;
            }
            return false;
        }
    }
    false
}

pub struct SessionLatestAssistantBlocksArgs<'a, C: ?Sized> {
    pub client: &'a C,
    pub session_id: &'a str,
    pub input: &'a Value,
    pub session_name: PromptSessionName,
    pub source: &'a str,
    pub timeout_ms: u64,
    pub clock: &'a dyn Clock,
}

pub async fn session_latest_assistant_blocks_internal_prompt<C: PromptGateClient + ?Sized>(
    args: SessionLatestAssistantBlocksArgs<'_, C>,
) -> bool {
    let query = get_prompt_query(args.input);
    let operation_name = format!(
        "[prompt-async-gate] {} session.messages",
        args.session_name.as_str()
    );

    let messages_future = args.client.session_messages(args.session_id, &query);
    let result = with_dispatch_timeout(
        messages_future,
        args.timeout_ms,
        &operation_name,
        args.clock,
    )
    .await;

    match result {
        Ok(response) => {
            let messages = get_messages_data(&response);
            latest_assistant_turn_blocks_internal_prompt(&messages)
        }
        Err(error) => {
            log(
                "[prompt-async-gate] latest assistant prompt-block check failed",
                Some(&json!({
                    "sessionID": args.session_id,
                    "source": args.source,
                    "error": error,
                })),
            );
            !is_prompt_message_inspection_aborted_str(&error)
        }
    }
}
