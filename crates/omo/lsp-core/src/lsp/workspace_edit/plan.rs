//! Port of TS `workspace-edit-plan.ts`.

use super::fingerprint::canonical_fingerprint;
use super::parser::failure_result;
use super::parser::parse_workspace_edit;
use super::path::canonical_workspace_root;
use super::simulation::simulate_operations;
use super::snapshot::snapshot_operations;
use super::types::ParseFailure;
use super::types::ParsedWorkspaceOperation as Parsed;
use super::types::WorkspaceEditFingerprintResult;
use super::types::WorkspaceEditPlan;
use super::types::WorkspaceEditPlanResult;
use serde_json::Value;
use std::collections::HashMap;

fn root_failure(message: String) -> super::types::ApplyResult {
    failure_result(&[ParseFailure {
        change_index: 0,
        message,
    }])
}

/// TS `fingerprintWorkspaceEdit`.
pub fn fingerprint_workspace_edit(
    edit: &Value,
    workspace_root: &str,
) -> WorkspaceEditFingerprintResult {
    let root = match canonical_workspace_root(workspace_root) {
        Ok(root) => root,
        Err(error) => return WorkspaceEditFingerprintResult::Failure(root_failure(error)),
    };
    let parsed = parse_workspace_edit(edit, &root.path);
    if !parsed.failures.is_empty() {
        return WorkspaceEditFingerprintResult::Failure(failure_result(&parsed.failures));
    }
    WorkspaceEditFingerprintResult::Success(canonical_fingerprint(&parsed.operations))
}

/// TS `planWorkspaceEdit`: parse, snapshot, and simulate without touching the disk.
pub fn plan_workspace_edit(edit: &Value, workspace_root: &str) -> WorkspaceEditPlanResult {
    let root = match canonical_workspace_root(workspace_root) {
        Ok(root) => root,
        Err(error) => return WorkspaceEditPlanResult::Failure(root_failure(error)),
    };
    let parsed = parse_workspace_edit(edit, &root.path);
    if !parsed.failures.is_empty() {
        return WorkspaceEditPlanResult::Failure(failure_result(&parsed.failures));
    }
    let snapshots = match snapshot_operations(&parsed.operations, &root.path) {
        Ok(snapshots) => snapshots,
        Err(error) => return WorkspaceEditPlanResult::Failure(root_failure(error)),
    };
    let simulated = simulate_operations(&parsed.operations, &snapshots);
    if !simulated.failures.is_empty() {
        return WorkspaceEditPlanResult::Failure(failure_result(&simulated.failures));
    }
    let mut first_change_by_path = HashMap::new();
    let mut reported_path_by_canonical = HashMap::new();
    let mut add = |path: &str, reported: &str, change_index: usize| {
        first_change_by_path
            .entry(path.to_string())
            .or_insert(change_index);
        reported_path_by_canonical
            .entry(path.to_string())
            .or_insert_with(|| reported.to_string());
    };
    for operation in &parsed.operations {
        match operation {
            Parsed::Rename {
                change_index,
                old_path,
                new_path,
                reported_old_path,
                reported_new_path,
                ..
            } => {
                add(old_path, reported_old_path, *change_index);
                add(new_path, reported_new_path, *change_index);
            }
            Parsed::Text {
                change_index,
                path,
                reported_path,
                ..
            }
            | Parsed::Create {
                change_index,
                path,
                reported_path,
                ..
            }
            | Parsed::Delete {
                change_index,
                path,
                reported_path,
                ..
            } => add(path, reported_path, *change_index),
        }
    }
    WorkspaceEditPlanResult::Success(Box::new(WorkspaceEditPlan {
        workspace_root: root.path,
        operations: simulated.operations,
        snapshots,
        first_change_by_path,
        reported_path_by_canonical,
        fingerprint: canonical_fingerprint(&parsed.operations),
    }))
}
