#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WindowsSandboxReadinessResponse {
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::windows_sandbox_readiness::WindowsSandboxReadiness>,
}
