//! Thread result row shaping and stored-receipt migration (`thread/tools/thread-row.ts`).

use serde_json::{Map, Value, json};

use super::super::address_core::row_name;
use super::super::contracts::state::ThreadState;
use super::super::errors::ThreadToolFailure;
use super::super::thread_link::thread_link;
use super::super::worktree::ThreadWorktree;
use super::internals::ThreadToolSummary;

pub fn thread_row(thread: &ThreadToolSummary, machine: Option<&str>) -> Value {
    let mut map = Map::new();
    map.insert("thread".into(), Value::String(thread.thread_id.clone()));
    map.insert("name".into(), Value::String(thread.name.clone()));
    if thread.unnamed == Some(true) {
        map.insert("unnamed".into(), Value::Bool(true));
    }
    map.insert("status".into(), Value::String(thread.status.clone()));
    map.insert(
        "state".into(),
        Value::String(thread.state.as_str().to_owned()),
    );
    if let Some(model) = &thread.model {
        map.insert(
            "model".into(),
            json!({ "provider": model.provider, "id": model.id }),
        );
    }
    if let Some(activity) = thread.activity {
        map.insert(
            "activity".into(),
            Value::String(activity.as_str().to_owned()),
        );
    }
    if let Some(error_code) = &thread.error_code {
        map.insert("error_code".into(), Value::String(error_code.clone()));
    }
    if let Some(turn_id) = &thread.turn_id {
        map.insert("turn_id".into(), Value::String(turn_id.clone()));
    }
    if let Some(waiting) = &thread.waiting_on {
        map.insert(
            "waiting_on".into(),
            serde_json::to_value(waiting).unwrap_or(Value::Null),
        );
    }
    if let Some(as_of) = &thread.as_of {
        map.insert("as_of".into(), Value::String(as_of.clone()));
    }
    if let Some(machine) = machine.filter(|s| !s.is_empty()) {
        map.insert("machine".into(), Value::String(machine.to_owned()));
    }
    map.insert("cwd".into(), Value::String(thread.cwd.clone()));
    map.insert(
        "created_at".into(),
        Value::String(thread.created_at.clone()),
    );
    map.insert(
        "updated_at".into(),
        thread
            .updated_at
            .as_ref()
            .map(|u| Value::String(u.clone()))
            .unwrap_or(Value::Null),
    );
    map.insert("link".into(), Value::String(thread.link.clone()));
    if let Some(device) = &thread.device {
        map.insert("device".into(), Value::String(device.clone()));
    }
    if let Some(alias) = &thread.alias {
        map.insert("alias".into(), Value::String(alias.clone()));
    }
    if let Some(surface) = &thread.surface {
        map.insert("surface".into(), Value::String(surface.clone()));
    }
    if let Some(controls) = &thread.controls {
        map.insert(
            "controls".into(),
            Value::Array(controls.iter().map(|c| Value::String(c.clone())).collect()),
        );
    }
    if let Some(version) = &thread.engine_version {
        map.insert("engine_version".into(), Value::String(version.clone()));
    }
    if let Some(error_note) = &thread.error_note {
        map.insert("error_note".into(), Value::String(error_note.clone()));
    }
    if let Some(skills) = &thread.skills {
        map.insert(
            "skills".into(),
            Value::Array(skills.iter().map(|s| Value::String(s.clone())).collect()),
        );
    }
    Value::Object(map)
}

pub fn row_named(
    mut summary: ThreadToolSummary,
    name: Option<&str>,
    first_message: Option<&str>,
) -> ThreadToolSummary {
    let shown = row_name(&super::super::address_core::row::NamedRow {
        name,
        first_message,
        folder: &summary.cwd,
    });
    summary.name = shown.name.clone();
    summary.unnamed = if shown.unnamed { Some(true) } else { None };
    summary.link = thread_link(&summary.thread_id, Some(&shown.name));
    summary
}

pub fn opened_result(
    opened: &ThreadToolSummary,
    worktree: Option<&ThreadWorktree>,
    warnings: Option<&[ThreadToolFailure]>,
    deduplicated: bool,
) -> Value {
    let mut row = thread_row(opened, None);
    if let Some(obj) = row.as_object_mut() {
        obj.insert("kind".into(), Value::String("ok".into()));
        if let Some(wt) = worktree {
            obj.insert(
                "worktree".into(),
                serde_json::to_value(wt).unwrap_or(Value::Null),
            );
        }
        if let Some(w) = warnings
            && !w.is_empty()
        {
            obj.insert(
                "warnings".into(),
                serde_json::to_value(w).unwrap_or(Value::Null),
            );
        }
        obj.insert("deduplicated".into(), Value::Bool(deduplicated));
    }
    row
}

