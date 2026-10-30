//! Port of senpi packages/coding-agent/src/core/lifecycle-stop.ts.
//!
//! The persisted `lastStop {runId, epoch, at, cause}` that says why a run stopped (senpi#3864).
use serde_json::{Map, Value, json};

/// Stop facts the stopping run produced itself, read from that run's own end.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunStopFacts {
    /// The run the turn end belonged to when its agent_end was emitted.
    pub run_id: Option<String>,
    pub guard: Option<GoalGuard>,
    /// senpi `TodoOwedOutcome`; only `capped`/`cap` matter here.
    pub todo: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GoalGuard {
    pub reason: String,
    /// Epoch ms.
    pub at: f64,
}

/// A deadline captured when it fired, before its durable write, for the run it stopped.
#[derive(Debug, Clone, PartialEq)]
pub struct DeadlineStopCapture {
    pub run_id: String,
    /// `inactivity` or `max_runtime`.
    pub kind: String,
    pub at: String,
    /// `Some(false)` when the deadline entry write failed.
    pub persisted: Option<bool>,
}

fn iso_ms(ms: f64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|at| at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

/// senpi `stopDetail`: first match wins - the run's own deadline explains its system abort, a guard explains a
/// refused continuation, then the classified abort, and `no_wake` only for a stall no hold or guard owns.
/// Provider failures keep the failed path. `input` is the durable turn end (`endedBy`, `deadlineStop`,
/// `budgetSpent`). The result has no `runId`/`epoch`, and `at` only when the cause names one.
pub fn stop_detail(
    input: Option<&Value>,
    facts: Option<&RunStopFacts>,
    deadline: Option<&DeadlineStopCapture>,
    stalled: bool,
) -> Option<Value> {
    let ended_by = input.and_then(|input| input["endedBy"].as_str());
    if matches!(ended_by, Some("provider-error" | "policy-rejection")) {
        return None;
    }
    if let Some(deadline) = deadline {
        let mut detail = json!({"cause": "deadline", "deadline": deadline.kind, "at": deadline.at});
        if deadline.persisted == Some(false) {
            detail["persisted"] = json!(false);
        }
        return Some(detail);
    }
    if let Some(guard) = facts.and_then(|facts| facts.guard.as_ref()) {
        return Some(json!({"cause": "goal_guard", "guard": guard.reason, "at": iso_ms(guard.at)}));
    }
    if matches!(
        facts.and_then(|facts| facts.todo.as_deref()),
        Some("capped" | "cap")
    ) {
        return Some(json!({"cause": "todo_guard", "todo": "cap"}));
    }
    let abort_source = match ended_by {
        Some("user-abort") => Some(Some("user")),
        Some("system-abort") => Some(Some("system")),
        Some("provider-abort") => Some(Some("provider")),
        Some("unknown-abort") => Some(None),
        _ => None,
    };
    if let Some(source) = abort_source {
        let mut detail = json!({"cause": "aborted"});
        if let Some(source) = source {
            detail["abortSource"] = json!(source);
        }
        return Some(detail);
    }
    let deadline_stop = input.is_some_and(|input| input["deadlineStop"] == true);
    let budget_spent = input.is_some_and(|input| input["budgetSpent"] == true);
    if stalled && !deadline_stop && !budget_spent {
        return Some(json!({"cause": "no_wake"}));
    }
    None
}

/// senpi `sameStop`: the same cause and qualifiers, whatever the time.
pub fn same_stop(stop: &Value, detail: &Value) -> bool {
    [
        "cause",
        "deadline",
        "guard",
        "todo",
        "abortSource",
        "persisted",
    ]
    .iter()
    .all(|key| stop.get(*key) == detail.get(*key))
}

/// senpi `goalGuardAfter`: the latest guard appended within one bound end's extension-processing window,
/// including legacy entries. A guard leaf that is no longer on the branch attributes nothing.
pub fn goal_guard_after(branch: &[Value], guard_start_leaf_id: Option<&str>) -> Option<GoalGuard> {
    let start = match guard_start_leaf_id {
        None => None,
        Some(id) => Some(branch.iter().position(|entry| entry["id"] == id)?),
    };
    let first = start.map_or(0, |start| start + 1);
    for entry in branch[first.min(branch.len())..].iter().rev() {
        if entry["type"] != "custom"
            || entry["customType"] != "goal-continuation-stopped"
            || !entry["data"].is_object()
        {
            continue;
        }
        let data = &entry["data"];
        return match (data["reason"].as_str(), data["at"].as_f64()) {
            (Some(reason), Some(at)) => Some(GoalGuard {
                reason: reason.to_owned(),
                at,
            }),
            _ => None,
        };
    }
    None
}

/// senpi `readLastStop`: malformed optional detail is dropped; it never invalidates the record that carries it.
pub fn read_last_stop(value: &Value, run_id: &Value) -> Option<Value> {
    let run = value.get("runId")?.as_str()?;
    if run_id.as_str() != Some(run) {
        return None;
    }
    let at = value.get("at")?.as_str()?;
    let epoch = value.get("epoch")?.as_number()?;
    let mut stop = Map::new();
    stop.insert("runId".into(), json!(run));
    stop.insert("at".into(), json!(at));
    stop.insert("epoch".into(), Value::Number(epoch.clone()));
    let cause = value.get("cause")?.as_str()?;
    stop.insert("cause".into(), json!(cause));
    match cause {
        "deadline" => {
            let deadline = value.get("deadline")?.as_str()?;
            if !matches!(deadline, "inactivity" | "max_runtime") {
                return None;
            }
            stop.insert("deadline".into(), json!(deadline));
            if value.get("persisted") == Some(&json!(false)) {
                stop.insert("persisted".into(), json!(false));
            }
        }
        "goal_guard" => {
            if let Some(guard) = value.get("guard").and_then(Value::as_str) {
                stop.insert("guard".into(), json!(guard));
            }
        }
        "todo_guard" => {
            stop.insert("todo".into(), json!("cap"));
        }
        "aborted" => {
            if let Some(source) = value
                .get("abortSource")
                .and_then(Value::as_str)
                .filter(|source| matches!(*source, "user" | "system" | "provider"))
            {
                stop.insert("abortSource".into(), json!(source));
            }
        }
        "no_wake" => {}
        _ => return None,
    }
    Some(Value::Object(stop))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// senpi test/suite/regressions/3864-last-stop.test.ts: first match wins.
    #[test]
    fn stop_detail_prefers_deadline_then_guard_then_abort_then_no_wake() {
        let input = json!({"endedBy": "system-abort", "deadlineStop": false, "budgetSpent": false});
        let deadline = DeadlineStopCapture {
            run_id: "r".into(),
            kind: "inactivity".into(),
            at: "t".into(),
            persisted: Some(false),
        };
        assert_eq!(
            stop_detail(Some(&input), None, Some(&deadline), false),
            Some(
                json!({"cause": "deadline", "deadline": "inactivity", "at": "t", "persisted": false})
            )
        );
        let facts = RunStopFacts {
            guard: Some(GoalGuard {
                reason: "budget".into(),
                at: 0.0,
            }),
            ..Default::default()
        };
        assert_eq!(
            stop_detail(Some(&input), Some(&facts), None, false).expect("guard")["cause"],
            "goal_guard"
        );
        assert_eq!(
            stop_detail(Some(&input), None, None, false),
            Some(json!({"cause": "aborted", "abortSource": "system"}))
        );
        let clean = json!({"endedBy": "clean", "deadlineStop": false, "budgetSpent": false});
        assert_eq!(
            stop_detail(Some(&clean), None, None, true),
            Some(json!({"cause": "no_wake"}))
        );
        let failed = json!({"endedBy": "provider-error"});
        assert_eq!(
            stop_detail(Some(&failed), None, Some(&deadline), true),
            None
        );
    }

    /// senpi test/suite/regressions/3864-goal-guard-attribution.test.ts: only guards after the start leaf count.
    #[test]
    fn a_goal_guard_is_attributed_only_after_its_start_leaf() {
        let guard = |id: &str, reason: &str| json!({"id": id, "type": "custom", "customType": "goal-continuation-stopped", "data": {"reason": reason, "at": 1}});
        let branch = vec![
            guard("a", "old"),
            json!({"id": "leaf", "type": "message"}),
            guard("b", "new"),
        ];
        assert_eq!(
            goal_guard_after(&branch, Some("leaf"))
                .expect("guard")
                .reason,
            "new"
        );
        assert!(goal_guard_after(&branch[..2], Some("leaf")).is_none());
        assert!(goal_guard_after(&branch, Some("gone")).is_none());
        assert_eq!(
            goal_guard_after(&branch[..1], None).expect("guard").reason,
            "old"
        );
    }

    #[test]
    fn read_last_stop_requires_the_records_run() {
        let stop = json!({"runId": "r", "at": "t", "epoch": 2, "cause": "aborted", "abortSource": "bogus"});
        assert_eq!(
            read_last_stop(&stop, &json!("r")),
            Some(json!({"runId": "r", "at": "t", "epoch": 2, "cause": "aborted"}))
        );
        assert!(read_last_stop(&stop, &json!("other")).is_none());
        assert!(
            read_last_stop(
                &json!({"runId": "r", "at": "t", "epoch": 1, "cause": "deadline", "deadline": "x"}),
                &json!("r")
            )
            .is_none()
        );
    }
}
