//! Port of TS `workspace-edit-snapshot.ts`.

use super::path::file_digest;
use super::path::read_dir_sorted;
use super::path::snapshot_path;
use super::types::ParsedWorkspaceOperation;
use super::types::WorkspaceSnapshotEntry;
use crate::request_context::dirname;
use crate::request_context::resolve_from;
use indexmap::IndexMap;
use std::path::Path;

struct Builder<'a> {
    workspace_root: &'a str,
    snapshots: IndexMap<String, WorkspaceSnapshotEntry>,
    /// Text-edit targets; simulation reads their content.
    text_targets: Vec<&'a str>,
    /// Rename sources; when any text edit exists, files under them keep content too,
    /// since a later edit may target the moved file.
    rename_sources: Vec<&'a str>,
}

impl Builder<'_> {
    fn needs_content(&self, path: &str) -> bool {
        self.text_targets.contains(&path)
            || (!self.text_targets.is_empty()
                && self
                    .rename_sources
                    .iter()
                    .any(|source| Path::new(path).starts_with(source)))
    }

    fn add(&mut self, path: &str, include_children: bool) -> Result<(), String> {
        let mut candidate = path.to_string();
        loop {
            let refresh = match self.snapshots.get(&candidate) {
                None => true,
                Some(WorkspaceSnapshotEntry::Directory { children: None }) => include_children,
                Some(_) => false,
            };
            if refresh {
                let mut entry = snapshot_path(&candidate, include_children && candidate == path)?;
                let digest = match &entry {
                    WorkspaceSnapshotEntry::File { content } if !self.needs_content(&candidate) => {
                        Some(file_digest(content))
                    }
                    _ => None,
                };
                if let Some(digest) = digest {
                    entry = WorkspaceSnapshotEntry::FileDigest { digest };
                }
                self.snapshots.insert(candidate.clone(), entry);
            }
            if candidate == self.workspace_root {
                break;
            }
            let parent = dirname(&candidate);
            if parent == candidate {
                break;
            }
            candidate = parent;
        }
        let is_dir = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir());
        if !include_children || !Path::new(path).exists() || !is_dir {
            return Ok(());
        }
        for child in read_dir_sorted(path).map_err(|error| error.to_string())? {
            self.add(&resolve_from(path, &child), true)?;
        }
        Ok(())
    }
}

/// TS `snapshotOperations`.
pub fn snapshot_operations(
    operations: &[ParsedWorkspaceOperation],
    workspace_root: &str,
) -> Result<IndexMap<String, WorkspaceSnapshotEntry>, String> {
    let mut builder = Builder {
        workspace_root,
        snapshots: IndexMap::new(),
        text_targets: operations
            .iter()
            .filter_map(|operation| match operation {
                ParsedWorkspaceOperation::Text { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect(),
        rename_sources: operations
            .iter()
            .filter_map(|operation| match operation {
                ParsedWorkspaceOperation::Rename { old_path, .. } => Some(old_path.as_str()),
                _ => None,
            })
            .collect(),
    };
    builder.add(workspace_root, false)?;
    for operation in operations {
        match operation {
            ParsedWorkspaceOperation::Rename {
                old_path, new_path, ..
            } => {
                builder.add(old_path, true)?;
                builder.add(new_path, true)?;
            }
            ParsedWorkspaceOperation::Delete { path, .. } => builder.add(path, true)?,
            ParsedWorkspaceOperation::Text { path, .. }
            | ParsedWorkspaceOperation::Create { path, .. } => {
                builder.add(path, false)?;
            }
        }
    }
    Ok(builder.snapshots)
}
