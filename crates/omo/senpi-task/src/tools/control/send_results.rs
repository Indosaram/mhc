//! Port of `tools/control/send-results.ts`.

use crate::tools::control::tool_result::tool_result;
use crate::tools::control::types::{
    ControlListScope, ControlTaskRecord, SendManager, SendOutcome, SendResultDetails, SendToolResult,
};

/// Raised when the manager reports a steered delivery that is not a steer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SendInvariantError {
    pub message: String,
}

pub fn invalid_arguments(reason: &str) -> SendToolResult {
    tool_result(
        reason,
        SendResultDetails::InvalidArguments {
            reason: reason.to_string(),
        },
    )
}

pub fn not_found(
    manager: &dyn SendManager,
    reason: &str,
    caller_session_id: Option<&str>,
) -> SendToolResult {
    let known = known_task_names(manager, caller_session_id);
    let list_text = if known.is_empty() {
        String::new()
    } else {
        format!(" Known tasks in this session: {}.", known.join(", "))
    };
    tool_result(
        &format!("{reason}{list_text}"),
        SendResultDetails::NotFound {
            reason: reason.to_string(),
            known_tasks: known,
        },
    )
}

fn known_task_names(manager: &dyn SendManager, caller_session_id: Option<&str>) -> Vec<String> {
    let scope = match caller_session_id {
        None => ControlListScope::All,
        Some(session_id) => ControlListScope::ParentSession {
            session_id: session_id.to_string(),
        },
    };
    manager
        .list(&scope)
        .into_iter()
        .map(|record| record.name.unwrap_or(record.task_id))
        .collect()
}

pub fn scope_denied(
    manager: &dyn SendManager,
    to: &str,
    caller_session_id: Option<&str>,
    all_scope: Option<bool>,
) -> Option<SendToolResult> {
    let caller_session_id = caller_session_id?;
    if all_scope == Some(true) {
        return None;
    }
    let record = resolve_listed_task(manager, to)?;
    if caller_session_id == record.parent_session_id || caller_session_id == record.root_session_id {
        return None;
    }
    let reason = format!(
        "Task {} belongs to session {}; pass all_scope to send across sessions.",
        record.task_id, record.parent_session_id
    );
    Some(tool_result(
        &reason,
        SendResultDetails::ScopeDenied {
            task_id: record.task_id,
            owning_session_id: record.parent_session_id,
            reason: reason.clone(),
        },
    ))
}

fn resolve_listed_task(manager: &dyn SendManager, to: &str) -> Option<ControlTaskRecord> {
    // PERF-DIVERGENCE(senpi tools/control/send-results.ts): upstream lists every record per send; we delegate to the manager's find_task, which loads a single record for an id. Same id-then-name result.
    manager.find_task(to)
}

pub fn map_send_outcome(outcome: SendOutcome) -> Result<SendToolResult, SendInvariantError> {
    Ok(match outcome {
        SendOutcome::Steered {
            task_id,
            status,
            delivered,
        } => {
            if delivered != "steer" {
                return Err(SendInvariantError {
                    message: format!(
                        "task_send invariant violated: expected steer delivery, received {delivered}"
                    ),
                });
            }
            tool_result(
                &format!("Delivered to {task_id} as {delivered}."),
                SendResultDetails::Steered {
                    task_id,
                    status,
                    delivered,
                },
            )
        }
        SendOutcome::Revived { task_id, run_epoch } => tool_result(
            &format!("Revived {task_id} (run epoch {run_epoch})."),
            SendResultDetails::Revived { task_id, run_epoch },
        ),
        SendOutcome::Stale {
            task_id,
            run_epoch,
            reason,
        } => tool_result(
            &reason,
            SendResultDetails::Stale {
                task_id,
                run_epoch,
                reason: reason.clone(),
            },
        ),
        SendOutcome::Held {
            task_id,
            pause_seq,
            entry_id,
        } => tool_result(
            &format!(
                "Held for {task_id}: its run is stopped, and the message is delivered when the run resumes."
            ),
            SendResultDetails::Held {
                task_id,
                pause_seq,
                entry_id,
            },
        ),
        SendOutcome::Queued {
            task_id,
            queue_position,
        } => tool_result(
            &format!("Queued for {task_id} at position {queue_position}."),
            SendResultDetails::Queued {
                task_id,
                queue_position,
            },
        ),
        SendOutcome::NotContinuable {
            task_id,
            reason,
            suggestion,
        } => tool_result(
            &format!("{reason} {suggestion}"),
            SendResultDetails::NotContinuable {
                task_id,
                reason,
                suggestion,
            },
        ),
        SendOutcome::OneShotAgent {
            task_id,
            agent,
            message,
        } => tool_result(
            &message,
            SendResultDetails::OneShotAgent {
                task_id,
                agent,
                message: message.clone(),
            },
        ),
        SendOutcome::ScopeDenied {
            task_id,
            owning_session_id,
            reason,
        } => tool_result(
            &reason,
            SendResultDetails::ScopeDenied {
                task_id,
                owning_session_id,
                reason: reason.clone(),
            },
        ),
        SendOutcome::NotFound { reason } => tool_result(
            &reason,
            SendResultDetails::NotFound {
                reason: reason.clone(),
                known_tasks: Vec::new(),
            },
        ),
    })
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    #[test]
    fn held_send_preserves_wire_identity() {
        let result = map_send_outcome(SendOutcome::Held {
            task_id: "task-one".into(),
            pause_seq: 4,
            entry_id: "entry-one".into(),
        })
        .expect("map held");
        let value = serde_json::to_value(&result.details).expect("serialize");
        assert_eq!(value["kind"], "held");
        assert_eq!(value["task_id"], "task-one");
        assert_eq!(value["pause_seq"], 4);
        assert_eq!(value["entry_id"], "entry-one");
    }
}
