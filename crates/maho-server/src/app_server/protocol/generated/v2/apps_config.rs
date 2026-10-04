#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppsConfigDetails01Value2 {
    #[serde(rename = "enabled")]
    pub enabled: bool,
    #[serde(rename = "approvals_reviewer", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub approvals_reviewer: Option<Box<crate::app_server::protocol::generated::v2::approvals_reviewer::ApprovalsReviewer>>,
    #[serde(rename = "destructive_enabled", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub destructive_enabled: Option<bool>,
    #[serde(rename = "open_world_enabled", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub open_world_enabled: Option<bool>,
    #[serde(rename = "default_tools_approval_mode")]
    pub default_tools_approval_mode: Option<Box<crate::app_server::protocol::generated::v2::app_tool_approval::AppToolApproval>>,
    #[serde(rename = "default_tools_enabled")]
    pub default_tools_enabled: Option<bool>,
    #[serde(rename = "tools", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub tools: Option<Box<crate::app_server::protocol::generated::v2::app_tools_config::AppToolsConfig>>,
}

pub type AppsConfigDetails01 = std::collections::BTreeMap<String, AppsConfigDetails01Value2>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppsConfig {
    #[serde(rename = "_default")]
    pub _default: Option<Box<crate::app_server::protocol::generated::v2::apps_default_config::AppsDefaultConfig>>,
    #[serde(flatten)]
    pub details: AppsConfigDetails01,
}
