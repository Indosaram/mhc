//! Helpers shared by the storage modules (`storage/shared.ts`).

use serde_json::Value;

use crate::js::{Js, JsObj};
use crate::time::parse_iso_to_millis;
use crate::types::SessionPlatform;

pub(crate) const RESERVED_KEYS: [&str; 3] = ["__proto__", "prototype", "constructor"];

const SESSION_ID_PREFIXES: [&str; 3] = ["codex:", "opencode:", "senpi:"];

/// Prefix a bare session id with the default `opencode` platform.
///
/// Senpi callers must pass pre-prefixed `senpi:<id>` ids to the read APIs, because the
/// default platform stays `opencode` for legacy compatibility.
pub fn normalize_session_id(session_id: &str) -> String {
    normalize_session_id_with_platform(session_id, SessionPlatform::Opencode)
}

/// Prefix a bare session id with `platform`; an existing known prefix always wins.
pub fn normalize_session_id_with_platform(session_id: &str, platform: SessionPlatform) -> String {
    if SESSION_ID_PREFIXES
        .iter()
        .any(|prefix| session_id.starts_with(prefix))
    {
        return session_id.to_string();
    }
    format!("{}:{session_id}", platform.as_str())
}

/// `parseIsoToMs`: falsy or unparsable values yield `None`.
pub(crate) fn parse_iso_to_ms(value: Option<&Js>) -> Option<i64> {
    value.and_then(Js::as_str).and_then(parse_iso_to_millis)
}

/// `getElapsedMs`: `undefined` unless both instants parse.
pub(crate) fn get_elapsed_ms(started_at: Option<&Js>, ended_at: &str) -> Js {
    match (parse_iso_to_ms(started_at), parse_iso_to_millis(ended_at)) {
        (Some(started), Some(ended)) => Js::int(ended - started),
        _ => Js::Undefined,
    }
}

/// Sort key `parseIsoToMs(work.updated_at ?? work.started_at) ?? 0`.
pub(crate) fn work_time_ms(work: &JsObj) -> i64 {
    let stamp = work
        .coalesce("updated_at")
        .or_else(|| work.get("started_at"));
    parse_iso_to_ms(stamp).unwrap_or(0)
}

pub(crate) fn build_work_from_mirror(state: &JsObj) -> JsObj {
    let plan_name = state
        .coalesce("plan_name")
        .cloned()
        .unwrap_or_else(|| state.field("active_plan"));
    let work_id = format!("{}-legacy", plan_name.to_js_string());
    let session_ids = match state.get("session_ids") {
        Some(Js::Value(Value::Array(items))) => items.clone(),
        _ => Vec::new(),
    };
    JsObj::from_entries([
        ("work_id".to_string(), Js::string(&work_id)),
        ("active_plan".to_string(), state.field("active_plan")),
        ("plan_name".to_string(), plan_name),
        ("status".to_string(), state.field("status")),
        ("started_at".to_string(), state.field("started_at")),
        ("ended_at".to_string(), state.field("ended_at")),
        ("elapsed_ms".to_string(), state.field("elapsed_ms")),
        ("updated_at".to_string(), state.field("updated_at")),
        (
            "session_ids".to_string(),
            Js::Value(Value::Array(session_ids)),
        ),
        (
            "session_origins".to_string(),
            state.field("session_origins"),
        ),
        ("agent".to_string(), state.field("agent")),
        ("worktree_path".to_string(), state.field("worktree_path")),
        ("task_sessions".to_string(), state.field("task_sessions")),
    ])
}

/// `[...value]` for the id arrays; `None` when the value is not iterable.
pub(crate) fn spread_array(value: Option<&Js>) -> Option<Vec<Value>> {
    match value {
        Some(Js::Value(Value::Array(items))) => Some(items.clone()),
        Some(Js::Value(Value::String(text))) => Some(
            text.chars()
                .map(|character| Value::String(character.to_string()))
                .collect(),
        ),
        _ => None,
    }
}

/// `value ? { ...value } : {}`.
pub(crate) fn spread_or_empty(value: &Js) -> Js {
    Js::Object(if value.truthy() {
        value.spread()
    } else {
        JsObj::default()
    })
}

pub(crate) fn project_work_to_mirror(state: &mut JsObj, work: &JsObj) {
    for key in [
        "active_plan",
        "plan_name",
        "status",
        "started_at",
        "ended_at",
        "elapsed_ms",
        "updated_at",
    ] {
        state.set(key, work.field(key));
    }
    let session_ids = spread_array(work.get("session_ids")).unwrap_or_default();
    state.set("session_ids", Js::Value(Value::Array(session_ids)));
    state.set(
        "session_origins",
        spread_or_empty(&work.field("session_origins")),
    );
    state.set("agent", work.field("agent"));
    state.set("worktree_path", work.field("worktree_path"));
    state.set(
        "task_sessions",
        spread_or_empty(&work.field("task_sessions")),
    );
}

/// Works of a validated document, in enumeration order.
pub(crate) fn work_objects(state: &JsObj) -> Vec<JsObj> {
    state
        .get_obj("works")
        .map(|works| works.values().filter_map(Js::as_object).cloned().collect())
        .unwrap_or_default()
}

pub(crate) fn select_mirror_work(state: &JsObj) -> Option<JsObj> {
    let mut works = work_objects(state);
    if works.is_empty() {
        return None;
    }

    let active_work_id = state.field("active_work_id");
    if active_work_id.truthy()
        && let Some(matched) = works
            .iter()
            .find(|work| work.field("work_id").strict_eq(&active_work_id))
    {
        return Some(matched.clone());
    }

    works.sort_by_key(|work| std::cmp::Reverse(work_time_ms(work)));
    works.into_iter().next()
}

/// `Object.fromEntries(works.map((work) => [work.work_id, work]))`.
pub(crate) fn works_by_id(works: &[JsObj]) -> JsObj {
    JsObj::from_entries(works.iter().map(|work| {
        (
            work.field("work_id").to_js_string(),
            Js::Object(work.clone()),
        )
    }))
}

/// `works.find((work) => work.work_id === workId)`.
pub(crate) fn find_work<'a>(works: &'a [JsObj], work_id: &Js) -> Option<&'a JsObj> {
    works
        .iter()
        .find(|work| work.field("work_id").strict_eq(work_id))
}
