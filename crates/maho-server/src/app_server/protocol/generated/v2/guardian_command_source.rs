#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GuardianCommandSource {
    #[serde(rename = "shell")]
    Shell,
    #[serde(rename = "unifiedExec")]
    UnifiedExec,
}
