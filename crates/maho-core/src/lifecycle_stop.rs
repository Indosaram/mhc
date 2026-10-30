//! Port of senpi packages/coding-agent/src/core/lifecycle-stop.ts.
//!
//! Why the last run stopped, derived only from that run's own end (senpi#3864).

use crate::session_lifecycle_types::{
    LifecycleAbortSource, LifecycleDeadlineKind, LifecycleStop, LifecycleStopCause,
    LifecycleTodoStop,
};
use serde_json::Value;

/// A goal guard read from the run's own extension-processing window: its reason and `at` (ms).
#[derive(Clone, Debug, PartialEq)]
pub struct GuardStop {
    pub reason: String,
    pub at: f64,
}

/// Stop facts the stopping run produced itself, read from that run's own end (senpi#3864).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunStopFacts {
    /// The run the turn end belonged to when its agent_end was emitted.
    pub run_id: Option<String>,
    pub guard: Option<GuardStop>,
    /// The todo-owed backstop outcome (`TodoOwedOutcome`), e.g. `"capped"`.
    pub todo: Option<String>,
}

/// A deadline captured when it fired, before its durable write, for the run it stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeadlineStopCapture {
    pub run_id: String,
    pub kind: LifecycleDeadlineKind,
    pub at: String,
    /// `Some(false)` only when the deadline-stop entry write failed.
    pub persisted: Option<bool>,
}

/// `Omit<LifecycleStop, "runId" | "epoch" | "at"> & { at?: string }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopDetail {
    pub cause: LifecycleStopCause,
    pub deadline: Option<LifecycleDeadlineKind>,
    pub guard: Option<String>,
    pub todo: Option<LifecycleTodoStop>,
    pub abort_source: Option<LifecycleAbortSource>,
    pub persisted: Option<bool>,
    pub at: Option<String>,
}

impl StopDetail {
    fn cause(cause: LifecycleStopCause) -> Self {
        Self {
            cause,
            deadline: None,
            guard: None,
            todo: None,
            abort_source: None,
            persisted: None,
            at: None,
        }
    }
}

