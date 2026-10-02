#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum HookRunStatus {
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "blocked")]
    Blocked,
    #[serde(rename = "stopped")]
    Stopped,
}
