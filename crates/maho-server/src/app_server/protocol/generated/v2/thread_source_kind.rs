#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadSourceKind {
    #[serde(rename = "cli")]
    Cli,
    #[serde(rename = "vscode")]
    Vscode,
    #[serde(rename = "exec")]
    Exec,
    #[serde(rename = "appServer")]
    AppServer,
    #[serde(rename = "subAgent")]
    SubAgent,
    #[serde(rename = "subAgentReview")]
    SubAgentReview,
    #[serde(rename = "subAgentCompact")]
    SubAgentCompact,
    #[serde(rename = "subAgentThreadSpawn")]
    SubAgentThreadSpawn,
    #[serde(rename = "subAgentOther")]
    SubAgentOther,
    #[serde(rename = "unknown")]
    Unknown,
}
