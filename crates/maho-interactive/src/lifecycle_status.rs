//! Port of senpi `packages/coding-agent/src/modes/interactive/lifecycle-status.ts` (v2026.10.27-2).
//!
//! The snapshots are the wire `SessionLifecycle` JSON the engine publishes on `session_lifecycle`
//! (`AgentSession::lifecycle_snapshot`), so the wire-only `activity` / `activityRevision` fields
//! are read as soon as the engine carries them.

use serde_json::Value;

pub const LIFECYCLE_STATUS_KEY: &str = "session-lifecycle";

/// Coarse state and activity have different high-water marks. Only a bound snapshot can change owners.
#[derive(Debug, Default)]
pub struct LifecycleStatus {
    session_id: Option<String>,
    owner: Option<String>,
    snapshot: Option<Value>,
    activity_seq: i64,
    activity_revision: i64,
}

fn seq(snapshot: &Value) -> i64 {
    snapshot["seq"].as_i64().unwrap_or(0)
}

fn revision_owner(snapshot: &Value) -> Option<&str> {
    snapshot
        .get("activityRevision")?
        .get("instanceId")?
        .as_str()
}

fn without_activity(snapshot: &Value) -> Value {
    let mut next = snapshot.clone();
    if let Some(object) = next.as_object_mut() {
        object.remove("activity");
        object.remove("activityRevision");
    }
    next
}

impl LifecycleStatus {
    pub fn new() -> Self {
        Self {
            activity_seq: -1,
            activity_revision: -1,
            ..Self::default()
        }
    }

    pub fn owns(&self, snapshot: &Value) -> bool {
        snapshot.get("activityRevision").is_none_or(Value::is_null)
            || revision_owner(snapshot) == self.owner.as_deref()
    }

    pub fn bind(&mut self, session_id: &str, snapshot: &Value) {
        if self.session_id.as_deref() != Some(session_id) {
            self.session_id = Some(session_id.to_owned());
            self.owner = None;
            self.snapshot = None;
            self.activity_seq = -1;
            self.activity_revision = -1;
        }
        if let Some(owner) = revision_owner(snapshot)
            && self.owner.as_deref() != Some(owner)
        {
            self.owner = Some(owner.to_owned());
            self.activity_seq = -1;
            self.activity_revision = -1;
            if let Some(current) = &self.snapshot {
                self.snapshot = Some(without_activity(current));
            }
        }
        self.accept(snapshot);
    }

    pub fn accept(&mut self, snapshot: &Value) {
        if !snapshot.is_object() {
            return;
        }
        let previous_seq = self.snapshot.as_ref().map(seq);
        if previous_seq.is_some_and(|previous| seq(snapshot) < previous) {
            return;
        }
        let newer = previous_seq.is_none_or(|previous| seq(snapshot) > previous);
        let revision = snapshot
            .get("activityRevision")
            .filter(|revision| !revision.is_null());
        let accepted_activity = match revision {
            Some(revision) => {
                revision["instanceId"].as_str() == self.owner.as_deref()
                    && (seq(snapshot) > self.activity_seq
                        || (seq(snapshot) == self.activity_seq
                            && revision["revision"].as_i64().unwrap_or(-1)
                                > self.activity_revision))
            }
            None => newer,
        };
        if newer {
            // An unfamiliar owner cannot smuggle activity in alongside an independently newer durable
            // state: a newer state from another owner also drops the bound owner's activity until the
            // bound owner's next update or a rebind from the authoritative snapshot.
            self.snapshot = Some(without_activity(snapshot));
        }
        if accepted_activity && let Some(current) = self.snapshot.as_mut() {
            if let Some(object) = current.as_object_mut() {
                match snapshot.get("activity").filter(|value| !value.is_null()) {
                    Some(activity) => {
                        object.insert("activity".into(), activity.clone());
                    }
                    None => {
                        object.remove("activity");
                    }
                }
                match revision {
                    Some(revision) => {
                        object.insert("activityRevision".into(), revision.clone());
                    }
                    None => {
                        object.remove("activityRevision");
                    }
                }
            }
            self.activity_seq = seq(snapshot);
            self.activity_revision = revision
                .and_then(|revision| revision["revision"].as_i64())
                .unwrap_or(-1);
        }
    }

