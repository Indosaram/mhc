//! Writing and lifecycle transitions of the state document (`storage/write-state.ts`).

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;

use serde_json::Value;

use crate::error::BoulderStateError;
use crate::js::{Js, JsObj};
use crate::records::BoulderState;
use crate::shared::{
    find_work, get_elapsed_ms, normalize_session_id, project_work_to_mirror, restore_demoted_work,
    spread_array, spread_or_empty, works_by_id,
};
use crate::storage::path::get_boulder_file_path;
use crate::storage::plan_progress::get_plan_name;
use crate::storage::read_state::{read_boulder_state, work_docs};
use crate::time::now_iso_string;
use crate::types::{BoulderWorkInput, CompleteBoulderInput, WorkOwner};

const GITIGNORE_CONTENT: &str = "*\n!/rules/\n!/rules/**\n";

/// Persist `state` as `JSON.stringify(state, null, 2)`, first syncing the active work
/// entry from the root mirror. Creating `.omo/` also writes its self-ignoring
/// `.gitignore`.
pub fn write_boulder_state(
    directory: &Path,
    state: &BoulderState,
) -> Result<(), BoulderStateError> {
    let path = get_boulder_file_path(directory);
    let write_error = |path: &Path| {
        let path = path.to_path_buf();
        move |source| BoulderStateError::Write { path, source }
    };
    if let Some(dir) = path.parent()
        && !dir.exists()
    {
        std::fs::create_dir_all(dir).map_err(write_error(dir))?;
        let gitignore = dir.join(".gitignore");
        std::fs::write(&gitignore, GITIGNORE_CONTENT).map_err(write_error(&gitignore))?;
    }

    let mut state_to_write = state.doc.clone();
    let active_work_id = state_to_write.field("active_work_id");
    if state_to_write.field("works").truthy() && active_work_id.truthy() {
        let key = active_work_id.to_js_string();
        if let Some(active_work) = state_to_write
            .get_obj("works")
            .and_then(|works| works.get_obj(&key))
        {
            let synced =
                sync_work_from_mirror(&state_to_write, active_work.clone()).ok_or_else(|| {
                    BoulderStateError::InvalidShape {
                        path: path.clone(),
                        reason: "`session_ids` of the state is not an array",
                    }
                })?;
            let mut works = state_to_write.field("works").spread();
            works.set(&key, Js::Object(synced));
            state_to_write.set("works", Js::Object(works));
        }
    }

    std::fs::write(&path, state_to_write.stringify_pretty()).map_err(write_error(&path))
}

fn sync_work_from_mirror(state: &JsObj, mut work: JsObj) -> Option<JsObj> {
    for key in [
        "active_plan",
        "plan_name",
        "status",
        "started_at",
        "ended_at",
        "elapsed_ms",
        "updated_at",
    ] {
        work.set(key, state.field(key));
    }
    let session_ids = spread_array(state.get("session_ids"))?;
    work.set("session_ids", Js::Value(Value::Array(session_ids)));
    work.set(
        "session_origins",
        spread_or_empty(&state.field("session_origins")),
    );
    work.set("agent", state.field("agent"));
    work.set("worktree_path", state.field("worktree_path"));
    work.set(
        "task_sessions",
        spread_or_empty(&state.field("task_sessions")),
    );
    Some(work)
}

/// Remove the state file; a missing file is success.
pub fn clear_boulder_state(directory: &Path) -> Result<(), BoulderStateError> {
    let path = get_boulder_file_path(directory);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BoulderStateError::Remove { path, source }),
    }
}

/// `<slug>-<8 hex digits>`, where the slug is the lower-cased plan name with every run of
/// non `[a-z0-9]` characters collapsed to `-` (or `work` when nothing remains).
pub fn generate_work_id(plan_name: &str) -> String {
    let lowered = plan_name.trim().to_lowercase();
    let mut slug = String::with_capacity(lowered.len());
    for character in lowered.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            slug.push(character);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "work" } else { slug };
    format!("{slug}-{:08x}", random_u32_below_max())
}

