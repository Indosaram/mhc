#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CommandExecutionStatus {
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "declined")]
    Declined,
}
