#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AppToolApproval {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "prompt")]
    Prompt,
    #[serde(rename = "writes")]
    Writes,
    #[serde(rename = "approve")]
    Approve,
}
