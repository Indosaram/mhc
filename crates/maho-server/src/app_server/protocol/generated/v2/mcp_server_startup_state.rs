#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum McpServerStartupState {
    #[serde(rename = "starting")]
    Starting,
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "cancelled")]
    Cancelled,
}
