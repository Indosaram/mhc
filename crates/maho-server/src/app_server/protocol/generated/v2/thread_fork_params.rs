pub type ThreadForkParamsConfig1 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadForkParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "lastTurnId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub last_turn_id: Option<Option<String>>,
    #[serde(rename = "model", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub model: Option<Option<String>>,
    #[serde(rename = "modelProvider", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub model_provider: Option<Option<String>>,
    #[serde(rename = "serviceTier", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub service_tier: Option<Option<String>>,
    #[serde(rename = "cwd", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cwd: Option<Option<String>>,
    #[serde(rename = "approvalPolicy", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub approval_policy: Option<Option<Box<crate::app_server::protocol::generated::v2::ask_for_approval::AskForApproval>>>,
    #[serde(rename = "approvalsReviewer", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub approvals_reviewer: Option<Option<Box<crate::app_server::protocol::generated::v2::approvals_reviewer::ApprovalsReviewer>>>,
    #[serde(rename = "sandbox", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub sandbox: Option<Option<Box<crate::app_server::protocol::generated::v2::sandbox_mode::SandboxMode>>>,
    #[serde(rename = "config", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub config: Option<Option<ThreadForkParamsConfig1>>,
    #[serde(rename = "baseInstructions", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub base_instructions: Option<Option<String>>,
    #[serde(rename = "developerInstructions", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub developer_instructions: Option<Option<String>>,
    #[serde(rename = "ephemeral", default, skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<bool>,
    #[serde(rename = "threadSource", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub thread_source: Option<Option<Box<crate::app_server::protocol::generated::v2::thread_source::ThreadSource>>>,
}
