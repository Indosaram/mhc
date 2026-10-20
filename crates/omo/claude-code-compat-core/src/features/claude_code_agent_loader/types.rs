use std::{collections::BTreeMap, fmt, path::PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentScope {
    User, Project, Opencode, OpencodeProject, DefinitionFile, OpencodeConfig,
}

impl fmt::Display for AgentScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::User => "user", Self::Project => "project", Self::Opencode => "opencode",
            Self::OpencodeProject => "opencode-project", Self::DefinitionFile => "definition-file",
            Self::OpencodeConfig => "opencode-config",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode { #[default] Subagent, Primary, All }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentModel {
    String(String),
    Provider { #[serde(rename = "providerID")] provider_id: String, #[serde(rename = "modelID")] model_id: String },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaudeCodeAgentConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<AgentMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<BTreeMap<String, bool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<AgentModel>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AgentFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub model: Option<String>,
    pub tools: Option<Value>,
    pub mode: Option<AgentMode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentJsonDefinition {
    pub name: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub tools: Option<Value>,
    pub mode: Option<AgentMode>,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedAgent {
    pub name: String,
    pub path: PathBuf,
    pub config: ClaudeCodeAgentConfig,
    pub scope: AgentScope,
}
