#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpToolCallError {
    #[serde(rename = "message")]
    pub message: String,
}
