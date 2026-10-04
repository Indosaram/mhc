#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CommandExecutionSource {
    #[serde(rename = "agent")]
    Agent,
    #[serde(rename = "userShell")]
    UserShell,
    #[serde(rename = "unifiedExecStartup")]
    UnifiedExecStartup,
    #[serde(rename = "unifiedExecInteraction")]
    UnifiedExecInteraction,
}
