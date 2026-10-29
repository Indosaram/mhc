//! Port of TS `outside-context-workspace.ts`.

use std::path::PathBuf;

use crate::lsp::workspace_markers::WORKSPACE_MARKERS;

pub fn find_workspace_root_outside_context(directory: &str) -> String {
    if let Some(marked) = nearest_marked_ancestor(directory) {
        return marked;
    }
    let fallback = nearest_existing_ancestor(directory);
    std::fs::canonicalize(&fallback)
        .map(|p| crate::request_context::strip_verbatim_prefix(&p.to_string_lossy()))
        .unwrap_or(fallback)
}

pub fn nearest_marked_ancestor(directory: &str) -> Option<String> {
    let mut current = PathBuf::from(directory);
    loop {
        if current.exists() && WORKSPACE_MARKERS.iter().any(|m| current.join(m).exists()) {
            return std::fs::canonicalize(&current)
                .ok()
                .map(|p| crate::request_context::strip_verbatim_prefix(&p.to_string_lossy()));
        }
        if !current.pop() {
            return None;
        }
    }
}

fn nearest_existing_ancestor(directory: &str) -> String {
    let mut current = PathBuf::from(directory);
    while !current.exists() {
        if !current.pop() {
            return directory.to_string();
        }
    }
    current.to_string_lossy().into_owned()
}
