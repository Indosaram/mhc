//! `isolation/details.ts`: every result builder projects the SAME persisted facts.

use serde::Serialize;

use crate::state::{IsolationMergeKind, IsolationRecord, TaskRecord};

/// `IsolationDetails`: the snake_case projection the task_output/tool surfaces read.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IsolationDetails {
    pub backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fell_back: Option<bool>,
    pub changes_applied: bool,
    pub kind: IsolationMergeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nested_patch_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `IsolationStartedDetails`: what a launch announces before any merge result exists.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IsolationStartedDetails {
    pub backend: String,
    pub merged_dir: String,
}

/// `isolationDetails(record)`: `None` until a merge result was recorded.
pub fn isolation_details(record: &TaskRecord) -> Option<IsolationDetails> {
    let isolation = record.isolation.as_ref()?;
    let merge = isolation.merge_result.as_ref()?;
    Some(IsolationDetails {
        backend: isolation.spec.backend.as_str().to_string(),
        fell_back: isolation.spec.fell_back,
        changes_applied: merge.changes_applied,
        kind: merge.kind,
        partial: merge.partial,
        patch_path: merge.patch_path.clone(),
        nested_patch_paths: merge.nested_patch_paths.clone(),
        branch_name: merge.branch_name.clone(),
        summary_path: merge.summary_path.clone(),
        files_changed: merge.files_changed,
        conflict: merge.conflict.clone(),
        manual_command: merge.manual_command.clone(),
        reason: merge.reason.clone(),
    })
}

/// `isolationLine(details)`: `isolation: <kind> via <backend>[ -> <patch>]`.
pub fn isolation_line(details: &IsolationDetails) -> String {
    let target = details
        .patch_path
        .as_ref()
        .map_or(String::new(), |path| format!(" -> {path}"));
    format!("isolation: {} via {}{target}", details.kind.as_str(), details.backend)
}

/// The `IsolationRecord` of a record, when one exists.
pub fn isolation_record(record: &TaskRecord) -> Option<&IsolationRecord> {
    record.isolation.as_ref()
}
