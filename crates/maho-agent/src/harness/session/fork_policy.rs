//! Port of senpi packages/agent/src/harness/session/fork-policy.ts.

use super::commit::{CommittedListAppendWrite, CommittedValueSetWrite};
use super::session::{SessionError, SessionErrorKind, session_invariant_error};
use super::types::ForkOptions;

#[derive(Debug, Clone, PartialEq)]
pub enum ForkCurrentStatePlan {
    Branch {
        branch: String,
        destination_tip: Option<String>,
    },
    Tree,
}

/// One current-state row being projected into a fork destination.
#[derive(Debug, Clone, PartialEq)]
pub enum ForkCurrentStateRow {
    Value(CommittedValueSetWrite),
    List(CommittedListAppendWrite),
}

impl ForkCurrentStateRow {
    pub fn namespace(&self) -> &str {
        match self {
            ForkCurrentStateRow::Value(write) => &write.namespace,
            ForkCurrentStateRow::List(write) => &write.namespace,
        }
    }

    pub fn key(&self) -> &str {
        match self {
            ForkCurrentStateRow::Value(write) => &write.key,
            ForkCurrentStateRow::List(write) => &write.key,
        }
    }
}

fn quote(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
}

/// Source branch view used by branch-scope fork selection.
pub struct BranchForkSource<'a> {
    pub tip: Option<Option<String>>,
    pub get_parent: &'a dyn Fn(&str) -> Option<Option<String>>,
    pub select_entry: &'a dyn Fn(&str),
}

pub fn select_branch_fork(
    options: &ForkOptions,
    source: &BranchForkSource<'_>,
) -> Result<ForkCurrentStatePlan, SessionError> {
    let ForkOptions::Branch {
        branch,
        entry_id,
        position,
        ..
    } = options
    else {
        return Ok(ForkCurrentStatePlan::Tree);
    };
    let Some(tip) = &source.tip else {
        return Err(session_invariant_error(format!("Unknown source branch: {branch}")));
    };
    let requested = entry_id.clone().or_else(|| tip.clone());
    let mut found = requested.is_none();
    let mut destination_tip: Option<String> = None;
    let mut entry_id = tip.clone();
    while let Some(current) = entry_id {
        let parent_id = (source.get_parent)(&current).ok_or_else(|| {
            session_invariant_error(format!("Corrupt source branch: missing parent {current}"))
        })?;
        if Some(current.clone()) == requested {
            found = true;
            destination_tip = if *position == Some(super::types::ForkPosition::Before) {
                parent_id.clone()
            } else {
                Some(current.clone())
            };
            if *position != Some(super::types::ForkPosition::Before) {
                (source.select_entry)(&current);
            }
        } else if found {
            (source.select_entry)(&current);
        }
        entry_id = parent_id;
    }
    if !found {
        let requested = requested.unwrap_or_default();
        return Err(session_invariant_error(format!(
            "Fork entry {requested} is not on source branch {}",
            quote(branch)
        )));
    }
    Ok(ForkCurrentStatePlan::Branch {
        branch: branch.clone(),
        destination_tip,
    })
}

/// Project one current scalar row or surviving list element into destination state.
pub fn project_fork_current_state_write(
    write: &ForkCurrentStateRow,
    plan: &ForkCurrentStatePlan,
    is_entry_copied: &dyn Fn(&str) -> bool,
) -> Result<Option<ForkCurrentStateRow>, SessionError> {
    let namespace = write.namespace();
    let key = write.key();
    let branch_scope = matches!(plan, ForkCurrentStatePlan::Branch { .. });
    match namespace {
        "pi.session.name" => Ok(Some(write.clone())),
        "pi.entry.label" => Ok(if is_entry_copied(key) {
            Some(write.clone())
        } else {
            None
        }),
        "pi.branch.tip" => {
            if !branch_scope {
                return Ok(Some(write.clone()));
            }
            let ForkCurrentStatePlan::Branch {
                branch,
                destination_tip,
            } = plan
            else {
                unreachable!("branch scope checked above")
            };
            if key == branch {
                Ok(Some(with_value(
                    write,
                    destination_tip.clone().map_or(serde_json::Value::Null, serde_json::Value::String),
                )))
            } else {
                Ok(None)
            }
        }
        "pi.lane.config" => Ok(if !branch_scope || key == branch_name(plan) {
            Some(write.clone())
        } else {
            None
        }),
        "pi.lane.state" => {
            if !branch_scope || key == branch_name(plan) {
                Ok(Some(with_value(
                    write,
                    serde_json::json!({ "currentOperationId": null, "lastOperationId": null, "inbox": [] }),
                )))
            } else {
                Ok(None)
            }
        }
        "pi.result" => Ok(None),
        _ => {
            if namespace.starts_with("pi.op.") || namespace.starts_with("pi.pending.") {
                return Ok(None);
            }
            if namespace == "pi" || namespace.starts_with("pi.") {
                return Err(SessionError::new(
                    SessionErrorKind::Invariant,
                    format!("Unknown reserved fork namespace: {namespace}"),
                ));
            }
            Ok(if branch_scope { None } else { Some(write.clone()) })
        }
    }
}

fn branch_name(plan: &ForkCurrentStatePlan) -> &str {
    match plan {
        ForkCurrentStatePlan::Branch { branch, .. } => branch,
        ForkCurrentStatePlan::Tree => "",
    }
}

fn with_value(write: &ForkCurrentStateRow, value: serde_json::Value) -> ForkCurrentStateRow {
    match write {
        ForkCurrentStateRow::Value(inner) => {
            let mut inner = inner.clone();
            inner.value = value;
            ForkCurrentStateRow::Value(inner)
        }
        ForkCurrentStateRow::List(inner) => {
            let mut inner = inner.clone();
            inner.value = value;
            ForkCurrentStateRow::List(inner)
        }
    }
}
