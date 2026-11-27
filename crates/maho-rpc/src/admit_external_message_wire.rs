//! Port of senpi `packages/coding-agent/src/modes/rpc/admit-external-message-wire.ts`.
//!
//! The wire format and field validation for `admit_external_message` (senpi#6233).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExternalDeliverAs {
    Steer,
    FollowUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalAdmissionKind {
    Started,
    Queued,
    Held,
    AlreadyAdmitted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWhenIdle {
    Start,
    Queue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionControlSender {
    pub kind: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durable_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcAdmitExternalMessageCommand {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub delivery_id: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender: Option<SessionControlSender>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    pub deliver_as: ExternalDeliverAs,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when_idle: Option<ExternalWhenIdle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcAdmitExternalMessageReply {
    pub kind: ExternalAdmissionKind,
    pub turn_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holds: Option<Vec<String>>,
}

pub fn parse_admit_external_message_command(
    value: &Value,
) -> Result<RpcAdmitExternalMessageCommand, String> {
    let Some(obj) = value.as_object() else {
        return Err("admit_external_message must be a JSON object.".to_owned());
    };
    if let Some(err) = admit_external_message_field_error(obj, 1024 * 1024) {
        return Err(err);
    }
    serde_json::from_value(value.clone()).map_err(|e| e.to_string())
}

const MAX_BOUNDED_ID: usize = 256;
const SENDER_SHAPE: &str =
    "sender must be { kind: \"agent\" | \"command_line\", sessionId, ... } of bounded strings.";

fn bounded_id(value: Option<&Value>) -> bool {
    let Some(s) = value.and_then(Value::as_str) else {
        return false;
    };
    !s.is_empty() && s.len() <= MAX_BOUNDED_ID
}

fn is_delivery_sender(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let kind = obj.get("kind").and_then(Value::as_str);
    if !matches!(kind, Some("agent" | "command_line")) {
        return false;
    }
    let allowed = ["kind", "sessionId", "durableSessionId", "label", "deviceId"];
    let required = ["kind", "sessionId"];
    let optional = ["durableSessionId", "label", "deviceId"];
    obj.keys().all(|k| allowed.contains(&k.as_str()))
        && required.iter().all(|k| bounded_id(obj.get(*k)))
        && optional.iter().all(|k| {
            obj.get(*k)
                .is_none_or(|v| v.is_null() || bounded_id(Some(v)))
        })
}

fn field_problem(value: &Map<String, Value>, max_text_characters: usize) -> Option<String> {
    if !bounded_id(value.get("deliveryId")) {
        return Some("needs deliveryId, a non-empty string of at most 256 characters.".to_owned());
    }
    let text_ok = value
        .get("text")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty() && s.len() <= max_text_characters);
    if !text_ok {
        return Some(format!(
            "needs text, a non-empty string of at most {max_text_characters} characters."
        ));
    }
    if !matches!(
        value.get("deliverAs").and_then(Value::as_str),
        Some("steer" | "followUp")
    ) {
        return Some("deliverAs must be \"steer\" or \"followUp\".".to_owned());
    }
    if let Some(display_text) = value.get("displayText")
        && !display_text.is_null()
        && display_text
            .as_str()
            .is_none_or(|s| s.len() > max_text_characters)
    {
        return Some(format!(
            "displayText must be a string of at most {max_text_characters} characters."
        ));
    }
    if let Some(expected_turn_id) = value.get("expectedTurnId")
        && !expected_turn_id.is_null()
        && !bounded_id(Some(expected_turn_id))
    {
        return Some(
            "expectedTurnId must be a non-empty string of at most 256 characters; read it from get_state.currentTurnId."
                .to_owned(),
        );
    }
    if let Some(chain_count) = value.get("chainCount")
        && !chain_count.is_null()
        && !chain_count
            .as_u64()
            .is_some_and(|n| (1..=9_007_199_254_740_991).contains(&n))
    {
        return Some(
            "chainCount must be a positive integer (1 for a chain's first hop).".to_owned(),
        );
    }
    if value.contains_key("remote_origin") || value.contains_key("remoteOrigin") {
        return Some(
            "must not name remote_origin: the engine stamps the device the connection speaks for."
                .to_owned(),
        );
    }
    if let Some(when_idle) = value.get("whenIdle")
        && !when_idle.is_null()
        && !matches!(when_idle.as_str(), Some("start" | "queue"))
    {
        return Some(
            "whenIdle must be \"start\" or \"queue\"; omit it to start a turn on an idle session."
                .to_owned(),
        );
    }
    if let Some(sender) = value.get("sender")
        && !sender.is_null()
        && !is_delivery_sender(sender)
    {
        return Some(SENDER_SHAPE.to_owned());
    }
    None
}

/// Why an `admit_external_message` record is refused, prefixed with its type; `None` for any other record.
pub fn admit_external_message_field_error(
    value: &Map<String, Value>,
    max_text_characters: usize,
) -> Option<String> {
    if value.get("type").and_then(Value::as_str) != Some("admit_external_message") {
        return None;
    }
    field_problem(value, max_text_characters)
        .map(|problem| format!("admit_external_message {problem}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_remote_origin_and_accepts_valid_record() {
        let good = json!({
            "type": "admit_external_message",
            "deliveryId": "d1",
            "text": "hello",
            "deliverAs": "steer",
            "sender": { "kind": "agent", "sessionId": "s1" }
        });
        assert_eq!(
            admit_external_message_field_error(good.as_object().expect("obj"), 1024),
            None
        );

        let spoofed = json!({
            "type": "admit_external_message",
            "deliveryId": "d1",
            "text": "hello",
            "deliverAs": "steer",
            "remote_origin": { "deviceId": "dev" }
        });
        assert!(
            admit_external_message_field_error(spoofed.as_object().expect("obj"), 1024)
                .is_some_and(|e| e.contains("remote_origin"))
        );
    }
}
