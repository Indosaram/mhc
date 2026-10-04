//! Status mutation rules from goal/transitions.ts.
use crate::{errors::GoalError, types::{Goal, GoalStatus, GoalUpdateSource}};
pub fn transition_goal_status(current: &Goal, status: GoalStatus, source: GoalUpdateSource, reason: Option<&str>, updated_at: u64) -> Result<Goal, GoalError> {
    let allowed = current.status == status || match source {
        GoalUpdateSource::Model => matches!((current.status, status), (GoalStatus::Active, GoalStatus::Blocked | GoalStatus::Complete) | (GoalStatus::Blocked, GoalStatus::Complete)),
        GoalUpdateSource::User => matches!((current.status, status), (GoalStatus::Active, GoalStatus::Paused) | (GoalStatus::Paused | GoalStatus::Blocked | GoalStatus::Complete, GoalStatus::Active)),
    };
    if !allowed { return Err(GoalError::InvalidMutation(format!("illegal goal transition: {} -> {status}", current.status))); }
    if status == GoalStatus::Complete && reason.is_some() { return Err(GoalError::InvalidMutation("reason must not be provided when status is complete".into())); }
    let mut next = current.clone();
    next.status = status;
    next.updated_at = updated_at;
    if status == GoalStatus::Blocked {
        if current.status != GoalStatus::Blocked {
            let blocked_reason = reason.map(|r| r.trim_matches(crate::validation::js_whitespace)).filter(|r| !r.is_empty()).ok_or_else(|| GoalError::InvalidMutation("reason is required when status is blocked".into()))?;
            next.blocked_reason = Some(blocked_reason.into()); next.blocked_at = Some(updated_at);
        }
    } else { next.blocked_reason = None; next.blocked_at = None; }
    if status == GoalStatus::Active && current.status != GoalStatus::Active { next.last_started_at = Some(updated_at); }
    else if status != GoalStatus::Active { next.last_started_at = None; }
    next.completed_at = if status == GoalStatus::Complete { Some(current.completed_at.unwrap_or(updated_at)) } else { None };
    Ok(next)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn goal() -> Goal { Goal { id: "g".into(), thread_id: "t".into(), objective: "work".into(), status: GoalStatus::Active, token_budget: None, tokens_used: 0, time_used_seconds: 0.0, consecutive_continuations: None, unattended_continuations: None, last_continuation_signature: None, created_at: 1, updated_at: 1, last_started_at: Some(1), blocked_reason: None, blocked_at: None, completed_at: None } }
    #[test] fn model_block_requires_reason() { let current = goal(); let result = transition_goal_status(&current, GoalStatus::Blocked, GoalUpdateSource::Model, None, 2); assert!(result.is_err()); }
    #[test] fn model_cannot_pause() { let current = goal(); let result = transition_goal_status(&current, GoalStatus::Paused, GoalUpdateSource::Model, None, 2); assert!(result.is_err()); }
    #[test] fn complete_rejects_reason() { let current = goal(); let result = transition_goal_status(&current, GoalStatus::Complete, GoalUpdateSource::Model, Some("done"), 2); assert!(result.is_err()); }
    #[test] fn blocked_repeat_preserves_original_reason_and_time() { let current = transition_goal_status(&goal(), GoalStatus::Blocked, GoalUpdateSource::Model, Some(" wait "), 2).unwrap(); let result = transition_goal_status(&current, GoalStatus::Blocked, GoalUpdateSource::Model, Some("other"), 3).unwrap(); assert_eq!(result.blocked_reason.as_deref(), Some("wait")); assert_eq!(result.blocked_at, Some(2)); }
    #[test] fn user_resume_clears_block_and_opens_accounting() { let current = transition_goal_status(&goal(), GoalStatus::Blocked, GoalUpdateSource::Model, Some("wait"), 2).unwrap(); let result = transition_goal_status(&current, GoalStatus::Active, GoalUpdateSource::User, None, 3).unwrap(); assert_eq!(result.last_started_at, Some(3)); assert_eq!(result.blocked_reason, None); }
    #[test] fn completion_repeat_keeps_original_time() { let current = transition_goal_status(&goal(), GoalStatus::Complete, GoalUpdateSource::Model, None, 2).unwrap(); let result = transition_goal_status(&current, GoalStatus::Complete, GoalUpdateSource::Model, None, 3).unwrap(); assert_eq!(result.completed_at, Some(2)); }
}
