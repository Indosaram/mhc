#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TurnStatus {
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "interrupted")]
    Interrupted,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "inProgress")]
    InProgress,
}
