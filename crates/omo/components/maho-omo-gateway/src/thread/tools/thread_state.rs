//! State & model vocabulary rules (`thread/tools/thread-state.ts`).

use std::collections::HashSet;

use serde_json::Value;

use super::super::contracts::results::{ThreadModel, ThreadRetryWait};
use super::super::contracts::state::{THREAD_STATES, ThreadActivity, ThreadState};

/// The one state rule (`#13403`, `#13407`).
pub fn thread_state(
    archived: Option<bool>,
    held: bool,
    activity: Option<ThreadActivity>,
) -> ThreadState {
    if archived == Some(true) {
        return ThreadState::Archived;
    }
    if !held {
        return ThreadState::Inactive;
    }
    match activity {
        Some(
            ThreadActivity::Running | ThreadActivity::Queued | ThreadActivity::AwaitingApproval,
        ) => ThreadState::Running,
        _ => ThreadState::Idle,
    }
}

#[derive(Debug, Clone, Default)]
pub struct ActivityDetail {
    pub activity: Option<ThreadActivity>,
    pub error_code: Option<String>,
    pub turn_id: Option<String>,
}

pub fn activity_detail(
    held: bool,
    activity: Option<ThreadActivity>,
    error_code: Option<&str>,
    turn_id: Option<&str>,
) -> ActivityDetail {
    if !held {
        return ActivityDetail::default();
    }
    let Some(act) = activity else {
        return ActivityDetail::default();
    };
    ActivityDetail {
        activity: Some(act),
        error_code: (act == ThreadActivity::Errored)
            .then_some(error_code)
            .flatten()
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        turn_id: turn_id.filter(|s| !s.is_empty()).map(str::to_owned),
    }
}

pub fn retry_wait(running: bool, waiting: Option<&Value>) -> Option<ThreadRetryWait> {
    if !running {
        return None;
    }
    let obj = waiting?.as_object()?;
    let reason = obj.get("kind").and_then(Value::as_str)?;
    if !matches!(reason, "rate_limit" | "retry") {
        return None;
    }
    let until = obj.get("until").and_then(Value::as_str)?;
    if chrono::DateTime::parse_from_rfc3339(until).is_err() {
        return None;
    }
    let attempt = obj.get("attempt").and_then(Value::as_i64)?;
    let max_attempts = obj.get("maxAttempts").and_then(Value::as_i64)?;
    Some(ThreadRetryWait {
        kind: "retry".to_owned(),
        reason: reason.to_owned(),
        until: until.to_owned(),
        attempt,
        max_attempts,
    })
}

pub fn thread_model(value: Option<&Value>) -> Option<ThreadModel> {
    let obj = value?.as_object()?;
    let provider = obj.get("provider").and_then(Value::as_str)?;
    let id = obj.get("id").and_then(Value::as_str)?;
    if provider.is_empty() || id.is_empty() {
        return None;
    }
    Some(ThreadModel {
        provider: provider.to_owned(),
        id: id.to_owned(),
    })
}

pub fn listed_states(asked: Option<&[ThreadState]>) -> HashSet<ThreadState> {
    match asked {
        Some(states) => states.iter().copied().collect(),
        None => THREAD_STATES
            .iter()
            .copied()
            .filter(|state| *state != ThreadState::Archived)
            .collect(),
    }
}