/// `Math.floor(Math.random() * 0xffffffff)`: uniform in `0..0xffffffff`.
fn random_u32_below_max() -> u32 {
    let mut hasher = RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    hasher.write_u128(nanos);
    let bits = hasher.finish();
    u32::try_from(bits % u64::from(u32::MAX)).unwrap_or_default()
}

fn new_work(
    work_id: &str,
    plan_path: &str,
    session_id: &str,
    owner: &WorkOwner,
    started_at: &str,
) -> JsObj {
    let mut work = JsObj::from_entries([
        ("work_id".to_string(), Js::string(work_id)),
        ("active_plan".to_string(), Js::string(plan_path)),
        (
            "plan_name".to_string(),
            Js::string(&get_plan_name(plan_path)),
        ),
        ("status".to_string(), Js::string("active")),
        ("started_at".to_string(), Js::string(started_at)),
        ("updated_at".to_string(), Js::string(started_at)),
        (
            "session_ids".to_string(),
            Js::Value(Value::Array(vec![Value::String(session_id.to_string())])),
        ),
        ("session_origins".to_string(), direct_origin(session_id)),
    ]);
    set_owner(&mut work, owner);
    work.set("task_sessions", Js::Object(JsObj::default()));
    work
}

fn direct_origin(session_id: &str) -> Js {
    Js::Object(JsObj::from_entries([(
        session_id.to_string(),
        Js::string("direct"),
    )]))
}

fn set_owner(target: &mut JsObj, owner: &WorkOwner) {
    if let Some(agent) = &owner.agent {
        target.set("agent", Js::string(agent));
    }
    if let Some(worktree_path) = &owner.worktree_path {
        target.set("worktree_path", Js::string(worktree_path));
    }
}

/// A fresh schema-v2 state with one active work for `plan_path`. Nothing is written.
pub fn create_boulder_state(plan_path: &str, session_id: &str, owner: &WorkOwner) -> BoulderState {
    let started_at = now_iso_string();
    let session_id = normalize_session_id(session_id);
    let work_id = generate_work_id(&get_plan_name(plan_path));
    let work = new_work(&work_id, plan_path, &session_id, owner, &started_at);
    let mut state = JsObj::from_entries([
        ("schema_version".to_string(), Js::int(2)),
        ("active_work_id".to_string(), Js::string(&work_id)),
        (
            "works".to_string(),
            Js::Object(JsObj::from_entries([(work_id.clone(), Js::Object(work))])),
        ),
        ("active_plan".to_string(), Js::string(plan_path)),
        ("started_at".to_string(), Js::string(&started_at)),
        ("status".to_string(), Js::string("active")),
        ("updated_at".to_string(), Js::string(&started_at)),
        (
            "session_ids".to_string(),
            Js::Value(Value::Array(vec![Value::String(session_id.clone())])),
        ),
        ("session_origins".to_string(), direct_origin(&session_id)),
        (
            "plan_name".to_string(),
            Js::string(&get_plan_name(plan_path)),
        ),
        ("task_sessions".to_string(), Js::Object(JsObj::default())),
    ]);
    set_owner(&mut state, owner);
    BoulderState::from_doc(state)
}

/// Make `work_id` the active work and persist; `Ok(None)` when there is no state or no
/// such work.
pub fn select_active_work(
    directory: &Path,
    work_id: &str,
) -> Result<Option<BoulderState>, BoulderStateError> {
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(None);
    };
    let works = work_docs(&state.doc);
    let Some(selected_work) = find_work(&works, &Js::string(work_id)).cloned() else {
        return Ok(None);
    };
    let next_work = restore_demoted_work(&selected_work);
    let mut all_works = works_by_id(&works);
    all_works.set(work_id, Js::Object(next_work.clone()));
    let mut next_state = state.doc;
    next_state.set("schema_version", Js::int(2));
    next_state.set("active_work_id", Js::string(work_id));
    next_state.set("works", Js::Object(all_works));
    project_work_to_mirror(&mut next_state, &next_work);
    let next_state = BoulderState::from_doc(next_state);
    write_boulder_state(directory, &next_state)?;
    Ok(Some(next_state))
}

