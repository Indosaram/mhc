#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WindowsSandboxSetupCompletedNotification {
    #[serde(rename = "mode")]
    pub mode: Box<crate::app_server::protocol::generated::v2::windows_sandbox_setup_mode::WindowsSandboxSetupMode>,
    #[serde(rename = "success")]
    pub success: bool,
    #[serde(rename = "error", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub error: Option<String>,
}
