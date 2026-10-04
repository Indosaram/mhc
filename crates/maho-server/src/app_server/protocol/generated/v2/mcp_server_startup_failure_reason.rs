#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum McpServerStartupFailureReason {
    #[serde(rename = "reauthenticationRequired")]
    Value,
}
