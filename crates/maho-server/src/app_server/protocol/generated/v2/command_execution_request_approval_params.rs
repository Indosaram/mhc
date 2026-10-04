#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecutionRequestApprovalParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: f64,
    #[serde(rename = "approvalId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub approval_id: Option<Option<String>>,
    #[serde(rename = "environmentId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub environment_id: Option<String>,
    #[serde(rename = "reason", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub reason: Option<Option<String>>,
    #[serde(rename = "networkApprovalContext", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub network_approval_context: Option<Option<Box<crate::app_server::protocol::generated::v2::network_approval_context::NetworkApprovalContext>>>,
    #[serde(rename = "command", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub command: Option<Option<String>>,
    #[serde(rename = "cwd", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cwd: Option<Option<Box<crate::app_server::protocol::generated::legacy_app_path_string::LegacyAppPathString>>>,
    #[serde(rename = "commandActions", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub command_actions: Option<Option<Vec<Box<crate::app_server::protocol::generated::v2::command_action::CommandAction>>>>,
    #[serde(rename = "proposedExecpolicyAmendment", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub proposed_execpolicy_amendment: Option<Option<Box<crate::app_server::protocol::generated::v2::exec_policy_amendment::ExecPolicyAmendment>>>,
    #[serde(rename = "proposedNetworkPolicyAmendments", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub proposed_network_policy_amendments: Option<Option<Vec<Box<crate::app_server::protocol::generated::v2::network_policy_amendment::NetworkPolicyAmendment>>>>,
}
