//! Task-session records and timers (`storage/task.ts`).

use std::path::Path;

use crate::error::BoulderStateError;
use crate::js::{Js, JsObj};
use crate::records::BoulderState;
use crate::shared::{
    RESERVED_KEYS, find_work, get_elapsed_ms, normalize_session_id, project_work_to_mirror,
    works_by_id,
};
use crate::storage::read_state::{read_boulder_state, work_docs};
use crate::storage::write_state::{WorkSlot, locate_work, write_boulder_state};
use crate::time::now_iso_string;
use crate::types::{EndTaskTimerInput, TaskSessionInput, TaskTimerInput};

fn is_reserved(task_key: &str) -> bool {
    RESERVED_KEYS.contains(&task_key)
}

/// The TypeScript mirror projection copies `task_sessions` shallowly, so the root entry
/// of the active work's task is the *same object* as the work's entry and in-place timer
/// updates land in both. Legacy works share the root map itself.
fn mirror_aliased_session(doc: &mut JsObj, task_key: &str, session: &JsObj) {
    if let Some(sessions) = doc.get_obj_mut("task_sessions")
        && sessions.get(task_key).is_some()
    {
        sessions.set(task_key, Js::Object(session.clone()));
    }
}

fn base_task_session(input: &TaskSessionInput, session_id: &str) -> JsObj {
    let mut session = JsObj::from_entries([
        ("task_key".to_string(), Js::string(&input.task_key)),
        ("task_label".to_string(), Js::string(&input.task_label)),
        ("task_title".to_string(), Js::string(&input.task_title)),
        ("session_id".to_string(), Js::string(session_id)),
    ]);
    if let Some(agent) = &input.agent {
        session.set("agent", Js::string(agent));
    }
    if let Some(category) = &input.category {
        session.set("category", Js::string(category));
    }
    session
}

/// Record the session working on a task of the active work (or, for a legacy
/// mirror-only state, the root `task_sessions`) and persist. Reserved keys
/// (`__proto__`, `prototype`, `constructor`) and a missing state yield `Ok(None)`.
pub fn upsert_task_session_state(
    directory: &Path,
    input: &TaskSessionInput,
) -> Result<Option<BoulderState>, BoulderStateError> {
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(None);
    };
    if let Some(active_work_id) = state.active_work_id().filter(|id| !id.is_empty()) {
        return upsert_task_session_state_for_work(directory, active_work_id, input);
    }
    if is_reserved(&input.task_key) {
        return Ok(None);
    }

    let mut doc = state.doc;
    let mut session = base_task_session(input, &normalize_session_id(&input.session_id));
    session.set("updated_at", Js::string(&now_iso_string()));
    let mut sessions = match doc.coalesce("task_sessions") {
        Some(Js::Object(sessions)) => sessions.clone(),
        Some(_) | None => JsObj::default(),
    };
    sessions.set(&input.task_key, Js::Object(session));
    doc.set("task_sessions", Js::Object(sessions));
    let state = BoulderState::from_doc(doc);
    write_boulder_state(directory, &state)?;
    Ok(Some(state))
}

/// Record the session working on a task of `work_id`, keeping the task's existing timer
/// fields, and persist. `Ok(None)` for reserved keys, no state, or no such work.
pub fn upsert_task_session_state_for_work(
    directory: &Path,
    work_id: &str,
    input: &TaskSessionInput,
) -> Result<Option<BoulderState>, BoulderStateError> {
    if is_reserved(&input.task_key) {
        return Ok(None);
    }
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(None);
    };
    let works = work_docs(&state.doc);
    let Some(target) = find_work(&works, &Js::string(work_id)) else {
        return Ok(None);
    };

    let previous = target
        .get_obj("task_sessions")
        .and_then(|sessions| sessions.get_obj(&input.task_key));
    let mut session = base_task_session(input, &normalize_session_id(&input.session_id));
    for key in ["started_at", "ended_at", "elapsed_ms", "status"] {
        if let Some(value) = previous.and_then(|previous| previous.get(key)) {
            session.set(key, value.clone());
        }
    }
    session.set("updated_at", Js::string(&now_iso_string()));

    let mut next_work = target.clone();
    let mut sessions = next_work.field("task_sessions").spread();
    sessions.set(&input.task_key, Js::Object(session));
    next_work.set("task_sessions", Js::Object(sessions));
    next_work.set("updated_at", Js::string(&now_iso_string()));

    let mut all_works = works_by_id(&works);
    all_works.set(work_id, Js::Object(next_work.clone()));
    let mut next_state = state.doc;
    next_state.set("schema_version", Js::int(2));
    next_state.set("works", Js::Object(all_works));
    if next_state
        .field("active_work_id")
        .strict_eq(&Js::string(work_id))
    {
        project_work_to_mirror(&mut next_state, &next_work);
    }
    let next_state = BoulderState::from_doc(next_state);
    write_boulder_state(directory, &next_state)?;
    Ok(Some(next_state))
}

