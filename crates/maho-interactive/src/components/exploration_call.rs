//! Port of `components/exploration-call.ts`.
use std::path::Path;

use maho_tools::path_utils::resolve_to_cwd;
use serde_json::Value;

use super::tool_execution::{ToolExecutionComponent, ToolExecutionPresentation};
use crate::tools::renderers::read::{CompactKind, get_compact_read_classification};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExplorationAction {
    Read,
    Search,
    List,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplorationCall {
    pub action: ExplorationAction,
    pub label: String,
    pub pending: bool,
    pub failed: bool,
}

fn string_arg(args: &Value, keys: &[&str]) -> Option<String> {
    if !args.is_object() {
        return None;
    }
    for key in keys {
        if let Some(value) = args.get(*key).and_then(Value::as_str).filter(|value| !value.is_empty()) {
            return Some(value.to_owned());
        }
    }
    None
}

fn is_semantic_read(args: &Value, cwd: &str) -> bool {
    get_compact_read_classification(Some(args), cwd)
        .is_some_and(|classification| matches!(classification.kind, CompactKind::Skill | CompactKind::Memory))
}

pub fn exploration_call(component: &ToolExecutionComponent) -> Option<ExplorationCall> {
    let (state, tool_name, presentation) = component.presentation_snapshot();
    if presentation != ToolExecutionPresentation::Classic {
        return None;
    }
    if !matches!(tool_name.as_str(), "read" | "grep" | "find" | "ls") {
        return None;
    }
    if !component.uses_built_in_renderers() {
        return None;
    }
    let (_, _, cwd) = component.identity();
    let pending = state.is_partial;
    let failed = state.is_error;
    let path = string_arg(&state.args, &["file_path", "path"]);
    match tool_name.as_str() {
        "read" => {
            if is_semantic_read(&state.args, &cwd) {
                return None;
            }
            let label = path
                .as_deref()
                .and_then(|path| Path::new(path).file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            Some(ExplorationCall { action: ExplorationAction::Read, label, pending, failed })
        }
        "grep" => {
            let pattern = string_arg(&state.args, &["pattern"]).unwrap_or_default();
            let dir = path.map_or_else(String::new, |path| {
                maho_core::paths::format_path_relative_to_cwd_or_absolute(
                    &resolve_to_cwd(&path, Path::new(&cwd)).to_string_lossy(),
                    &cwd,
                )
            });
            let label = if dir.is_empty() { pattern } else { format!("{pattern} in {dir}") };
            Some(ExplorationCall { action: ExplorationAction::Search, label, pending, failed })
        }
        _ => {
            let label = match &path {
                Some(path) => Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| path.clone()),
                None => String::from("."),
            };
            Some(ExplorationCall { action: ExplorationAction::List, label, pending, failed })
        }
    }
}
