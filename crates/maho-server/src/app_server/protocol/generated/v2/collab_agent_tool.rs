#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CollabAgentTool {
    #[serde(rename = "spawnAgent")]
    SpawnAgent,
    #[serde(rename = "sendInput")]
    SendInput,
    #[serde(rename = "resumeAgent")]
    ResumeAgent,
    #[serde(rename = "wait")]
    Wait,
    #[serde(rename = "closeAgent")]
    CloseAgent,
}
