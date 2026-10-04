#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum McpServerStatusDetail {
    #[serde(rename = "full")]
    Full,
    #[serde(rename = "toolsAndAuthOnly")]
    ToolsAndAuthOnly,
}