fn iso_from_ms(ms: f64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// `Some(None)` is an abort with no classified source (`unknown-abort`).
fn abort_source(ended_by: &str) -> Option<Option<LifecycleAbortSource>> {
    match ended_by {
        "user-abort" => Some(Some(LifecycleAbortSource::User)),
        "system-abort" => Some(Some(LifecycleAbortSource::System)),
        "provider-abort" => Some(Some(LifecycleAbortSource::Provider)),
        "unknown-abort" => Some(None),
        _ => None,
    }
}

/// First match wins: the run's own deadline explains its system abort, a guard explains a refused continuation,
/// then the classified abort, and `no_wake` only for a stall no hold or guard owns. Provider failures keep the
/// failed path.
pub fn stop_detail(
    input: Option<&Value>,
    facts: Option<&RunStopFacts>,
    deadline: Option<&DeadlineStopCapture>,
    stalled: bool,
) -> Option<StopDetail> {
    let ended_by = input
        .and_then(|i| i["endedBy"].as_str())
        .unwrap_or_default();
    if ended_by == "provider-error" || ended_by == "policy-rejection" {
        return None;
    }
    if let Some(deadline) = deadline {
        return Some(StopDetail {
            deadline: Some(deadline.kind),
            at: Some(deadline.at.clone()),
            persisted: (deadline.persisted == Some(false)).then_some(false),
            ..StopDetail::cause(LifecycleStopCause::Deadline)
        });
    }
    if let Some(guard) = facts.and_then(|f| f.guard.as_ref()) {
        return Some(StopDetail {
            guard: Some(guard.reason.clone()),
            at: Some(iso_from_ms(guard.at)),
            ..StopDetail::cause(LifecycleStopCause::GoalGuard)
        });
    }
    if matches!(
        facts.and_then(|f| f.todo.as_deref()),
        Some("capped" | "cap")
    ) {
        return Some(StopDetail {
            todo: Some(LifecycleTodoStop::Cap),
            ..StopDetail::cause(LifecycleStopCause::TodoGuard)
        });
    }
    if input.is_some()
        && let Some(source) = abort_source(ended_by)
    {
        return Some(StopDetail {
            abort_source: source,
            ..StopDetail::cause(LifecycleStopCause::Aborted)
        });
    }
    let deadline_stop = input.is_some_and(|i| i["deadlineStop"] == true);
    let budget_spent = input.is_some_and(|i| i["budgetSpent"] == true);
    if stalled && !deadline_stop && !budget_spent {
        return Some(StopDetail::cause(LifecycleStopCause::NoWake));
    }
    None
}

pub fn same_stop(stop: &LifecycleStop, detail: &StopDetail) -> bool {
    stop.cause == detail.cause
        && stop.deadline == detail.deadline
        && stop.guard == detail.guard
        && stop.todo == detail.todo
        && stop.abort_source == detail.abort_source
        && stop.persisted == detail.persisted
}

/// The latest guard appended within one bound end's extension-processing window, including legacy entries.
pub fn goal_guard_after(branch: &[Value], guard_start_leaf_id: Option<&str>) -> Option<GuardStop> {
    let start = match guard_start_leaf_id {
        None => None,
        Some(id) => Some(branch.iter().position(|entry| entry["id"] == id)?),
    };
    let first = start.map_or(0, |index| index + 1);
    for entry in branch.iter().skip(first).rev() {
        if entry["type"] != "custom"
            || entry["customType"] != "goal-continuation-stopped"
            || !entry["data"].is_object()
        {
            continue;
        }
        let data = &entry["data"];
        return match (data["reason"].as_str(), data["at"].as_f64()) {
            (Some(reason), Some(at)) => Some(GuardStop {
                reason: reason.to_owned(),
                at,
            }),
            _ => None,
        };
    }
    None
}

/// Malformed optional detail is dropped; it never invalidates the record that carries it.
pub fn read_last_stop(value: &Value, run_id: &Value) -> Option<LifecycleStop> {
    let object = value.as_object()?;
    let id = object.get("runId")?.as_str()?;
    if run_id.as_str() != Some(id) {
        return None;
    }
    let at = object.get("at")?.as_str()?.to_owned();
    let epoch = object.get("epoch")?.as_f64()?;
    let base = LifecycleStop {
        cause: LifecycleStopCause::NoWake,
        run_id: id.to_owned(),
        at,
        epoch: epoch as u64,
        deadline: None,
        guard: None,
        todo: None,
        abort_source: None,
        persisted: None,
    };
    match object.get("cause")?.as_str()? {
        "deadline" => {
            let deadline = match object.get("deadline")?.as_str()? {
                "inactivity" => LifecycleDeadlineKind::Inactivity,
                "max_runtime" => LifecycleDeadlineKind::MaxRuntime,
                _ => return None,
            };
            Some(LifecycleStop {
                cause: LifecycleStopCause::Deadline,
                deadline: Some(deadline),
                persisted: (object.get("persisted") == Some(&Value::Bool(false))).then_some(false),
                ..base
            })
        }
        "goal_guard" => Some(LifecycleStop {
            cause: LifecycleStopCause::GoalGuard,
            guard: object
                .get("guard")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..base
        }),
        "todo_guard" => Some(LifecycleStop {
            cause: LifecycleStopCause::TodoGuard,
            todo: Some(LifecycleTodoStop::Cap),
            ..base
        }),
        "aborted" => Some(LifecycleStop {
            cause: LifecycleStopCause::Aborted,
            abort_source: match object.get("abortSource").and_then(Value::as_str) {
                Some("user") => Some(LifecycleAbortSource::User),
                Some("system") => Some(LifecycleAbortSource::System),
                Some("provider") => Some(LifecycleAbortSource::Provider),
                _ => None,
            },
            ..base
        }),
        "no_wake" => Some(base),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_runs_own_deadline_explains_its_abort_first() {
        let deadline = DeadlineStopCapture {
            run_id: "r".into(),
            kind: LifecycleDeadlineKind::Inactivity,
            at: "2026-10-01T00:00:00.000Z".into(),
            persisted: Some(false),
        };
        let detail = stop_detail(
            Some(&json!({"endedBy":"system-abort"})),
            None,
            Some(&deadline),
            true,
        )
        .expect("stop");
        assert_eq!(detail.cause, LifecycleStopCause::Deadline);
        assert_eq!(detail.persisted, Some(false));
        let aborted = stop_detail(Some(&json!({"endedBy":"unknown-abort"})), None, None, false)
            .expect("aborted");
        assert_eq!(aborted.cause, LifecycleStopCause::Aborted);
        assert_eq!(aborted.abort_source, None);
        assert!(
            stop_detail(Some(&json!({"endedBy":"provider-error"})), None, None, true).is_none()
        );
        assert_eq!(
            stop_detail(Some(&json!({"endedBy":"clean"})), None, None, true)
                .expect("stall")
                .cause,
            LifecycleStopCause::NoWake
        );
        assert!(
            stop_detail(
                Some(&json!({"endedBy":"clean","budgetSpent":true})),
                None,
                None,
                true
            )
            .is_none()
        );
    }

    #[test]
    fn a_guard_is_read_only_after_the_bound_leaf() {
        let branch = vec![
            json!({"id":"a","type":"custom","customType":"goal-continuation-stopped","data":{"reason":"old","at":1}}),
            json!({"id":"b","type":"message"}),
            json!({"id":"c","type":"custom","customType":"goal-continuation-stopped","data":{"reason":"new","at":2}}),
        ];
        assert_eq!(
            goal_guard_after(&branch, Some("b")).expect("guard").reason,
            "new"
        );
        assert!(goal_guard_after(&branch, Some("c")).is_none());
        assert!(goal_guard_after(&branch, Some("missing")).is_none());
        assert_eq!(
            goal_guard_after(&branch, None).expect("guard").reason,
            "new"
        );
    }

    #[test]
    fn a_stored_stop_is_read_only_for_its_own_run() {
        let stored = json!({"cause":"deadline","deadline":"max_runtime","runId":"r","at":"t","epoch":3,"persisted":false});
        let stop = read_last_stop(&stored, &json!("r")).expect("stop");
        assert_eq!(stop.deadline, Some(LifecycleDeadlineKind::MaxRuntime));
        assert_eq!(stop.persisted, Some(false));
        assert!(read_last_stop(&stored, &json!("other")).is_none());
        assert!(
            read_last_stop(
                &json!({"cause":"deadline","runId":"r","at":"t","epoch":3}),
                &json!("r")
            )
            .is_none()
        );
        let aborted = read_last_stop(
            &json!({"cause":"aborted","abortSource":"weird","runId":"r","at":"t","epoch":1}),
            &json!("r"),
        )
        .expect("aborted");
        assert_eq!(aborted.abort_source, None);
        assert!(same_stop(
            &aborted,
            &StopDetail::cause(LifecycleStopCause::Aborted)
        ));
    }
}
