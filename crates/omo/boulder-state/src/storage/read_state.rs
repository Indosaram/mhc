//! Reading and querying the state document (`storage/read-state.ts`).

use std::path::Path;

use serde_json::Value;

use crate::error::BoulderStateError;
use crate::js::{Js, JsObj};
use crate::records::{BoulderState, BoulderWorkState, TaskSessionState};
use crate::shared::{
    build_work_from_mirror, normalize_session_id, project_work_to_mirror, select_mirror_work,
    work_objects, work_time_ms,
};
use crate::storage::path::{get_boulder_file_path, resolve_boulder_plan_path_for_work};
use crate::storage::plan_progress::get_plan_progress;
use crate::types::{BoulderWorkResumeOption, BoulderWorkStatus, WorkLookupOptions};

/// Read `<directory>/.omo/boulder.json`, normalize session ids and project the active
/// work onto the root mirror.
///
/// A missing file or an empty `{}` document is `Ok(None)`. A file that exists but is not
/// a JSON object whose `works` entries are objects is an error.
pub fn read_boulder_state(directory: &Path) -> Result<Option<BoulderState>, BoulderStateError> {
    let path = get_boulder_file_path(directory);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(BoulderStateError::Read { path, source }),
    };
    let content = String::from_utf8_lossy(&bytes);
    let parsed: Value =
        serde_json::from_str(&content).map_err(|source| BoulderStateError::InvalidJson {
            path: path.clone(),
            source,
        })?;
    let mut state = match Js::from_value(parsed) {
        Js::Object(state) => state,
        Js::Undefined | Js::Value(_) => {
            return Err(BoulderStateError::InvalidShape {
                path,
                reason: "root is not a JSON object",
            });
        }
    };
    if state.is_empty() {
        return Ok(None);
    }
    if !has_valid_works(&state) {
        return Err(BoulderStateError::InvalidShape {
            path,
            reason: "`works` is not an object of work objects",
        });
    }

    normalize_state(&mut state);
    if let Some(mirror_work) = select_mirror_work(&state) {
        state.set("active_work_id", mirror_work.field("work_id"));
        project_work_to_mirror(&mut state, &mirror_work);
    }
    Ok(Some(BoulderState::from_doc(state)))
}

/// Shapes of `works` on which the TypeScript reader throws (and reports "no state"):
/// `Object.values(works)` yielding a non-object entry. Arrays of work objects are also
/// rejected here (see `parity.md`).
fn has_valid_works(state: &JsObj) -> bool {
    match state.get("works") {
        None | Some(Js::Value(Value::Null | Value::Bool(_) | Value::Number(_))) => true,
        Some(Js::Value(Value::String(text))) => text.is_empty(),
        Some(Js::Value(Value::Array(items))) => items.is_empty(),
        Some(Js::Object(works)) => works.values().all(|work| matches!(work, Js::Object(_))),
        Some(Js::Undefined | Js::Value(Value::Object(_))) => false,
    }
}

fn normalize_state(state: &mut JsObj) {
    normalize_session_fields(state);

    let sole_session_id = match state.get("session_ids").and_then(Js::as_array) {
        Some(ids) if ids.len() == 1 => ids.first().and_then(Value::as_str).map(str::to_string),
        Some(_) | None => None,
    };
    if let Some(sole) = sole_session_id
        && let Some(origins) = state.get_obj_mut("session_origins")
        && !matches!(origins.get_str(&sole), Some("appended" | "direct"))
    {
        origins.set(&sole, Js::string("direct"));
    }

    if !matches!(state.get("task_sessions"), Some(Js::Object(_))) {
        state.set("task_sessions", Js::Object(JsObj::default()));
    }

    if let Some(works) = state.get_obj_mut("works") {
        for work in works.objects_mut() {
            normalize_session_fields(work);
        }
    }
}

fn normalize_session_fields(target: &mut JsObj) {
    let session_ids: Vec<Value> = target
        .get("session_ids")
        .and_then(Js::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(|id| Value::String(normalize_session_id(id)))
                .collect()
        })
        .unwrap_or_default();
    target.set("session_ids", Js::Value(Value::Array(session_ids)));

    let origins = target
        .get_obj("session_origins")
        .map(|origins| {
            JsObj::from_entries(
                origins
                    .iter()
                    .map(|(id, origin)| (normalize_session_id(id), origin.clone())),
            )
        })
        .unwrap_or_default();
    target.set("session_origins", Js::Object(origins));
}

/// Works of `state`, or the legacy work synthesised from a pre-v2 mirror.
pub fn get_boulder_works(state: &BoulderState) -> Vec<BoulderWorkState> {
    work_docs(&state.doc)
        .into_iter()
        .map(BoulderWorkState::from_doc)
        .collect()
}

pub(crate) fn work_docs(state: &JsObj) -> Vec<JsObj> {
    if state.get_obj("works").is_some() {
        return work_objects(state);
    }
    if matches!(state.get("works"), Some(Js::Value(Value::Array(_)))) {
        return Vec::new();
    }
    let legacy_fields_present = ["active_plan", "plan_name", "started_at"]
        .iter()
        .all(|key| state.field(key).truthy());
    if legacy_fields_present {
        vec![build_work_from_mirror(state)]
    } else {
        Vec::new()
    }
}

