//! Port of `packages/coding-agent/src/core/tools/bash-schema.ts`.

use crate::definition::ToolError;
use crate::truncate::TruncationResult;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct BashSpawnContext {
    pub command: String,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

pub type BashSpawnHook =
    Arc<dyn Fn(BashSpawnContext) -> Result<BashSpawnContext, ToolError> + Send + Sync>;

/// `getShellEnv()` supplier; `maho-core` injects the agent-bin-on-PATH env.
pub type ShellEnvProvider = Arc<dyn Fn() -> BTreeMap<String, String> + Send + Sync>;

#[derive(Deserialize)]
pub struct BashToolInput {
    pub command: String,
    pub timeout: Option<f64>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust: Option<Value>,
}

#[derive(Clone)]
pub struct ShellToolConfig {
    pub name: String,
    pub label: String,
    pub shell_name: String,
    pub prompt: String,
    pub prompt_snippet: String,
    pub prompt_guidelines: Option<Vec<String>>,
    pub full_prompt_guidelines: Option<Vec<String>>,
    pub temp_file_prefix: String,
}

pub const BASH_PROMPT_SNIPPET: &str = "Execute bash commands (ls, rg, find, etc.)";
pub const BASH_PROMPT_GUIDELINE: &str =
    "You can inspect PI_* environment variables for current model and session details.";
pub const BASH_PROMPT_SHORT_GUIDELINE: &str = "PI_* env vars = current model + session details.";

pub fn bash_schema_json() -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": { "type": "string", "description": "Shell command to execute" },
            "timeout": { "type": "number", "description": "Timeout in seconds (optional, no default timeout)" }
        },
        "required": ["command"]
    })
}

pub fn bash_output_schema_json() -> Value {
    json!({
        "type": "object",
        "properties": {
            "output": { "type": "string" },
            "truncated": { "type": "boolean" },
            "full_output_path": { "type": "string" },
            "exit_code": { "type": "number" },
            "wall_time_seconds": { "type": "number" }
        },
        "required": ["output", "truncated", "exit_code", "wall_time_seconds"]
    })
}

pub fn bash_tool_config() -> ShellToolConfig {
    ShellToolConfig {
        name: "bash".to_owned(),
        label: "bash".to_owned(),
        shell_name: "bash".to_owned(),
        prompt: "$".to_owned(),
        prompt_snippet: BASH_PROMPT_SNIPPET.to_owned(),
        prompt_guidelines: Some(vec![BASH_PROMPT_GUIDELINE.to_owned()]),
        full_prompt_guidelines: Some(vec![BASH_PROMPT_GUIDELINE.to_owned()]),
        temp_file_prefix: "pi-bash".to_owned(),
    }
}
