//! Project-scoped memory nudge (#10044).
//!
//! Nudges the model with project-scoped memory when touching files in child projects.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use memory_core::recall::RecallDocument;
use crate::context::MemoryIdentityContext;
use crate::project_memory_grants::{grant_project_memory_read, revoke_project_memory_reads};
use crate::project_recall_seats::{parse_project_memory_path, ProjectRecallCorpus};

pub const PROJECT_MEMORY_NUDGE_MAX_NOTES: usize = 30;
pub const PROJECT_MEMORY_RECALL_MAX_PROJECTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectMemorySwitches {
    pub nudge: bool,
    pub recall: bool,
}

impl Default for ProjectMemorySwitches {
    fn default() -> Self {
        Self {
            nudge: true,
            recall: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedProject {
    pub project_dir: PathBuf,
    pub identity_id: String,
    pub repo_path: PathBuf,
}

#[derive(Default)]
struct SessionScope {
    touched: Vec<TouchedProject>,
    touched_paths: BTreeMap<String, String>,
    nudged: BTreeSet<String>,
}

pub struct ProjectMemoryScope {
    resolve_context: Arc<dyn Fn(&str) -> Option<MemoryIdentityContext> + Send + Sync>,
    switches: Arc<dyn Fn() -> ProjectMemorySwitches + Send + Sync>,
    sessions: Mutex<BTreeMap<String, SessionScope>>,
}

impl ProjectMemoryScope {
    pub fn new(
        resolve_context: Arc<dyn Fn(&str) -> Option<MemoryIdentityContext> + Send + Sync>,
        switches: Arc<dyn Fn() -> ProjectMemorySwitches + Send + Sync>,
    ) -> Self {
        Self {
            resolve_context,
            switches,
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn touched_most_recent_first(&self, session_id: &str) -> Vec<TouchedProject> {
        let sessions = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.get(session_id).map(|s| s.touched.clone()).unwrap_or_default()
    }

    pub fn source_for(&self, session_id: &str, path: &str) -> Option<String> {
        let (identity_id, subpath) = parse_project_memory_path(path)?;
        let sessions = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let scope = sessions.get(session_id)?;
        let project = scope.touched.iter().find(|p| p.identity_id == identity_id)?;
        Some(project.repo_path.join(subpath).to_string_lossy().to_string())
    }

    pub fn record_touch(
        &self,
        session_id: &str,
        project_dir: PathBuf,
        identity_id: String,
        repo_path: PathBuf,
        touched_rel: Option<String>,
    ) {
        let mut sessions = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let scope = sessions.entry(session_id.to_string()).or_default();
        if let Some(idx) = scope.touched.iter().position(|p| p.identity_id == identity_id) {
            scope.touched.remove(idx);
        }
        scope.touched.insert(
            0,
            TouchedProject {
                project_dir,
                identity_id: identity_id.clone(),
                repo_path: repo_path.clone(),
            },
        );
        if let Some(rel) = touched_rel {
            scope.touched_paths.insert(identity_id, rel);
        }

        if let Some(ctx) = (self.resolve_context)(session_id) {
            grant_project_memory_read(&ctx, &repo_path);
        }
    }

    pub fn on_session_shutdown(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.remove(session_id);
        if let Some(ctx) = (self.resolve_context)(session_id) {
            revoke_project_memory_reads(&ctx);
        }
    }
}

pub fn format_project_memory_nudge(
    project_dir: &str,
    repo_path: &str,
    notes: &[RecallDocument],
) -> String {
    let limit = notes.len().min(PROJECT_MEMORY_NUDGE_MAX_NOTES);
    let shown = &notes[..limit];
    let mut lines = vec![
        String::new(),
        format!("[Project memory: {project_dir}]"),
        "This project keeps its own memory from earlier sessions. Consult it actively: read any note below that bears on the work before relying on assumptions.".to_string(),
        format!("Notes live under {repo_path}; open one with the read tool at that directory plus the path shown."),
    ];
    for note in shown {
        lines.push(format!("- {}: {}", note.path, note.description));
    }
    if notes.len() > limit {
        lines.push(format!("- +{} more", notes.len() - limit));
    }
    lines.join("\n")
}

pub fn with_project_documents(
    own: &[RecallDocument],
    projects: &[RecallDocument],
) -> Vec<RecallDocument> {
    if projects.is_empty() {
        return own.to_vec();
    }
    let mut merged = own.to_vec();
    merged.extend_from_slice(projects);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_project_memory_nudge() {
        let notes = vec![
            RecallDocument {
                path: "reference/deploy.md".to_string(),
                description: "how the child deploys".to_string(),
                body: "body".to_string(),
            },
        ];
        let nudge = format_project_memory_nudge("/workspace/child", "/memory/child/repo", &notes);
        assert!(nudge.contains("[Project memory: /workspace/child]"));
        assert!(nudge.contains("reference/deploy.md: how the child deploys"));
        assert!(nudge.contains("Notes live under /memory/child/repo"));
    }

    #[test]
    fn test_with_project_documents_merging() {
        let own = vec![RecallDocument {
            path: "own.md".to_string(),
            description: "own".to_string(),
            body: "own".to_string(),
        }];
        let proj = vec![RecallDocument {
            path: "@project/child/proj.md".to_string(),
            description: "proj".to_string(),
            body: "proj".to_string(),
        }];
        let merged = with_project_documents(&own, &proj);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].path, "own.md");
        assert_eq!(merged[1].path, "@project/child/proj.md");
    }
}
