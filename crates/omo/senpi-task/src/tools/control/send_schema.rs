//! Port of `tools/control/send-schema.ts`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Structured protocol message accepted by `task_send`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StructuredMessageInput {
    #[serde(rename = "shutdown_request")]
    ShutdownRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "shutdown_response")]
    ShutdownResponse {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        approve: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// `message` field of `TaskSendParams`: a plain string or a structured message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TaskSendMessage {
    Plain(String),
    Structured(StructuredMessageInput),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSendInput {
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<TaskSendMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all_scope: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberScopedTaskSendInput {
    pub to: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

fn recipient_schema() -> Value {
    json!({ "description": "Child task id/name or team member name.", "type": "string" })
}

fn plain_message_schema() -> Value {
    json!({ "description": "The instruction or context to deliver.", "type": "string" })
}

fn summary_schema() -> Value {
    json!({ "description": "Optional one-line summary for team messages.", "type": "string" })
}

fn structured_message_schema() -> Value {
    json!({
        "anyOf": [
            {
                "type": "object",
                "properties": {
                    "type": { "const": "shutdown_request", "type": "string" },
                    "reason": { "description": "Optional shutdown request reason.", "type": "string" }
                },
                "required": ["type"]
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "shutdown_response", "type": "string" },
                    "request_id": {
                        "description": "Accepted for protocol shape; ignored by senpi-task.",
                        "type": "string"
                    },
                    "approve": {
                        "description": "Approve or reject the shutdown request.",
                        "type": "boolean"
                    },
                    "reason": { "description": "Required when approve is false.", "type": "string" }
                },
                "required": ["type", "approve"]
            }
        ]
    })
}

/// JSON schema equivalent of the TypeBox `TaskSendParams` definition.
pub fn task_send_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "to": recipient_schema(),
            "message": { "anyOf": [plain_message_schema(), structured_message_schema()] },
            "team_run_id": {
                "description": "Team run id for lead-to-member messages or shutdown messages.",
                "type": "string"
            },
            "summary": summary_schema(),
            "all_scope": {
                "description": "Allow messaging a child owned by another session. Off by default.",
                "type": "boolean"
            }
        },
        "required": ["to"]
    })
}

/// JSON schema equivalent of the TypeBox `MemberScopedTaskSendParams` definition.
pub fn member_scoped_task_send_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "to": recipient_schema(),
            "message": plain_message_schema(),
            "summary": summary_schema()
        },
        "required": ["to", "message"]
    })
}

pub fn is_structured_message(message: Option<&TaskSendMessage>) -> bool {
    matches!(message, Some(TaskSendMessage::Structured(_)))
}

/// Narrowing helper mirroring the TS type guard.
pub fn as_structured_message(message: Option<&TaskSendMessage>) -> Option<&StructuredMessageInput> {
    match message {
        Some(TaskSendMessage::Structured(structured)) => Some(structured),
        _ => None,
    }
}