fn is_open(work: &JsObj) -> bool {
    !matches!(work.get_str("status"), Some("completed" | "abandoned"))
}

/// Works that are neither completed nor abandoned.
pub fn get_active_works(directory: &Path) -> Result<Vec<BoulderWorkState>, BoulderStateError> {
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(Vec::new());
    };
    Ok(work_docs(&state.doc)
        .into_iter()
        .filter(is_open)
        .map(BoulderWorkState::from_doc)
        .collect())
}

pub fn get_work_by_id(
    directory: &Path,
    work_id: &str,
) -> Result<Option<BoulderWorkState>, BoulderStateError> {
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(None);
    };
    let target = Js::string(work_id);
    Ok(work_docs(&state.doc)
        .into_iter()
        .find(|work| work.field("work_id").strict_eq(&target))
        .map(BoulderWorkState::from_doc))
}

/// First work named `plan_name`, restricted to `worktree_path` when one is given.
pub fn get_work_by_plan_name(
    directory: &Path,
    plan_name: &str,
    options: &WorkLookupOptions,
) -> Result<Option<BoulderWorkState>, BoulderStateError> {
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(None);
    };
    let worktree_filter = options
        .worktree_path
        .as_deref()
        .filter(|path| !path.is_empty());
    Ok(work_docs(&state.doc)
        .into_iter()
        .find(|work| {
            work.field("plan_name").strict_eq(&Js::string(plan_name))
                && worktree_filter
                    .is_none_or(|path| work.field("worktree_path").strict_eq(&Js::string(path)))
        })
        .map(BoulderWorkState::from_doc))
}

fn has_session(doc: &JsObj, session_id: &str) -> bool {
    doc.get("session_ids")
        .and_then(Js::as_array)
        .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(session_id)))
}

/// Most recently updated (or started) work containing the session; ties keep the first
/// work. Falls back to the legacy mirror work when only the root lists the session.
pub fn get_work_for_session(
    directory: &Path,
    session_id: &str,
) -> Result<Option<BoulderWorkState>, BoulderStateError> {
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(None);
    };
    let normalized = normalize_session_id(session_id);
    let mut newest: Option<(JsObj, i64)> = None;
    for work in work_docs(&state.doc) {
        if !has_session(&work, &normalized) {
            continue;
        }
        let work_ms = work_time_ms(&work);
        if newest
            .as_ref()
            .is_none_or(|(_, newest_ms)| work_ms > *newest_ms)
        {
            newest = Some((work, work_ms));
        }
    }
    if let Some((work, _)) = newest {
        return Ok(Some(BoulderWorkState::from_doc(work)));
    }
    Ok(has_session(&state.doc, &normalized)
        .then(|| BoulderWorkState::from_doc(build_work_from_mirror(&state.doc))))
}

/// Open works with their plan progress, for a "resume which work?" picker.
pub fn get_work_resume_options(
    directory: &Path,
) -> Result<Vec<BoulderWorkResumeOption>, BoulderStateError> {
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(Vec::new());
    };
    let active_work_id = state.doc.field("active_work_id");
    Ok(work_docs(&state.doc)
        .into_iter()
        .filter(is_open)
        .map(|doc| {
            let is_current_mirror = active_work_id.strict_eq(&doc.field("work_id"));
            let work = BoulderWorkState::from_doc(doc);
            let progress = get_plan_progress(&resolve_boulder_plan_path_for_work(directory, &work));
            let started_at = work.started_at().unwrap_or_default().to_string();
            let updated_at = match work.doc.coalesce("updated_at") {
                Some(value) => value.as_str().unwrap_or_default().to_string(),
                None => started_at.clone(),
            };
            BoulderWorkResumeOption {
                work_id: work.work_id().unwrap_or_default().to_string(),
                plan_name: work.plan_name().unwrap_or_default().to_string(),
                active_plan: work.active_plan().unwrap_or_default().to_string(),
                worktree_path: work.worktree_path().map(str::to_string),
                status: work.status().unwrap_or(BoulderWorkStatus::Active),
                started_at,
                updated_at,
                ended_at: work.ended_at().map(str::to_string),
                elapsed_ms: work.elapsed_ms(),
                session_count: work.session_ids().len(),
                progress,
                is_current_mirror,
            }
        })
        .collect())
}

/// Task session of the active work, falling back to the root `task_sessions` map.
pub fn get_task_session_state(
    directory: &Path,
    task_key: &str,
) -> Result<Option<TaskSessionState>, BoulderStateError> {
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(None);
    };
    let active_work_id = state.doc.field("active_work_id");
    if active_work_id.truthy()
        && let Some(session) = state
            .doc
            .get_obj("works")
            .and_then(|works| works.get_obj(&active_work_id.to_js_string()))
            .and_then(|work| work.get_obj("task_sessions"))
            .and_then(|sessions| sessions.get_obj(task_key))
    {
        return Ok(Some(TaskSessionState::from_doc(session.clone())));
    }
    Ok(state
        .doc
        .get_obj("task_sessions")
        .and_then(|sessions| sessions.get_obj(task_key))
        .cloned()
        .map(TaskSessionState::from_doc))
}
