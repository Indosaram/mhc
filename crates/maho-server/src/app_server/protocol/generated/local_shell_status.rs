#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LocalShellStatus {
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "in_progress")]
    InProgress,
    #[serde(rename = "incomplete")]
    Incomplete,
}
