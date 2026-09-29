//! Port of TS `workspace-edit-commit.ts` and `workspace-edit.ts`.

use super::path::snapshot_path;
use super::plan::plan_workspace_edit;
use super::types::ApplyResult;
use super::types::ApplyWorkspaceEditOptions;
use super::types::EntryKind;
use super::types::PlannedWorkspaceOperation as Planned;
use super::types::WorkspaceEditCommit;
use super::types::WorkspaceEditCommitIo;
use super::types::WorkspaceEditPlan;
use super::types::WorkspaceEditPlanResult;
use super::types::WorkspaceMutation;
use super::types::WorkspaceMutationDelta;
use super::types::WorkspaceSnapshotEntry as Entry;
use crate::abort::AbortSignal;
use crate::request_context::context_cwd;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

fn default_write(path: &str, content: &str) -> Result<(), String> {
    std::fs::write(path, content).map_err(|error| error.to_string())
}

fn default_rename(old_path: &str, new_path: &str) -> Result<(), String> {
    std::fs::rename(old_path, new_path).map_err(|error| error.to_string())
}

fn default_remove(path: &str, recursive: bool) -> Result<(), String> {
    let is_dir = std::fs::symlink_metadata(path)
        .map_err(|error| error.to_string())?
        .is_dir();
    let result = match (is_dir, recursive) {
        (true, true) => std::fs::remove_dir_all(path),
        (true, false) => std::fs::remove_dir(path),
        (false, _) => std::fs::remove_file(path),
    };
    result.map_err(|error| error.to_string())
}

fn snapshots_equal(expected: &Entry, actual: &Entry) -> bool {
    match (expected, actual) {
        (Entry::File { content: left }, Entry::File { content: right }) => left == right,
        (
            Entry::Directory {
                children: Some(left),
            },
            Entry::Directory { children: right },
        ) => right.as_ref() == Some(left),
        _ => std::mem::discriminant(expected) == std::mem::discriminant(actual),
    }
}

fn first_operation_index(plan: &WorkspaceEditPlan) -> usize {
    plan.operations.first().map_or(0, Planned::change_index)
}

#[derive(Default)]
struct Accumulator {
    mutations: Vec<WorkspaceMutation>,
    files_modified: Vec<String>,
    total_edits: usize,
}

fn mutation_delta(operations: Vec<WorkspaceMutation>) -> WorkspaceMutationDelta {
    let changed: BTreeSet<String> = operations
        .iter()
        .flat_map(|operation| operation.changed_paths().into_iter().map(str::to_string))
        .collect();
    WorkspaceMutationDelta {
        operations,
        changed_paths: changed.into_iter().collect(),
    }
}

fn failed_commit(
    plan: &WorkspaceEditPlan,
    message: String,
    change_index: usize,
    accumulator: Accumulator,
    late_abort: bool,
) -> WorkspaceEditCommit {
    WorkspaceEditCommit {
        result: ApplyResult {
            success: false,
            files_modified: accumulator.files_modified,
            total_edits: accumulator.total_edits,
            errors: vec![format!("change {change_index}: {message}")],
            failed_change: Some(change_index),
            late_abort: late_abort.then_some(true),
        },
        delta: mutation_delta(accumulator.mutations),
        fingerprint: Some(plan.fingerprint.clone()),
    }
}

fn verify_snapshots(plan: &WorkspaceEditPlan) -> Option<WorkspaceEditCommit> {
    for (path, expected) in &plan.snapshots {
        let change_index = plan
            .first_change_by_path
            .get(path)
            .copied()
            .unwrap_or_else(|| first_operation_index(plan));
        let include_children = matches!(expected, Entry::Directory { children: Some(_) });
        match snapshot_path(path, include_children) {
            Err(detail) => {
                return Some(failed_commit(
                    plan,
                    format!("cannot verify snapshot for {path}: {detail}"),
                    change_index,
                    Accumulator::default(),
                    false,
                ));
            }
            Ok(actual) if !snapshots_equal(expected, &actual) => {
                return Some(failed_commit(
                    plan,
                    format!("workspace state changed before commit: {path}"),
                    change_index,
                    Accumulator::default(),
                    false,
                ));
            }
            Ok(_) => {}
        }
    }
    None
}

fn reported_path(plan: &WorkspaceEditPlan, path: &str) -> String {
    plan.reported_path_by_canonical
        .get(path)
        .cloned()
        .unwrap_or_else(|| path.to_string())
}

fn add_modified(accumulator: &mut Accumulator, path: String) {
    if !accumulator.files_modified.contains(&path) {
        accumulator.files_modified.push(path);
    }
}