fn stored_state(row: &Map<String, Value>) -> ThreadState {
    if row.get("status").and_then(Value::as_str) == Some("live")
        && row.get("alive").and_then(Value::as_bool) != Some(false)
    {
        ThreadState::Idle
    } else {
        ThreadState::Inactive
    }
}

fn stored_row(row: Map<String, Value>) -> Value {
    if row.get("thread").and_then(Value::as_str).is_some()
        && row.get("state").and_then(Value::as_str).is_some()
    {
        return Value::Object(row);
    }
    if row.get("thread").and_then(Value::as_str).is_some() {
        let mut next = row.clone();
        next.remove("alive");
        next.insert(
            "state".into(),
            Value::String(stored_state(&row).as_str().to_owned()),
        );
        return Value::Object(next);
    }
    let Some(id) = row
        .get("thread_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Value::Object(row);
    };
    let name = row
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let link = row
        .get("link")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| thread_link(&id, Some(&name)));
    let mut next = Map::new();
    next.insert("thread".into(), Value::String(id));
    next.insert("name".into(), Value::String(name));
    if let Some(v) = row.get("status") {
        next.insert("status".into(), v.clone());
    }
    next.insert(
        "state".into(),
        Value::String(stored_state(&row).as_str().to_owned()),
    );
    for key in [
        "cwd",
        "created_at",
        "updated_at",
        "device",
        "alias",
        "surface",
        "error_note",
    ] {
        if let Some(v) = row.get(key)
            && !v.is_null()
        {
            next.insert(key.into(), v.clone());
        }
    }
    next.insert("link".into(), Value::String(link));
    Value::Object(next)
}

fn rename_key(obj: Map<String, Value>, from: &str, to: &str) -> Map<String, Value> {
    obj.into_iter()
        .map(|(k, v)| {
            if k == from {
                (to.to_owned(), v)
            } else {
                (k, v)
            }
        })
        .collect()
}

fn stored_candidates(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| match item {
                    Value::Object(obj) if obj.contains_key("id") => {
                        Value::Object(rename_key(obj, "id", "thread"))
                    }
                    other => other,
                })
                .collect(),
        ),
        other => other,
    }
}

