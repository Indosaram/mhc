use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Allow,
    Deny,
    Ask,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub permission: String,
    pub pattern: String,
    pub action: Action,
}
pub type Ruleset = Vec<Rule>;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PermissionValue {
    Action(Action),
    Patterns(Map<String, Value>),
}
pub type PermissionConfig = Vec<(String, PermissionValue)>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionPresetName {
    Guarded,
    FullAccess,
    Workspace,
    AcceptEdits,
    Auto,
    ReadOnly,
    Ask,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reply {
    Once,
    Always,
    Reject,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionDecision {
    Once,
    Always,
    Reject,
    Allow,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub permission: String,
    pub patterns: Vec<String>,
    pub always: Vec<String>,
    /// Lexical path spellings for display and deny checks; allows use only the resolved targets.
    #[serde(
        rename = "originalPaths",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub original_paths: Option<Vec<String>>,
    pub metadata: Map<String, Value>,
    /// A wider "Allow always" scope the prompt may offer (senpi#3403), for a tool without its own
    /// permission parser.
    #[serde(
        rename = "widerAlways",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub wider_always: Option<WiderAlways>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<RequestTool>,
}
/// The wider "Allow always" choice: this label, these patterns when chosen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WiderAlways {
    pub label: String,
    pub patterns: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestTool {
    /// The message that raised the request. senpi carries it for RPC clients that correlate by
    /// message; the engine here does not hand tools a message id, so it is absent.
    #[serde(rename = "messageID", default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(rename = "callID")]
    pub call_id: String,
    /// The calling tool's id when another tool (a codemode script) issued this call.
    #[serde(
        rename = "parentCallID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_call_id: Option<String>,
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
impl Request {
    pub fn new(
        id: String,
        session_id: String,
        permission: String,
        patterns: Vec<String>,
        always: Vec<String>,
        metadata: Map<String, Value>,
    ) -> Self {
        Self {
            id,
            session_id,
            permission,
            patterns,
            always,
            original_paths: None,
            metadata,
            wider_always: None,
            tool: None,
        }
    }
}
impl Default for ReplyInput {
    fn default() -> Self {
        Self {
            request_id: String::new(),
            reply: Reply::Reject,
            message: None,
            always: None,
        }
    }
}
/// The rule every refusal states to the model after its fixed first sentence (clients match that
/// sentence as a prefix): a refused call's purpose is not to be reached by another route. Without
/// it, "this specific tool call" reads as permission to try a different tool.
pub const NO_OTHER_ROUTE: &str =
    "do not get the same result another way (another tool, command, path or host)";
pub const DENIAL_IS_FINAL: &str = "Treat this as final for what the call was meant to do: do not get the same result another way (another tool, command, path or host). Tell the user what was refused, and continue only with work that does not depend on it.";
#[derive(Debug)]
pub enum PermissionError {
    Rejected,
    Corrected(String),
    Denied(Vec<String>),
}
impl std::fmt::Display for PermissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected => write!(
                f,
                "The user rejected permission to use this specific tool call. {DENIAL_IS_FINAL}"
            ),
            Self::Corrected(feedback) => write!(
                f,
                "The user rejected permission to use this specific tool call with the following feedback: {feedback}\nFollow the feedback. Unless it tells you otherwise, {NO_OTHER_ROUTE}."
            ),
            Self::Denied(_) => write!(
                f,
                "The user has specified a rule which prevents you from using this specific tool call. {DENIAL_IS_FINAL}"
            ),
        }
    }
}
impl std::error::Error for PermissionError {}
