//! Attaching sessions to works (`storage/session.ts`).

use std::path::Path;

use serde_json::Value;

use crate::error::BoulderStateError;
use crate::js::{Js, JsObj};
use crate::records::BoulderState;
use crate::shared::{find_work, normalize_session_id, project_work_to_mirror, works_by_id};
use crate::storage::read_state::{read_boulder_state, work_docs};
use crate::storage::write_state::write_boulder_state;
use crate::time::now_iso_string;
use crate::types::BoulderSessionOrigin;

/// Attach a session to the active work (or, for a legacy mirror-only state, to the root)
/// and persist. `Ok(None)` when there is no state.
pub fn append_session_id(
    directory: &Path,
    session_id: &str,
    origin: BoulderSessionOrigin,
) -> Result<Option<BoulderState>, BoulderStateError> {
    let normalized = normalize_session_id(session_id);
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(None);
    };
    if let Some(active_work_id) = state.active_work_id().filter(|id| !id.is_empty()) {
        return append_session_id_for_work(directory, active_work_id, &normalized, origin);
    }

    let mut doc = state.doc;
    let listed = doc
        .get("session_ids")
        .and_then(Js::as_array)
        .is_some_and(|ids| {
            ids.iter()
                .any(|id| id.as_str() == Some(normalized.as_str()))
        });
    let mut origins = doc.get_obj("session_origins").cloned().unwrap_or_default();
    if listed {
        if origins.get(&normalized).is_some_and(Js::truthy) {
            return Ok(Some(BoulderState::from_doc(doc)));
        }
        origins.set(&normalized, Js::string(origin.as_str()));
        doc.set("session_origins", Js::Object(origins));
    } else {
        let mut ids = doc
            .get("session_ids")
            .and_then(Js::as_array)
            .cloned()
            .unwrap_or_default();
        ids.push(Value::String(normalized.clone()));
        origins.set(&normalized, Js::string(origin.as_str()));
        doc.set("session_ids", Js::Value(Value::Array(ids)));
        doc.set("session_origins", Js::Object(origins));
    }
    let state = BoulderState::from_doc(doc);
    write_boulder_state(directory, &state)?;
    Ok(Some(state))
}

/// Attach a session to `work_id` (recording `origin`, overwriting a previous one) and
/// persist. `Ok(None)` when there is no state or no such work.
pub fn append_session_id_for_work(
    directory: &Path,
    work_id: &str,
    session_id: &str,
    origin: BoulderSessionOrigin,
) -> Result<Option<BoulderState>, BoulderStateError> {
    let normalized = normalize_session_id(session_id);
    let Some(state) = read_boulder_state(directory)? else {
        return Ok(None);
    };
    let works = work_docs(&state.doc);
    let Some(target) = find_work(&works, &Js::string(work_id)) else {
        return Ok(None);
    };

    let mut updated = target.clone();
    let mut ids = updated
        .get("session_ids")
        .and_then(Js::as_array)
        .cloned()
        .unwrap_or_default();
    if !ids
        .iter()
        .any(|id| id.as_str() == Some(normalized.as_str()))
    {
        ids.push(Value::String(normalized.clone()));
    }
    updated.set("session_ids", Js::Value(Value::Array(ids)));
    let mut origins: JsObj = updated.field("session_origins").spread();
    origins.set(&normalized, Js::string(origin.as_str()));
    updated.set("session_origins", Js::Object(origins));
    updated.set("updated_at", Js::string(&now_iso_string()));

    let mut all_works = works_by_id(&works);
    all_works.set(work_id, Js::Object(updated.clone()));
    let mut next_state = state.doc;
    next_state.set("schema_version", Js::int(2));
    next_state.set("works", Js::Object(all_works));
    if next_state
        .field("active_work_id")
        .strict_eq(&Js::string(work_id))
    {
        project_work_to_mirror(&mut next_state, &updated);
    }
    let next_state = BoulderState::from_doc(next_state);
    write_boulder_state(directory, &next_state)?;
    Ok(Some(next_state))
}
