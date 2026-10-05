use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalKind { CommandExecution, FileChange }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision { Accept, AcceptForSession, Decline, Cancel }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalOutcome {
    pub allow: bool,
    pub decision: ApprovalDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
pub type SendToThreadSubscribers = Arc<dyn Fn(&str, Value) -> usize + Send + Sync>;
pub const APPROVAL_DECISIONS: &[&str] = &["accept", "acceptForSession", "decline", "cancel"];
pub const PERMISSION_OPTIONS: &[&str] = &["Allow once", "Allow always", "Deny", "Deny with feedback"];
pub const CANCEL_REASON: &str = "approval request was cancelled because the turn ended";
pub const NO_SUBSCRIBER_REASON: &str = "no client connected to approve";
