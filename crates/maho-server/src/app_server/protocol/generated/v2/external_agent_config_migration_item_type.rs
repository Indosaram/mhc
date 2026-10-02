#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ExternalAgentConfigMigrationItemType {
    #[serde(rename = "AGENTS_MD")]
    AGENTSMD,
    #[serde(rename = "CONFIG")]
    CONFIG,
    #[serde(rename = "SKILLS")]
    SKILLS,
    #[serde(rename = "PLUGINS")]
    PLUGINS,
    #[serde(rename = "MCP_SERVER_CONFIG")]
    MCPSERVERCONFIG,
    #[serde(rename = "SUBAGENTS")]
    SUBAGENTS,
    #[serde(rename = "HOOKS")]
    HOOKS,
    #[serde(rename = "COMMANDS")]
    COMMANDS,
    #[serde(rename = "MEMORY")]
    MEMORY,
    #[serde(rename = "SESSIONS")]
    SESSIONS,
}