/// Add a new active work for `input.plan_path` next to the existing works and persist;
/// `Ok(None)` when there is no state to add to.
pub fn add_boulder_work(
    directory: &Path,
    input: &BoulderWorkInput,
) -> Result<Option<BoulderState>, BoulderStateError> {
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(None);
    };
    let work_id = generate_work_id(&get_plan_name(&input.plan_path));
    let started_at = input.started_at.clone().unwrap_or_else(now_iso_string);
    let session_id = normalize_session_id(&input.session_id);
    let next_work = new_work(
        &work_id,
        &input.plan_path,
        &session_id,
        &input.owner,
        &started_at,
    );

    let mut works = works_by_id(&work_docs(&state.doc));
    works.set(&work_id, Js::Object(next_work.clone()));
    let mut next_state = state.doc;
    next_state.set("schema_version", Js::int(2));
    next_state.set("works", Js::Object(works));
    next_state.set("active_work_id", Js::string(&work_id));
    project_work_to_mirror(&mut next_state, &next_work);
    let next_state = BoulderState::from_doc(next_state);
    write_boulder_state(directory, &next_state)?;
    Ok(Some(next_state))
}

/// Where a work lives inside the document.
pub(crate) enum WorkSlot {
    /// Under this key of the `works` map.
    Stored(String),
    /// Synthesised from a legacy (pre-v2) root mirror.
    Legacy(JsObj),
}

/// `state.works?.[workId] ?? getBoulderWorks(state).find((w) => w.work_id === workId)`.
pub(crate) fn locate_work(state: &JsObj, work_id: &Js) -> Option<WorkSlot> {
    let key = work_id.to_js_string();
    if let Some(works) = state.get_obj("works") {
        if works.get_obj(&key).is_some() {
            return Some(WorkSlot::Stored(key));
        }
        return works
            .iter()
            .find(|(_, work)| {
                work.as_object()
                    .is_some_and(|work| work.field("work_id").strict_eq(work_id))
            })
            .map(|(key, _)| WorkSlot::Stored(key.to_string()));
    }
    work_docs(state)
        .into_iter()
        .find(|work| work.field("work_id").strict_eq(work_id))
        .map(WorkSlot::Legacy)
}

/// Mark a work completed (default: the active work) and persist. An already completed
/// work with `ended_at` and `elapsed_ms` is returned unchanged without writing.
pub fn complete_boulder(
    directory: &Path,
    input: &CompleteBoulderInput,
) -> Result<Option<BoulderState>, BoulderStateError> {
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(None);
    };
    let mut doc = state.doc;
    let target_work_id = match &input.work_id {
        Some(work_id) => Js::string(work_id),
        None => doc.field("active_work_id"),
    };
    if !target_work_id.truthy() {
        return Ok(None);
    }
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

    let already_complete = work.get_str("status") == Some("completed")
        && !matches!(work.field("ended_at"), Js::Undefined)
        && !matches!(work.field("elapsed_ms"), Js::Undefined);
    if already_complete {
        return Ok(Some(BoulderState::from_doc(doc)));
    }

    let ended_at = input.ended_at.clone().unwrap_or_else(now_iso_string);
    let elapsed = get_elapsed_ms(work.get("started_at"), &ended_at);
    work.set("ended_at", Js::string(&ended_at));
    work.set("elapsed_ms", elapsed);
    work.set("status", Js::string("completed"));
    work.set("updated_at", Js::string(&now_iso_string()));

    if let WorkSlot::Stored(key) = &slot
        && let Some(works) = doc.get_obj_mut("works")
    {
        works.set(key, Js::Object(work.clone()));
    }
    if doc.field("active_work_id").strict_eq(&target_work_id) {
        project_work_to_mirror(&mut doc, &work);
    }
    let state = BoulderState::from_doc(doc);
    write_boulder_state(directory, &state)?;
    Ok(Some(state))
}
