//! Task isolation records (state/types.ts TaskIsolationSpec, IsolationRecord, IsolationMergeResult).
//!
//! TaskIsolationSpec/IsolationMergeResult are the TASK-LEVEL persisted shapes; the core
//! merge result lives in maho-isolation-core and is projected into IsolationMergeResult by
//! isolation::settle::project, exactly like the TypeScript project() helper.

use serde::{Deserialize, Serialize};

use isolation_core::BackendKind;

/// The core merge-kind union, re-exported as the task-level IsolationMergeKind (state/types.ts).
pub use isolation_core::MergeKind as IsolationMergeKind;

/// Merge strategy for an isolated child (TS mode: patch | branch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMergeMode {
    Patch,
    Branch,
}

/// The producer's `MergeMode` carries no serde derives, so the persisted record uses the serde-able
/// mirror above and converts at the merge call.
impl From<IsolationMergeMode> for isolation_core::MergeMode {
    fn from(mode: IsolationMergeMode) -> Self {
        match mode {
            IsolationMergeMode::Patch => isolation_core::MergeMode::Patch,
            IsolationMergeMode::Branch => isolation_core::MergeMode::Branch,
        }
    }
}

/// The spec of one isolated child (TS TaskIsolationSpec). Field names stay snake_case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskIsolationSpec {
    pub backend: BackendKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fell_back: Option<bool>,
    pub merged_dir: String,
    pub base_dir: String,
    pub mode: IsolationMergeMode,
    pub apply: bool,
}

/// The persisted isolation record: the spec plus its eventual merge result (TS IsolationRecord).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IsolationRecord {
    #[serde(flatten)]
    pub spec: TaskIsolationSpec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_result: Option<IsolationMergeResult>,
}

impl IsolationRecord {
    pub fn new(spec: TaskIsolationSpec) -> Self {
        Self {
            spec,
            merge_result: None,
        }
    }
}

/// The task-level merge result (TS IsolationMergeResult). camelCase keys except duration_ms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IsolationMergeResult {
    pub kind: IsolationMergeKind,
    #[serde(rename = "changesApplied")]
    pub changes_applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(rename = "patchPath", skip_serializing_if = "Option::is_none")]
    pub patch_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(rename = "summaryPath", skip_serializing_if = "Option::is_none")]
    pub summary_path: Option<String>,
    #[serde(rename = "filesChanged", skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<usize>,
    #[serde(rename = "nestedPatchPaths", skip_serializing_if = "Option::is_none")]
    pub nested_patch_paths: Option<Vec<String>>,
    #[serde(rename = "branchName", skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
    #[serde(rename = "manualCommand", skip_serializing_if = "Option::is_none")]
    pub manual_command: Option<String>,
}

impl IsolationMergeResult {
    /// A retained result with the given error text and optional reason.
    pub fn retained(error: Option<String>, reason: Option<String>, duration_ms: u64) -> Self {
        Self {
            kind: IsolationMergeKind::Retained,
            changes_applied: false,
            duration_ms: Some(duration_ms),
            patch_path: None,
            error,
            reason,
            summary_path: None,
            files_changed: None,
            nested_patch_paths: None,
            branch_name: None,
            partial: None,
            conflict: None,
            manual_command: None,
        }
    }
}
