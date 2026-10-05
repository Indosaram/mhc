#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum McpServerElicitationAction {
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "decline")]
    Decline,
    #[serde(rename = "cancel")]
    Cancel,
}
