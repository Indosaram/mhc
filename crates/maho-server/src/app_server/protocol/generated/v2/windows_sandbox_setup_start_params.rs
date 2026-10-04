#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WindowsSandboxSetupStartParams {
    #[serde(rename = "mode")]
    pub mode: Box<crate::app_server::protocol::generated::v2::windows_sandbox_setup_mode::WindowsSandboxSetupMode>,
    #[serde(rename = "cwd", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cwd: Option<Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>>,
}
