//! `SendOutcome` -> `ContinueResult` projection (`manager/continue-result.ts`).

use crate::state::{DeliverAs, TaskStatus};
use crate::steering::SendOutcome;

pub const CONTINUE_SUGGESTION: &str = "Use task_output to read the final result.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinueDelivery {
    Steer,
    FollowUp,
    Revive,
    Held,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinueResult {
    Continued {
        task_id: String,
        status: TaskStatus,
        delivered: ContinueDelivery,
    },
    NotContinuable {
        task_id: Option<String>,
        reason: String,
        suggestion: String,
    },
}



pub fn to_continue_result(outcome: SendOutcome) -> ContinueResult {
    match outcome {
        SendOutcome::Stale {
            task_id, reason, ..
        } => ContinueResult::NotContinuable {
            task_id: Some(task_id),
            reason,
            suggestion: CONTINUE_SUGGESTION.into(),
        },
        SendOutcome::Steered {
            task_id,
            status,
            delivered,
        } => ContinueResult::Continued {
            task_id,
            status,
            delivered: match delivered {
                DeliverAs::Steer => ContinueDelivery::Steer,
                DeliverAs::FollowUp => ContinueDelivery::FollowUp,
            },
        },
        SendOutcome::Revived { task_id, .. } => ContinueResult::Continued {
            task_id,
            status: TaskStatus::Running,
            delivered: ContinueDelivery::Revive,
        },
        SendOutcome::Held { task_id, .. } => ContinueResult::Continued {
            task_id,
            status: TaskStatus::Paused,
            delivered: ContinueDelivery::FollowUp,
        },
        SendOutcome::Queued { task_id, .. } => ContinueResult::Continued {
            task_id,
            status: TaskStatus::Pending,
            delivered: ContinueDelivery::FollowUp,
        },
        SendOutcome::NotContinuable {
            task_id,
            reason,
            suggestion,
        } => ContinueResult::NotContinuable {
            task_id: Some(task_id),
            reason,
            suggestion,
        },
        SendOutcome::OneShotAgent {
            task_id, message, ..
        } => ContinueResult::NotContinuable {
            task_id: Some(task_id),
            reason: message,
            suggestion: CONTINUE_SUGGESTION.to_string(),
        },
        SendOutcome::ScopeDenied {
            task_id, reason, ..
        } => ContinueResult::NotContinuable {
            task_id: Some(task_id),
            reason,
            suggestion: CONTINUE_SUGGESTION.to_string(),
        },
        SendOutcome::NotFound { reason, suggestion } => ContinueResult::NotContinuable {
            task_id: None,
            reason,
            suggestion,
        },
    }
}

#[cfg(test)]
mod held_tests {
    use super::*;
    #[test]
    fn held_send_projects_to_paused_follow_up() {
        assert_eq!(
            to_continue_result(SendOutcome::Held {
                task_id: "task".into(),
                pause_seq: 3,
                entry_id: "input".into()
            }),
            ContinueResult::Continued {
                task_id: "task".into(),
                status: TaskStatus::Paused,
                delivered: ContinueDelivery::FollowUp
            }
        );
    }
}