/// Upsert the task session and mark it running, keeping an earlier `started_at`.
pub fn start_task_timer(
    directory: &Path,
    work_id: &str,
    input: &TaskTimerInput,
) -> Result<Option<BoulderState>, BoulderStateError> {
    let Some(next_state) = upsert_task_session_state_for_work(directory, work_id, &input.task)?
    else {
        return Ok(None);
    };
    let mut doc = next_state.doc;
    let task_key = &input.task.task_key;
    let Some(work) = doc
        .get_obj_mut("works")
        .and_then(|works| works.get_obj_mut(work_id))
    else {
        return Ok(None);
    };
    let Some(session) = work
        .get_obj_mut("task_sessions")
        .and_then(|sessions| sessions.get_obj_mut(task_key))
    else {
        return Ok(None);
    };
    let started_at = session
        .coalesce("started_at")
        .cloned()
        .or_else(|| input.started_at.as_deref().map(Js::string))
        .unwrap_or_else(|| Js::string(&now_iso_string()));
    session.set("started_at", started_at);
    session.set("status", Js::string("running"));
    session.set("updated_at", Js::string(&now_iso_string()));
    let session = session.clone();
    work.set("updated_at", Js::string(&now_iso_string()));
    if doc.field("active_work_id").strict_eq(&Js::string(work_id)) {
        mirror_aliased_session(&mut doc, task_key, &session);
    }
    let state = BoulderState::from_doc(doc);
    write_boulder_state(directory, &state)?;
    Ok(Some(state))
}

/// Mark a task session completed with its elapsed time and persist. `Ok(None)` when
/// there is no state, no such work, or the work has no such task session.
pub fn end_task_timer(
    directory: &Path,
    work_id: &str,
    input: &EndTaskTimerInput,
) -> Result<Option<BoulderState>, BoulderStateError> {
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(None);
    };
    let mut doc = state.doc;
    let target_work_id = Js::string(work_id);
    let Some(slot) = locate_work(&doc, &target_work_id) else {
        return Ok(None);
    };
    let mut work = match &slot {
        WorkSlot::Stored(key) => doc
            .get_obj("works")
            .and_then(|works| works.get_obj(key))
            .cloned()
            .unwrap_or_default(),
        WorkSlot::Legacy(work) => work.clone(),
    };
    let Some(mut session) = work
        .get_obj("task_sessions")
        .and_then(|sessions| sessions.get_obj(&input.task_key))
        .cloned()
    else {
        return Ok(None);
    };

    let ended_at = input.ended_at.clone().unwrap_or_else(now_iso_string);
    let elapsed = get_elapsed_ms(session.get("started_at"), &ended_at);
    session.set("ended_at", Js::string(&ended_at));
    session.set("elapsed_ms", elapsed);
    session.set("status", Js::string("completed"));
    session.set("updated_at", Js::string(&now_iso_string()));
    if let Some(sessions) = work.get_obj_mut("task_sessions") {
        sessions.set(&input.task_key, Js::Object(session.clone()));
    }
    work.set("updated_at", Js::string(&now_iso_string()));

    match &slot {
        WorkSlot::Stored(key) => {
            if let Some(works) = doc.get_obj_mut("works") {
                works.set(key, Js::Object(work.clone()));
            }
            if doc.field("active_work_id").strict_eq(&target_work_id) {
                mirror_aliased_session(&mut doc, &input.task_key, &session);
            }
        }
        WorkSlot::Legacy(_) => mirror_aliased_session(&mut doc, &input.task_key, &session),
    }
    if doc.field("active_work_id").strict_eq(&target_work_id) {
        project_work_to_mirror(&mut doc, &work);
    }
    let state = BoulderState::from_doc(doc);
    write_boulder_state(directory, &state)?;
    Ok(Some(state))
}
