use std::{collections::BTreeMap, fmt, path::PathBuf};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpScope { User, Project, Local }
impl fmt::Display for McpScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(match self { Self::User => "user", Self::Project => "project", Self::Local => "local" }) }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpTransport { Http, Sse, Stdio }
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ClaudeCodeMcpServer {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub transport: Option<McpTransport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<McpScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ClaudeCodeMcpConfig { pub mcp_servers: BTreeMap<String, ClaudeCodeMcpServer> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpServerConfig {
    Local { command: Vec<String>, #[serde(skip_serializing_if = "Option::is_none")] environment: Option<BTreeMap<String, String>>, enabled: bool },
    Remote { url: String, #[serde(skip_serializing_if = "Option::is_none")] headers: Option<BTreeMap<String, String>>, #[serde(skip_serializing_if = "Option::is_none")] oauth: Option<McpOAuthConfig>, enabled: bool },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedMcpServer { pub name: String, pub scope: McpScope, pub config: McpServerConfig }
#[derive(Debug, Clone, Default)]
pub struct McpLoadResult { pub servers: BTreeMap<String, McpServerConfig>, pub loaded_servers: Vec<LoadedMcpServer> }
