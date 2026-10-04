use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectedNotification {
    pub method: String,
    pub params: Value,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionTurnCompletion {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionResult {
    pub notifications: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_completion: Option<ProjectionTurnCompletion>,
}
pub fn message_id_from_message(message: &Value) -> Option<&str> { message["responseId"].as_str() }