    pub fn text(&self) -> Option<String> {
        let snapshot = self.snapshot.as_ref()?;
        let mut parts: Vec<String> =
            vec![snapshot["state"].as_str().unwrap_or_default().to_owned()];
        let label = |value: &Value, fallback: &str| -> String {
            value
                .get("label")
                .and_then(Value::as_str)
                .or_else(|| value.get(fallback).and_then(Value::as_str))
                .unwrap_or_default()
                .to_owned()
        };
        if let Some(waits) = snapshot.get("waitingOn").filter(|waits| waits.is_object()) {
            let children = waits["children"].as_u64().unwrap_or(0);
            if children > 0 {
                parts.push(format!("children: {children}"));
            }
            let sources = waits["childSources"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(waits["background"].as_array().into_iter().flatten());
            for source in sources {
                parts.push(format!(
                    "{}: {}",
                    source["source"].as_str().unwrap_or_default(),
                    source["activeCount"].as_u64().unwrap_or(0)
                ));
                for item in source["items"].as_array().into_iter().flatten() {
                    parts.push(label(item, "id"));
                }
                if let Some(omitted) = source["omittedItems"].as_u64().filter(|n| *n > 0) {
                    parts.push(format!("+{omitted} named items"));
                }
            }
            for child in waits["childReports"].as_array().into_iter().flatten() {
                parts.push(format!(
                    "{}: {}",
                    label(child, "taskId"),
                    child["state"].as_str().unwrap_or_default()
                ));
            }
            if let Some(omitted) = waits["childReportsOmitted"].as_u64().filter(|n| *n > 0) {
                parts.push(format!("+{omitted} child reports"));
            }
            for wait in waits["human"].as_array().into_iter().flatten() {
                let kind = wait["kind"].as_str().unwrap_or_default();
                match wait["childLabel"]
                    .as_str()
                    .filter(|label| !label.is_empty())
                {
                    Some(child) => parts.push(format!("{kind}: {child}")),
                    None => parts.push(kind.to_owned()),
                }
            }
        }
        if let Some(stop) = snapshot.get("lastStop").filter(|stop| stop.is_object()) {
            let detail = ["deadline", "guard", "todo", "abortSource"]
                .iter()
                .find_map(|key| stop[*key].as_str().filter(|value| !value.is_empty()));
            let cause = stop["cause"].as_str().unwrap_or_default();
            parts.push(match detail {
                Some(detail) => format!("{cause}: {detail}"),
                None => cause.to_owned(),
            });
        }
        if let Some(failure) = snapshot
            .get("failure")
            .filter(|failure| failure.is_object())
        {
            let reason = failure["reason"].as_str().unwrap_or_default();
            parts.push(
                match failure["taskId"].as_str().filter(|task| !task.is_empty()) {
                    Some(task) => format!("{reason}: {task}"),
                    None => reason.to_owned(),
                },
            );
        }
        if let Some(activity) = snapshot.get("activity") {
            match activity["kind"].as_str() {
                Some("tool") => parts.push(format!(
                    "activity: {} (outstanding: {})",
                    activity["toolName"].as_str().unwrap_or_default(),
                    activity["outstanding"]
                )),
                Some("model") => parts.push("activity: model".to_owned()),
                _ => {}
            }
        }
        Some(maho_tui::terminal_text::sanitize_terminal_label(
            &parts.join(" · "),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lifecycle(seq: u64, state: &str) -> Value {
        json!({"state": state, "seq": seq, "epoch": 1})
    }

    #[test]
    fn renders_state_waits_stop_failure_and_activity() {
        let mut status = LifecycleStatus::new();
        status.bind(
            "s1",
            &json!({
                "state": "waiting_children", "seq": 3,
                "waitingOn": {
                    "children": 2,
                    "background": [{"source": "bash", "activeCount": 1, "items": [{"id": "job-1", "label": "build"}], "omittedItems": 2}],
                    "childReports": [{"taskId": "t1", "state": "running"}],
                    "childReportsOmitted": 1,
                    "human": [{"kind": "question", "childLabel": "worker"}, {"kind": "approval"}],
                },
                "lastStop": {"cause": "deadline", "deadline": "inactivity"},
                "failure": {"reason": "task_error", "taskId": "t9"},
                "activity": {"kind": "tool", "toolName": "bash", "outstanding": 2},
                "activityRevision": {"instanceId": "i1", "revision": 1},
            }),
        );
        assert_eq!(
            status.text().as_deref(),
            Some(
                "waiting_children · children: 2 · bash: 1 · build · +2 named items · t1: running · +1 child reports · question: worker · approval · deadline: inactivity · task_error: t9 · activity: bash (outstanding: 2)"
            )
        );
    }

    #[test]
    fn older_seq_is_ignored_and_newer_state_replaces() {
        let mut status = LifecycleStatus::new();
        status.bind("s1", &lifecycle(5, "running"));
        status.accept(&lifecycle(4, "done"));
        assert_eq!(status.text().as_deref(), Some("running"));
        status.accept(&lifecycle(6, "done"));
        assert_eq!(status.text().as_deref(), Some("done"));
    }

    #[test]
    fn drops_foreign_owner_activity_and_orders_by_revision() {
        let mut status = LifecycleStatus::new();
        let with = |seq: u64, owner: &str, revision: u64, kind: &str| {
            json!({"state": "running", "seq": seq,
                "activity": {"kind": kind, "toolName": "read", "outstanding": 1},
                "activityRevision": {"instanceId": owner, "revision": revision}})
        };
        status.bind("s1", &with(1, "owner", 1, "model"));
        assert_eq!(status.text().as_deref(), Some("running · activity: model"));
        // Same seq, higher revision from the bound owner: activity advances.
        status.accept(&with(1, "owner", 2, "tool"));
        assert_eq!(
            status.text().as_deref(),
            Some("running · activity: read (outstanding: 1)")
        );
        assert!(!status.owns(&with(1, "other", 9, "model")));
        // A newer state from another owner drops activity instead of adopting it.
        status.accept(&with(2, "other", 1, "model"));
        assert_eq!(status.text().as_deref(), Some("running"));
        // Rebinding to the authoritative snapshot adopts the new owner.
        status.bind("s1", &with(2, "other", 1, "model"));
        assert_eq!(status.text().as_deref(), Some("running · activity: model"));
    }

    #[test]
    fn a_new_session_resets_the_high_water_marks() {
        let mut status = LifecycleStatus::new();
        status.bind("s1", &lifecycle(9, "done"));
        status.bind("s2", &lifecycle(1, "running"));
        assert_eq!(status.text().as_deref(), Some("running"));
        assert!(LifecycleStatus::new().text().is_none());
    }

    #[test]
    fn activity_replays_and_legacy_tombstones_leave_current_state_intact() {
        let update = |revision: i64, count: i64| {
            json!({"state":"running","seq":100,
                "activity":{"kind":"tool","toolName":"bash","outstanding":count},
                "activityRevision":{"instanceId":"owner","revision":revision}})
        };
        let mut status = LifecycleStatus::new();
        status.bind("s", &update(0, 0));
        for (revision, count) in [(1, 1), (2, 2), (3, 1)] {
            status.accept(&update(revision, count));
            assert_eq!(status.snapshot.as_ref().expect("snapshot")["activity"]["outstanding"], count);
        }
        let current = status.snapshot.clone();
        let mut older_seq = update(900, 99);
        older_seq["seq"] = json!(99);
        let mut foreign = update(900, 99);
        foreign["activityRevision"]["instanceId"] = json!("foreign");
        for stale in [update(3, 99), update(2, 99), older_seq, foreign, lifecycle(100, "done")] {
            status.accept(&stale);
            assert_eq!(status.snapshot, current);
        }
        let mut same_seq = update(4, 2);
        same_seq["state"] = json!("failed");
        same_seq["failure"] = json!({"reason":"lost"});
        status.accept(&same_seq);
        let snapshot = status.snapshot.as_ref().expect("snapshot");
        assert_eq!(snapshot["state"], "running");
        assert!(snapshot.get("failure").is_none());
        assert_eq!(snapshot["activity"]["outstanding"], 2);
        status.accept(&lifecycle(101, "done"));
        let completed = status.snapshot.clone();
        status.accept(&lifecycle(101, "failed"));
        assert_eq!(status.snapshot, completed);
    }

    #[test]
    fn authoritative_rebinding_preserves_or_resets_activity_watermarks_by_owner() {
        let update = |owner: &str, revision: i64| {
            json!({"state":"running","seq":100,"activity":{"kind":"model"},
                "activityRevision":{"instanceId":owner,"revision":revision}})
        };
        let mut status = LifecycleStatus::new();
        status.bind("s", &update("owner", 5));
        status.bind("s", &update("owner", 1));
        assert_eq!(status.activity_revision, 5);
        let mut replacement = update("replacement", 0);
        replacement.as_object_mut().expect("snapshot object").remove("activity");
        status.bind("s", &replacement);
        assert!(status.snapshot.as_ref().expect("snapshot").get("activity").is_none());
        status.accept(&update("replacement", 1));
        let current = status.snapshot.clone();
        status.accept(&update("owner", 900));
        assert_eq!(status.snapshot, current);
        status.accept(&lifecycle(101, "waiting_background"));
        let snapshot = status.snapshot.as_ref().expect("snapshot");
        assert_eq!(snapshot["state"], "waiting_background");
        assert!(snapshot.get("activity").is_none());
    }
}
