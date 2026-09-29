//! Port of `src/read-state.test.ts` (`readBoulderState`, `getWorkForSession`).

use std::path::Path;

use boulder_state::{BoulderStateError, get_work_for_session, read_boulder_state};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn write_state(directory: &Path, content: &str) {
    let boulder_directory = directory.join(".omo");
    std::fs::create_dir_all(&boulder_directory).expect("create .omo");
    std::fs::write(boulder_directory.join("boulder.json"), content).expect("write state");
}

fn create_work(work_id: &str, started_at: &str, updated_at: Option<&str>) -> Value {
    let mut work = json!({
        "work_id": work_id,
        "active_plan": format!(".omo/plans/{work_id}.md"),
        "plan_name": work_id,
        "status": "active",
        "started_at": started_at,
    });
    if let Some(updated_at) = updated_at {
        work["updated_at"] = json!(updated_at);
    }
    work["session_ids"] = json!(["opencode:sess-a"]);
    work
}

fn create_state(works: &[Value]) -> String {
    let first = &works[0];
    let works_map: serde_json::Map<String, Value> = works
        .iter()
        .map(|work| {
            (
                work["work_id"].as_str().expect("id").to_string(),
                work.clone(),
            )
        })
        .collect();
    let mut state = json!({
        "schema_version": 2,
        "active_work_id": first["work_id"],
        "works": works_map,
        "active_plan": first["active_plan"],
        "plan_name": first["plan_name"],
        "status": first["status"],
        "started_at": first["started_at"],
    });
    if let Some(updated_at) = first.get("updated_at") {
        state["updated_at"] = updated_at.clone();
    }
    state["session_ids"] = first["session_ids"].clone();
    state["session_origins"] = json!({});
    state["task_sessions"] = json!({});
    state.to_string()
}

fn work_id_for_session(directory: &Path, session_id: &str) -> Option<String> {
    get_work_for_session(directory, session_id)
        .expect("readable state")
        .and_then(|work| work.work_id().map(str::to_string))
}

#[test]
fn no_boulder_file_reads_as_none() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");

    // when
    let state = read_boulder_state(directory.path()).expect("missing file is not an error");

    // then
    assert_eq!(state, None);
}

#[test]
fn malformed_state_json_is_an_invalid_json_error() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    write_state(directory.path(), "{not-json");

    // when
    let result = read_boulder_state(directory.path());

    // then
    assert!(matches!(result, Err(BoulderStateError::InvalidJson { .. })));
}

#[test]
fn non_object_state_json_is_an_invalid_shape_error() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    write_state(directory.path(), "[]");

    // when
    let result = read_boulder_state(directory.path());

    // then
    assert!(matches!(
        result,
        Err(BoulderStateError::InvalidShape { .. })
    ));
}

#[test]
fn empty_object_state_json_reads_as_none() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    write_state(directory.path(), "{}");

    // when
    let state = read_boulder_state(directory.path()).expect("empty object is not an error");

    // then
    assert_eq!(state, None);
}

#[test]
fn multiple_works_for_one_session_returns_newest_updated_work() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    write_state(
        directory.path(),
        &create_state(&[
            create_work(
                "older",
                "2026-06-05T01:00:00.000Z",
                Some("2026-06-05T02:00:00.000Z"),
            ),
            create_work(
                "newest",
                "2026-06-05T01:30:00.000Z",
                Some("2026-06-05T03:00:00.000Z"),
            ),
        ]),
    );

    // when
    let work_id = work_id_for_session(directory.path(), "sess-a");

    // then
    assert_eq!(work_id.as_deref(), Some("newest"));
}

#[test]
fn works_without_updated_times_return_newest_started_work() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    write_state(
        directory.path(),
        &create_state(&[
            create_work("older-start", "2026-06-05T01:00:00.000Z", None),
            create_work("newer-start", "2026-06-05T02:00:00.000Z", None),
        ]),
    );

    // when
    let work_id = work_id_for_session(directory.path(), "opencode:sess-a");

    // then
    assert_eq!(work_id.as_deref(), Some("newer-start"));
}

#[test]
fn identical_sort_times_return_first_inserted_work() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    write_state(
        directory.path(),
        &create_state(&[
            create_work(
                "first",
                "2026-06-05T01:00:00.000Z",
                Some("2026-06-05T02:00:00.000Z"),
            ),
            create_work(
                "second",
                "2026-06-05T01:30:00.000Z",
                Some("2026-06-05T02:00:00.000Z"),
            ),
        ]),
    );

    // when
    let work_id = work_id_for_session(directory.path(), "sess-a");

    // then
    assert_eq!(work_id.as_deref(), Some("first"));
}

#[test]
fn invalid_sort_times_return_first_inserted_work() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    write_state(
        directory.path(),
        &create_state(&[
            create_work("first-invalid", "not-a-date", Some("also-not-a-date")),
            create_work("second-invalid", "still-not-a-date", Some("nope")),
        ]),
    );

    // when
    let work_id = work_id_for_session(directory.path(), "sess-a");

    // then
    assert_eq!(work_id.as_deref(), Some("first-invalid"));
}

#[test]
fn valid_time_wins_over_invalid_time() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    write_state(
        directory.path(),
        &create_state(&[
            create_work("invalid", "not-a-date", Some("also-not-a-date")),
            create_work(
                "valid",
                "2026-06-05T01:00:00.000Z",
                Some("2026-06-05T02:00:00.000Z"),
            ),
        ]),
    );

    // when
    let work_id = work_id_for_session(directory.path(), "sess-a");

    // then
    assert_eq!(work_id.as_deref(), Some("valid"));
}

#[test]
fn matching_mirror_session_without_works_returns_legacy_mirror_work() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    let state = json!({
        "schema_version": 2,
        "active_plan": ".omo/plans/mirror.md",
        "plan_name": "mirror",
        "status": "active",
        "started_at": "2026-06-05T01:00:00.000Z",
        "session_ids": ["opencode:sess-a"],
        "session_origins": { "opencode:sess-a": "direct" },
        "task_sessions": {},
    });
    write_state(directory.path(), &state.to_string());

    // when
    let work_id = work_id_for_session(directory.path(), "sess-a");

    // then
    assert_eq!(work_id.as_deref(), Some("mirror-legacy"));
}
