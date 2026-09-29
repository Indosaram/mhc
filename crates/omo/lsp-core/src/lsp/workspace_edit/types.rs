use crate::abort::AbortSignal;
use indexmap::IndexMap;
use serde::Serialize;
use std::collections::HashMap;

/// TS `ApplyResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub success: bool,
    pub files_modified: Vec<String>,
    pub total_edits: usize,
    pub errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_change: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub late_abort: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Directory,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }
}

/// TS `WorkspaceSnapshotEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceSnapshotEntry {
    Missing,
    File { content: String },
    Directory { children: Option<Vec<String>> },
}

impl WorkspaceSnapshotEntry {
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    pub fn entry_kind(&self) -> Option<EntryKind> {
        match self {
            Self::Missing => None,
            Self::File { .. } => Some(EntryKind::File),
            Self::Directory { .. } => Some(EntryKind::Directory),
        }
    }
}

/// TS `PlannedWorkspaceOperation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedWorkspaceOperation {
    Text {
        change_index: usize,
        path: String,
        before_text: String,
        after_text: String,
        edit_count: usize,
        document_version: Option<i64>,
    },
    Create {
        change_index: usize,
        path: String,
        replaced: bool,
    },
    Rename {
        change_index: usize,
        old_path: String,
        new_path: String,
        source_kind: EntryKind,
        replace_destination: bool,
    },
    Delete {
        change_index: usize,
        path: String,
        target_kind: EntryKind,
        recursive: bool,
    },
    Noop {
        change_index: usize,
    },
}

impl PlannedWorkspaceOperation {
    pub fn change_index(&self) -> usize {
        match self {
            Self::Text { change_index, .. }
            | Self::Create { change_index, .. }
            | Self::Rename { change_index, .. }
            | Self::Delete { change_index, .. }
            | Self::Noop { change_index } => *change_index,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Create { .. } => "create",
            Self::Rename { .. } => "rename",
            Self::Delete { .. } => "delete",
            Self::Noop { .. } => "noop",
        }
    }
}

/// TS `WorkspaceEditPlan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEditPlan {
    pub workspace_root: String,
    pub operations: Vec<PlannedWorkspaceOperation>,
    pub snapshots: IndexMap<String, WorkspaceSnapshotEntry>,
    pub first_change_by_path: HashMap<String, usize>,
    pub reported_path_by_canonical: HashMap<String, String>,
    pub fingerprint: String,
}

/// TS `WorkspaceEditPlanResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceEditPlanResult {
    Success(Box<WorkspaceEditPlan>),
    Failure(ApplyResult),
}

impl WorkspaceEditPlanResult {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success(_))
    }
}

/// TS `WorkspaceMutation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WorkspaceMutation {
    #[serde(rename_all = "camelCase")]
    Text {
        path: String,
        before_text: String,
        after_text: String,
    },
    Create {
        path: String,
        replaced: bool,
    },
    #[serde(rename_all = "camelCase")]
    Rename {
        old_path: String,
        new_path: String,
        source_kind: EntryKind,
    },
    #[serde(rename_all = "camelCase")]
    Delete {
        path: String,
        target_kind: EntryKind,
    },
}

impl WorkspaceMutation {
    pub fn changed_paths(&self) -> Vec<&str> {
        match self {
            Self::Rename {
                old_path, new_path, ..
            } => vec![old_path.as_str(), new_path.as_str()],
            Self::Text { path, .. } | Self::Create { path, .. } | Self::Delete { path, .. } => {
                vec![path.as_str()]
            }
        }
    }
}

/// TS `WorkspaceMutationDelta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMutationDelta {
    pub operations: Vec<WorkspaceMutation>,
    pub changed_paths: Vec<String>,
}

/// TS `WorkspaceEditCommit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEditCommit {
    pub result: ApplyResult,
    pub delta: WorkspaceMutationDelta,
    pub fingerprint: Option<String>,
}

pub type WriteFileFn = Box<dyn FnMut(&str, &str) -> Result<(), String> + Send>;
pub type RenameFn = Box<dyn FnMut(&str, &str) -> Result<(), String> + Send>;
pub type RemoveFn = Box<dyn FnMut(&str, bool) -> Result<(), String> + Send>;

/// TS `Partial<WorkspaceEditCommitIo>`: any unset hook uses the real filesystem.
#[derive(Default)]
pub struct WorkspaceEditCommitIo {
    pub write_file: Option<WriteFileFn>,
    pub rename: Option<RenameFn>,
    pub remove: Option<RemoveFn>,
}

impl std::fmt::Debug for WorkspaceEditCommitIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceEditCommitIo")
            .field("write_file", &self.write_file.is_some())
            .field("rename", &self.rename.is_some())
            .field("remove", &self.remove.is_some())
            .finish()
    }
}

/// TS `ApplyWorkspaceEditOptions`.
#[derive(Debug, Default)]
pub struct ApplyWorkspaceEditOptions {
    pub workspace_root: Option<String>,
    pub signal: Option<AbortSignal>,
    pub io: WorkspaceEditCommitIo,
}

/// TS `WorkspaceEditValidationError` (message `change N: detail`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("change {change_index}: {detail}")]
pub struct WorkspaceEditValidationError {
    pub change_index: usize,
    pub detail: String,
}

impl WorkspaceEditValidationError {
    pub fn new(change_index: usize, detail: impl Into<String>) -> Self {
        Self {
            change_index,
            detail: detail.into(),
        }
    }
}

/// Parsed text edit; positions stay as JSON numbers until range validation (TS semantics).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedTextEdit {
    pub start_line: f64,
    pub start_character: f64,
    pub end_line: f64,
    pub end_character: f64,
    pub new_text: String,
}

/// TS `ParsedWorkspaceOperation`.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedWorkspaceOperation {
    Text {
        change_index: usize,
        path: String,
        reported_path: String,
        edits: Vec<ParsedTextEdit>,
        version: Option<i64>,
    },
    Create {
        change_index: usize,
        path: String,
        reported_path: String,
        overwrite: bool,
        ignore_if_exists: bool,
        followed_symbolic_link: bool,
    },
    Rename {
        change_index: usize,
        old_path: String,
        new_path: String,
        reported_old_path: String,
        reported_new_path: String,
        overwrite: bool,
        ignore_if_exists: bool,
        followed_symbolic_link: bool,
    },
    Delete {
        change_index: usize,
        path: String,
        reported_path: String,
        recursive: bool,
        ignore_if_not_exists: bool,
        followed_symbolic_link: bool,
    },
}

impl ParsedWorkspaceOperation {
    pub fn change_index(&self) -> usize {
        match self {
            Self::Text { change_index, .. }
            | Self::Create { change_index, .. }
            | Self::Rename { change_index, .. }
            | Self::Delete { change_index, .. } => *change_index,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseFailure {
    pub change_index: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedWorkspaceEdit {
    pub operations: Vec<ParsedWorkspaceOperation>,
    pub failures: Vec<ParseFailure>,
}

/// TS `WorkspaceEditFingerprintResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceEditFingerprintResult {
    Success(String),
    Failure(ApplyResult),
}
