//! The slice of the OpenCode session client that team-core needs (`TeamSessionContext`).

use serde_json::Value;

/// `SessionLookupResponse`: `{ data?, error? }`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionLookupResponse {
    pub data: Option<Value>,
    pub error: Option<Value>,
}

/// A thrown client error. `status` / `message` mirror the fields team-core inspects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionClientError {
    pub status: Option<i64>,
    pub message: Option<String>,
}

impl SessionClientError {
    #[must_use]
    pub fn message(message: &str) -> Self {
        Self {
            status: None,
            message: Some(message.to_owned()),
        }
    }
}

/// `TeamSessionClient`: `session.get` plus the optional `session.messages`.
pub trait TeamSessionClient: Send + Sync {
    fn get(&self, session_id: &str) -> Result<SessionLookupResponse, SessionClientError>;

    /// `None` when the client has no `session.messages` loader.
    fn messages(&self, _session_id: &str) -> Option<Result<Value, SessionClientError>> {
        None
    }
}

/// `TeamSessionContext`.
pub struct TeamSessionContext<'a> {
    pub client: &'a dyn TeamSessionClient,
}

pub(crate) fn get_messages_data(response: &Value) -> Vec<Value> {
    match response {
        Value::Object(record) => match record.get("data") {
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        },
        Value::Array(items) => items.clone(),
        _ => Vec::new(),
    }
}

pub(crate) fn value_contains_message_id(value: &Value, message_id: &str) -> bool {
    match value {
        Value::String(text) => text.contains(message_id),
        Value::Array(items) => items
            .iter()
            .any(|entry| value_contains_message_id(entry, message_id)),
        Value::Object(record) => record
            .values()
            .any(|entry| value_contains_message_id(entry, message_id)),
        _ => false,
    }
}
