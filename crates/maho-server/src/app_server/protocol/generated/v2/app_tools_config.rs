#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppToolsConfigValue1 {
    #[serde(rename = "enabled", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub enabled: Option<bool>,
    #[serde(rename = "approval_mode", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub approval_mode: Option<Box<crate::app_server::protocol::generated::v2::app_tool_approval::AppToolApproval>>,
}

pub type AppToolsConfig = std::collections::BTreeMap<String, AppToolsConfigValue1>;
