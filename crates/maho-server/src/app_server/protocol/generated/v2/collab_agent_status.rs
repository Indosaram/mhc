#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CollabAgentStatus {
    #[serde(rename = "pendingInit")]
    PendingInit,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "interrupted")]
    Interrupted,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "errored")]
    Errored,
    #[serde(rename = "shutdown")]
    Shutdown,
    #[serde(rename = "notFound")]
    NotFound,
}
