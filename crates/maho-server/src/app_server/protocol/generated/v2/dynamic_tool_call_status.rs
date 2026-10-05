#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DynamicToolCallStatus {
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
}
