#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum HookExecutionMode {
    #[serde(rename = "sync")]
    Sync,
    #[serde(rename = "async")]
    Async,
}