pub fn current_result(result: Value) -> Value {
    let Value::Object(mut value) = result else {
        return result;
    };
    if value.get("kind").and_then(Value::as_str) == Some("error") {
        if let Some(cands) = value.remove("candidates") {
            value.insert("candidates".into(), stored_candidates(cands));
        }
        if let Some(Value::Object(mut err)) = value.remove("error") {
            if let Some(Value::Object(mut details)) = err.remove("details") {
                if let Some(cands) = details.remove("candidates") {
                    details.insert("candidates".into(), stored_candidates(cands));
                }
                err.insert("details".into(), Value::Object(details));
            }
            value.insert("error".into(), Value::Object(err));
        }
        return Value::Object(value);
    }
    if let Some(Value::Object(nested)) = value.remove("thread") {
        let mut flat = Map::new();
        if let Some(k) = value.remove("kind") {
            flat.insert("kind".into(), k);
        }
        if let Value::Object(row) = stored_row(nested) {
            for (k, v) in row {
                flat.insert(k, v);
            }
        }
        for (k, v) in value {
            flat.insert(k, v);
        }
        value = flat;
    } else if value.get("thread_id").and_then(Value::as_str).is_some()
        && !value.contains_key("thread")
    {
        value = rename_key(value, "thread_id", "thread");
    }
    if value.get("thread").and_then(Value::as_str).is_some()
        && value.get("status").and_then(Value::as_str).is_some()
        && let Value::Object(row) = stored_row(value.clone())
    {
        value = row;
    }
    if let Some(Value::Array(threads)) = value.remove("threads") {
        let mapped = threads
            .into_iter()
            .map(|row| match row {
                Value::Object(obj) => stored_row(obj),
                other => other,
            })
            .collect();
        value.insert("threads".into(), Value::Array(mapped));
    }
    value.remove("endpoint");
    if let Some(Value::Object(mut prompt)) = value.remove("prompt") {
        prompt.remove("endpoint");
        value.insert("prompt".into(), Value::Object(prompt));
    }
    Value::Object(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::contracts::results::ThreadRetryWait;
    use crate::thread::contracts::state::{ThreadActivity, ThreadModel, ThreadState};

    fn base_summary() -> ThreadToolSummary {
        ThreadToolSummary {
            thread_id: "01965d50-0000-7000-8000-000000000001".into(),
            session_id: "rpc-1".into(),
            durable_session_id: Some("01965d50-0000-7000-8000-000000000001".into()),
            session_path: None,
            socket: Some("/run/host.sock".into()),
            name: "Alpha".into(),
            unnamed: None,
            status: "live".into(),
            state: ThreadState::Idle,
            activity: Some(ThreadActivity::Idle),
            error_code: None,
            turn_id: None,
            waiting_on: None,
            model: Some(ThreadModel {
                provider: "openai".into(),
                id: "gpt-5.4".into(),
            }),
            as_of: None,
            device: None,
            alias: None,
            cwd: "/work/repo".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: Some("2026-01-01T00:01:00Z".into()),
            surface: Some("daemon".into()),
            controls: Some(vec!["rename".into(), "close".into()]),
            engine_version: Some("2026.11.19-5".into()),
            error_note: None,
            skills: Some(vec!["git-master".into()]),
            link: "thread://01965d50-0000-7000-8000-000000000001".into(),
        }
    }

    #[test]
    fn thread_row_emits_thread_state_model_skills_and_omits_optional_empty_fields() {
        let mut s = base_summary();
        let row = thread_row(&s, Some("studio"));
        assert_eq!(row["thread"], s.thread_id);
        assert_eq!(row["status"], "live");
        assert_eq!(row["state"], "idle");
        assert_eq!(row["model"]["id"], "gpt-5.4");
        assert_eq!(row["skills"][0], "git-master");
        assert_eq!(row["machine"], "studio");
        assert!(row.get("socket").is_none());

        s.skills = None;
        s.controls = None;
        s.model = None;
        let stripped = thread_row(&s, None);
        assert!(stripped.get("skills").is_none());
        assert!(stripped.get("controls").is_none());
        assert!(stripped.get("machine").is_none());
    }

    #[test]
    fn thread_row_formats_errored_and_waiting_states() {
        let mut s = base_summary();
        s.state = ThreadState::Running;
        s.activity = Some(ThreadActivity::Errored);
        s.error_code = Some("rate_limit".into());
        s.waiting_on = Some(ThreadRetryWait {
            kind: "rate_limit".into(),
            reason: "429".into(),
            until: "2026-01-01T00:05:00Z".into(),
            attempt: 2,
            max_attempts: 5,
        });
        let row = thread_row(&s, None);
        assert_eq!(row["state"], "running");
        assert_eq!(row["activity"], "errored");
        assert_eq!(row["error_code"], "rate_limit");
        assert_eq!(row["waiting_on"]["attempt"], 2);
    }

    #[test]
    fn row_named_falls_back_to_first_message_else_folder_and_marks_unnamed() {
        let s = base_summary();
        let from_msg = row_named(s.clone(), None, Some("Fix the CI job"));
        assert_eq!(from_msg.name, "Fix the CI job");
        assert_eq!(from_msg.unnamed, Some(true));

        let from_dir = row_named(s, Some("  "), None);
        assert_eq!(from_dir.name, "repo");
        assert_eq!(from_dir.unnamed, Some(true));
    }

    #[test]
    fn current_result_flattens_stored_thread_and_strips_endpoint() {
        let legacy = json!({
            "kind": "ok",
            "thread": {
                "thread_id": "t-1",
                "name": "Alpha",
                "status": "live",
                "alive": true,
                "endpoint": { "socket": "/run/x.sock" }
            },
            "endpoint": { "socket": "/run/x.sock" }
        });
        let current = current_result(legacy);
        assert_eq!(current["thread"], "t-1");
        assert_eq!(current["state"], "idle");
        assert!(current.get("endpoint").is_none());
    }

    #[test]
    fn current_result_normalizes_error_candidates_and_threads_list() {
        let listed = json!({
            "kind": "ok",
            "threads": [{ "thread_id": "t-1", "name": "Alpha", "status": "resumable", "alive": false }]
        });
        let normalized = current_result(listed);
        assert_eq!(normalized["threads"][0]["thread"], "t-1");
        assert_eq!(normalized["threads"][0]["state"], "inactive");

        let err = current_result(json!({
            "kind": "error",
            "candidates": [{ "id": "t-1", "name": "Alpha" }],
            "error": { "code": "ambiguous_target", "details": { "candidates": [{ "id": "t-2" }] } }
        }));
        assert_eq!(err["candidates"][0]["thread"], "t-1");
        assert_eq!(err["error"]["details"]["candidates"][0]["thread"], "t-2");
    }

    #[test]
    fn opened_result_attaches_worktree_warnings_and_deduplicated_flag() {
        let s = base_summary();
        let out = opened_result(&s, None, None, true);
        assert_eq!(out["kind"], "ok");
        assert_eq!(out["deduplicated"], true);
        assert!(out.get("worktree").is_none());
    }
}