fn commit_operation(
    plan: &WorkspaceEditPlan,
    io: &mut WorkspaceEditCommitIo,
    accumulator: &mut Accumulator,
    operation: &Planned,
) -> Result<(), String> {
    match operation {
        Planned::Noop { .. } => {}
        Planned::Text {
            path,
            before_text,
            after_text,
            edit_count,
            ..
        } => {
            match io.write_file.as_mut() {
                Some(write) => write(path, after_text)?,
                None => default_write(path, after_text)?,
            }
            accumulator.mutations.push(WorkspaceMutation::Text {
                path: path.clone(),
                before_text: before_text.clone(),
                after_text: after_text.clone(),
            });
            add_modified(accumulator, reported_path(plan, path));
            accumulator.total_edits += edit_count;
        }
        Planned::Create { path, replaced, .. } => {
            match io.write_file.as_mut() {
                Some(write) => write(path, "")?,
                None => default_write(path, "")?,
            }
            accumulator.mutations.push(WorkspaceMutation::Create {
                path: path.clone(),
                replaced: *replaced,
            });
            add_modified(accumulator, reported_path(plan, path));
        }
        Planned::Rename {
            old_path,
            new_path,
            source_kind,
            replace_destination,
            ..
        } => {
            if *replace_destination {
                let is_dir = Path::new(new_path).exists()
                    && std::fs::symlink_metadata(new_path).is_ok_and(|meta| meta.is_dir());
                let target_kind = if is_dir {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                };
                match io.remove.as_mut() {
                    Some(remove) => remove(new_path, is_dir)?,
                    None => default_remove(new_path, is_dir)?,
                }
                accumulator.mutations.push(WorkspaceMutation::Delete {
                    path: new_path.clone(),
                    target_kind,
                });
                add_modified(accumulator, reported_path(plan, new_path));
            }
            match io.rename.as_mut() {
                Some(rename) => rename(old_path, new_path)?,
                None => default_rename(old_path, new_path)?,
            }
            accumulator.mutations.push(WorkspaceMutation::Rename {
                old_path: old_path.clone(),
                new_path: new_path.clone(),
                source_kind: *source_kind,
            });
            add_modified(accumulator, reported_path(plan, new_path));
        }
        Planned::Delete {
            path,
            target_kind,
            recursive,
            ..
        } => {
            match io.remove.as_mut() {
                Some(remove) => remove(path, *recursive)?,
                None => default_remove(path, *recursive)?,
            }
            accumulator.mutations.push(WorkspaceMutation::Delete {
                path: path.clone(),
                target_kind: *target_kind,
            });
            add_modified(accumulator, reported_path(plan, path));
        }
    }
    Ok(())
}

fn aborted(signal: Option<&AbortSignal>) -> bool {
    signal.is_some_and(AbortSignal::aborted)
}

/// TS `commitWorkspaceEditPlan`: verify snapshots, then cross the commit barrier.
pub fn commit_workspace_edit_plan(
    plan: &WorkspaceEditPlan,
    signal: Option<&AbortSignal>,
    io: &mut WorkspaceEditCommitIo,
) -> WorkspaceEditCommit {
    let cancelled = |plan: &WorkspaceEditPlan| {
        failed_commit(
            plan,
            "cancelled before commit".to_string(),
            first_operation_index(plan),
            Accumulator::default(),
            false,
        )
    };
    if aborted(signal) {
        return cancelled(plan);
    }
    if let Some(stale) = verify_snapshots(plan) {
        return stale;
    }
    if aborted(signal) {
        return cancelled(plan);
    }
    let mut accumulator = Accumulator::default();
    let mut late_abort = false;
    for operation in &plan.operations {
        if let Err(detail) = commit_operation(plan, io, &mut accumulator, operation) {
            let late = late_abort || aborted(signal);
            return failed_commit(
                plan,
                format!("I/O failure during {}: {detail}", operation.kind()),
                operation.change_index(),
                accumulator,
                late,
            );
        }
        if aborted(signal) {
            late_abort = true;
        }
    }
    WorkspaceEditCommit {
        result: ApplyResult {
            success: true,
            files_modified: accumulator.files_modified,
            total_edits: accumulator.total_edits,
            errors: Vec::new(),
            failed_change: None,
            late_abort: late_abort.then_some(true),
        },
        delta: mutation_delta(accumulator.mutations),
        fingerprint: Some(plan.fingerprint.clone()),
    }
}

/// TS `applyWorkspaceEditDetailed`. `None` models a null/undefined edit.
pub fn apply_workspace_edit_detailed(
    edit: Option<&Value>,
    mut options: ApplyWorkspaceEditOptions,
) -> WorkspaceEditCommit {
    let Some(edit) = edit.filter(|edit| !edit.is_null()) else {
        return WorkspaceEditCommit {
            result: ApplyResult {
                success: false,
                errors: vec!["No edit provided".to_string()],
                ..ApplyResult::default()
            },
            delta: WorkspaceMutationDelta::default(),
            fingerprint: None,
        };
    };
    let workspace_root = match options.workspace_root {
        Some(root) => root,
        None => match context_cwd() {
            Ok(cwd) => cwd,
            Err(error) => {
                return WorkspaceEditCommit {
                    result: ApplyResult {
                        success: false,
                        errors: vec![error.to_string()],
                        ..ApplyResult::default()
                    },
                    delta: WorkspaceMutationDelta::default(),
                    fingerprint: None,
                };
            }
        },
    };
    match plan_workspace_edit(edit, &workspace_root) {
        WorkspaceEditPlanResult::Failure(result) => WorkspaceEditCommit {
            result,
            delta: WorkspaceMutationDelta::default(),
            fingerprint: None,
        },
        WorkspaceEditPlanResult::Success(plan) => {
            commit_workspace_edit_plan(&plan, options.signal.as_ref(), &mut options.io)
        }
    }
}

/// TS `applyWorkspaceEdit`.
pub fn apply_workspace_edit(
    edit: Option<&Value>,
    options: ApplyWorkspaceEditOptions,
) -> ApplyResult {
    apply_workspace_edit_detailed(edit, options).result
}

/// TS `workspaceEditFingerprint`.
pub fn workspace_edit_fingerprint(edit: &Value, workspace_root: &str) -> Option<String> {
    match super::plan::fingerprint_workspace_edit(edit, workspace_root) {
        super::types::WorkspaceEditFingerprintResult::Success(fingerprint) => Some(fingerprint),
        super::types::WorkspaceEditFingerprintResult::Failure(_) => None,
    }
}
