use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::Value;

use super::paths::{ResolveCodegraphWorkspacePathsOptions, resolve_codegraph_workspace_paths};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodegraphInitGuidanceInput {
    pub cwd: Option<String>,
    pub tool_name: Option<String>,
    pub tool_output: String,
}

impl CodegraphInitGuidanceInput {
    pub fn new(tool_name: impl Into<Option<String>>, tool_output: impl Into<String>) -> Self {
        Self {
            cwd: None,
            tool_name: tool_name.into(),
            tool_output: tool_output.into(),
        }
    }

    pub fn with_cwd(
        cwd: impl Into<Option<String>>,
        tool_name: impl Into<Option<String>>,
        tool_output: impl Into<String>,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            tool_name: tool_name.into(),
            tool_output: tool_output.into(),
        }
    }

    pub fn from_value(cwd: Option<String>, tool_name: Option<String>, value: &Value) -> Self {
        Self {
            cwd,
            tool_name,
            tool_output: text_from_unknown(value),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodegraphInitGuidanceOptions {
    pub home_dir: Option<PathBuf>,
}

pub fn text_from_unknown(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(arr) => arr
            .iter()
            .map(text_from_unknown)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| format!("{k}: {}", text_from_unknown(v)))
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
    }
}

fn normalize_codegraph_output(output: &str) -> String {
    let Ok(re) = Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])") else {
        return output.to_string();
    };
    re.replace_all(output, "").into_owned()
}

fn is_codegraph_tool(tool_name: Option<&str>) -> bool {
    let Some(name) = tool_name else {
        return false;
    };
    name.starts_with("codegraph.")
        || name.starts_with("codegraph_")
        || name.starts_with("mcp__codegraph__")
}

fn looks_like_codegraph_uninitialized_output(output: &str) -> bool {
    let normalized = normalize_codegraph_output(output);
    if let Ok(uninit_re) = Regex::new(
        r"(?is)CodeGraph not initialized in (.*?)\.\s*Run ['`]codegraph init['`] in that project first\.",
    ) && uninit_re.is_match(&normalized)
    {
        return true;
    }
    let Ok(status_re) = Regex::new(r"(?im)^.*?\bNot initialized\s*$") else {
        return false;
    };
    let Ok(hint_re) = Regex::new(
        r#"(?i)Run\s+["'`]codegraph init["'`]\s+(?:in that project first|to initialize)\.?"#,
    ) else {
        return false;
    };
    status_re.is_match(&normalized) && hint_re.is_match(&normalized)
}

fn extract_project_path(output: &str) -> Option<String> {
    let normalized = normalize_codegraph_output(output);

    if let Ok(uninit_re) = Regex::new(
        r"(?is)CodeGraph not initialized in (.*?)\.\s*Run ['`]codegraph init['`] in that project first\.",
    ) && let Some(captures) = uninit_re.captures(&normalized)
        && let Some(matched) = captures.get(1)
    {
        let trimmed = matched.as_str().trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if let Ok(not_indexed_re) =
        Regex::new(r"(?i)The project at (.+?) isn't indexed with codegraph\b")
        && let Some(captures) = not_indexed_re.captures(&normalized)
        && let Some(matched) = captures.get(1)
    {
        let trimmed = matched.as_str().trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if let Ok(no_project_re) = Regex::new(r"No CodeGraph project is loaded for this session\.")
        && no_project_re.is_match(&normalized)
        && let Ok(searched_re) =
            Regex::new(r"(?im)^Searched for a \.codegraph/ directory starting from:\s*(.+?)\s*$")
        && let Some(captures) = searched_re.captures(&normalized)
        && let Some(matched) = captures.get(1)
    {
        let trimmed = matched.as_str().trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if !looks_like_codegraph_uninitialized_output(&normalized) {
        return None;
    }

    if let Ok(status_re) = Regex::new(r"(?im)^.*?\bProject:\s*(.+?)\s*$")
        && let Some(captures) = status_re.captures(&normalized)
        && let Some(matched) = captures.get(1)
    {
        let trimmed = matched.as_str().trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
}

pub fn get_codegraph_uninitialized_project(input: &CodegraphInitGuidanceInput) -> Option<String> {
    if !is_codegraph_tool(input.tool_name.as_deref()) {
        return None;
    }

    if let Some(project_path) = extract_project_path(&input.tool_output) {
        return Some(project_path);
    }

    if !looks_like_codegraph_uninitialized_output(&input.tool_output) {
        return None;
    }

    input
        .cwd
        .as_ref()
        .map(|cwd| cwd.trim().to_string())
        .filter(|cwd| !cwd.is_empty())
}

pub fn build_codegraph_init_guidance(
    project_path: &str,
    options: &CodegraphInitGuidanceOptions,
) -> String {
    let resolved = resolve_codegraph_workspace_paths(
        Path::new(project_path),
        &ResolveCodegraphWorkspacePathsOptions {
            home_dir: options.home_dir.clone(),
        },
    );
    let display_project_path = serde_json::to_string(project_path).unwrap_or_default();
    let display_project_link =
        serde_json::to_string(&resolved.project_link.to_string_lossy()).unwrap_or_default();
    let display_data_dir =
        serde_json::to_string(&resolved.data_dir.to_string_lossy()).unwrap_or_default();
    let display_data_root =
        serde_json::to_string(&resolved.data_root.to_string_lossy()).unwrap_or_default();

    [
        "OMO CodeGraph initialization guidance:".to_string(),
        String::new(),
        format!("CodeGraph is not initialized for {display_project_path}. Initialize it through OMO's global local store instead of leaving a standalone project-local index."),
        String::new(),
        format!("- Link or create {display_project_link} so it points at {display_data_dir} under the OMO store {display_data_root}."),
        format!("- Then run `codegraph init` from {display_project_path} and retry the CodeGraph tool."),
        "- OMO's CodeGraph bootstrap does this automatically on session start; if bootstrap just ran, wait for it to finish and retry.".to_string(),
    ].join("\n")
}

pub fn build_codegraph_init_guidance_for_tool_result(
    input: &CodegraphInitGuidanceInput,
    options: &CodegraphInitGuidanceOptions,
) -> Option<String> {
    let project_path = get_codegraph_uninitialized_project(input)?;
    Some(build_codegraph_init_guidance(&project_path, options))
}
