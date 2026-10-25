//! Project-scoped recall (pin `recall/project-corpus.ts`). A child project's memory joins a
//! session's recall under a qualified path, `@project/<identity-id>/<path>`: still a plain relative
//! path for every path check (no leading slash, no `..`), never equal to a path of the session's own
//! memory, and unique per identity, so ledgers and surfaced sets keep one key per note across
//! identities.

use super::provider::RecallDocument;

/// Namespace every copied project document's path carries.
pub const PROJECT_MEMORY_PATH_PREFIX: &str = "@project/";

/// Copies `identity_id`'s documents under its namespace, keeping each description and body.
pub fn qualify_project_documents(
    identity_id: &str,
    documents: &[RecallDocument],
) -> Vec<RecallDocument> {
    documents
        .iter()
        .map(|document| RecallDocument {
            path: format!(
                "{PROJECT_MEMORY_PATH_PREFIX}{identity_id}/{}",
                document.path
            ),
            description: document.description.clone(),
            body: document.body.clone(),
        })
        .collect()
}

/// One qualified project path: the identity that owns the note and the note's own relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMemoryPath {
    pub identity_id: String,
    pub path: String,
}

/// Splits a qualified project path; a path outside the namespace, or one missing its identity or
/// its note path, is not a project path.
pub fn parse_project_memory_path(path: &str) -> Option<ProjectMemoryPath> {
    let rest = path.strip_prefix(PROJECT_MEMORY_PATH_PREFIX)?;
    let slash = rest.find('/')?;
    if slash == 0 || slash == rest.len() - 1 {
        return None;
    }
    Some(ProjectMemoryPath {
        identity_id: rest[..slash].to_string(),
        path: rest[slash + 1..].to_string(),
    })
}

#[cfg(test)]
#[path = "project_corpus_tests.rs"]
mod tests;
