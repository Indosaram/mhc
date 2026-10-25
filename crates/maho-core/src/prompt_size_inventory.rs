//! Prompt size inventory collection and types used by prompt-size inspection.

pub trait PromptSizeInventory: std::fmt::Debug + Send + Sync {
    /// Nearest package.json name of an extension path.
    fn package_name_of(&self, path: &str) -> Option<String>;
    /// Configured MCP server names, from config only: never attaches or exposes endpoints.
    fn mcp_server_names(&self, cwd: &str, project_trusted: bool) -> Vec<String>;
}

/// Startup notes for prompt size reporting.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSizeStartupNotes {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpStartupNotes>,
    pub third_party_session_start: ThirdPartySessionStartNotes,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStartupNotes {
    pub count: usize,
    pub names: Vec<String>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThirdPartySessionStartNotes {
    pub names: Vec<String>,
    pub note: String,
}
