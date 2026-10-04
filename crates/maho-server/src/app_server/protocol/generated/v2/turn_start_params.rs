#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnStartParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "clientUserMessageId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub client_user_message_id: Option<Option<String>>,
    #[serde(rename = "input")]
    pub input: Vec<Box<crate::app_server::protocol::generated::v2::user_input::UserInput>>,
    #[serde(rename = "cwd", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cwd: Option<Option<String>>,
    #[serde(rename = "approvalPolicy", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub approval_policy: Option<Option<Box<crate::app_server::protocol::generated::v2::ask_for_approval::AskForApproval>>>,
    #[serde(rename = "approvalsReviewer", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub approvals_reviewer: Option<Option<Box<crate::app_server::protocol::generated::v2::approvals_reviewer::ApprovalsReviewer>>>,
    #[serde(rename = "sandboxPolicy", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub sandbox_policy: Option<Option<Box<crate::app_server::protocol::generated::v2::sandbox_policy::SandboxPolicy>>>,
    #[serde(rename = "model", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub model: Option<Option<String>>,
    #[serde(rename = "serviceTier", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub service_tier: Option<Option<String>>,
    #[serde(rename = "effort", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub effort: Option<Option<Box<crate::app_server::protocol::generated::reasoning_effort::ReasoningEffort>>>,
    #[serde(rename = "summary", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub summary: Option<Option<Box<crate::app_server::protocol::generated::reasoning_summary::ReasoningSummary>>>,
    #[serde(rename = "personality", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub personality: Option<Option<Box<crate::app_server::protocol::generated::personality::Personality>>>,
    #[serde(rename = "outputSchema", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub output_schema: Option<Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>>,
}
