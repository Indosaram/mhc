use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action { Allow, Deny, Ask }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule { pub permission: String, pub pattern: String, pub action: Action }
pub type Ruleset = Vec<Rule>;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PermissionValue { Action(Action), Patterns(Map<String, Value>) }
pub type PermissionConfig = Vec<(String, PermissionValue)>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionPresetName { FullAccess, Workspace, AcceptEdits, Auto, ReadOnly, Ask }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reply { Once, Always, Reject }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionDecision { Once, Always, Reject, Allow }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub permission: String,
    pub patterns: Vec<String>,
    pub always: Vec<String>,
    pub metadata: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<RequestTool>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestTool {
    #[serde(rename = "messageID")]
    pub message_id: String,
    #[serde(rename = "callID")]
    pub call_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplyInput {
    #[serde(rename = "requestID")]
    pub request_id: String,
    pub reply: Reply,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub always: Option<Vec<String>>,
}
impl Request { pub fn new(id: String, session_id: String, permission: String, patterns: Vec<String>, always: Vec<String>, metadata: Map<String, Value>) -> Self { Self { id, session_id, permission, patterns, always, metadata, tool: None } } }
impl Default for ReplyInput { fn default() -> Self { Self { request_id: String::new(), reply: Reply::Reject, message: None, always: None } } }
#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("The user rejected permission to use this specific tool call.")]
    Rejected,
    #[error("The user rejected permission to use this specific tool call with the following feedback: {0}")]
    Corrected(String),
    #[error("The user has specified a rule which prevents you from using this specific tool call.")]
    Denied(Vec<String>),
}
