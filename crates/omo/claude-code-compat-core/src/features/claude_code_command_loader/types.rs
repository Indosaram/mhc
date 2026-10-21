use std::{collections::BTreeMap, fmt, path::PathBuf};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandScope { User, Project, Opencode, OpencodeProject }

impl fmt::Display for CommandScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::User => "user", Self::Project => "project",
            Self::Opencode => "opencode", Self::OpencodeProject => "opencode-project",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffDefinition {
    pub label: String,
    pub agent: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDefinition {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtask: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoffs: Option<Vec<HandoffDefinition>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CommandFrontmatter {
    pub description: Option<String>,
    #[serde(rename = "argument-hint")]
    pub argument_hint: Option<String>,
    pub agent: Option<String>,
    pub model: Option<serde_json::Value>,
    pub subtask: Option<bool>,
    pub handoffs: Option<Vec<HandoffDefinition>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedCommand {
    pub name: String,
    pub path: PathBuf,
    pub definition: CommandDefinition,
    pub scope: CommandScope,
}

pub type CommandRecord = BTreeMap<String, CommandDefinition>;
