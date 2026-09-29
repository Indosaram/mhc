//! Byte-compatibility with files the TypeScript package writes. Every `*.rewritten.json`
//! / `*.selected*.json` fixture was produced by running the TypeScript
//! `readBoulderState` + `writeBoulderState` / `selectActiveWork` on the paired input.

use std::path::Path;

use boulder_state::{
    BoulderStateError, get_work_by_id, read_boulder_state, select_active_work, write_boulder_state,
};
use pretty_assertions::assert_eq;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).expect("fixture")
}

fn stage(content: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(directory.path().join(".omo")).expect("create .omo");
    std::fs::write(directory.path().join(".omo/boulder.json"), content).expect("write");
    directory
}

fn written(directory: &tempfile::TempDir) -> String {
    std::fs::read_to_string(directory.path().join(".omo/boulder.json")).expect("read back")
}

fn rewrite(content: &str) -> String {
    let directory = stage(content);
    let state = read_boulder_state(directory.path())
        .expect("readable")
        .expect("state present");
    write_boulder_state(directory.path(), &state).expect("write");
    written(&directory)
}

#[test]
fn real_project_boulder_json_round_trips_to_the_typescript_bytes() {
    // given
    let input = fixture("project-boulder.json");

    // when
    let output = rewrite(&input);

    // then
    assert_eq!(output, fixture("project-boulder.rewritten.json"));
}

#[test]
fn real_project_boulder_json_rewrite_is_a_fixed_point() {
    // given
    let once = fixture("project-boulder.rewritten.json");

    // when
    let twice = rewrite(&once);

    // then
    assert_eq!(twice, once);
}

#[test]
fn real_project_boulder_json_exposes_its_active_work() {
    // given
    let directory = stage(&fixture("project-boulder.json"));

    // when
    let work = get_work_by_id(directory.path(), "company-onboarding-messenger-notion")
        .expect("readable")
        .expect("work present");

    // then
    assert_eq!(
        (work.plan_name(), work.session_ids(), work.worktree_path()),
        (
            Some("company-onboarding-messenger-notion"),
            vec!["senpi:01a0d321-f242-7f51-bb98-3cb6977b6e43".to_string()],
            None
        )
    );
}

#[test]
fn multi_work_state_rewrite_matches_typescript_bytes() {
    // given
    let input = fixture("multi-work.json");

    // when
    let output = rewrite(&input);

    // then
    assert_eq!(output, fixture("multi-work.rewritten.json"));
}

#[test]
fn selecting_a_numeric_work_id_matches_typescript_bytes() {
    // given
    let directory = stage(&fixture("multi-work.json"));

    // when
    let state = select_active_work(directory.path(), "7").expect("writable");

    // then
    assert_eq!(
        state
            .and_then(|s| s.active_work_id().map(str::to_string))
            .as_deref(),
        Some("7")
    );
    assert_eq!(written(&directory), fixture("multi-work.selected-7.json"));
}

#[test]
fn selecting_the_legacy_work_of_a_mirror_only_state_matches_typescript_bytes() {
    // given
    let directory = stage(&fixture("legacy-mirror.json"));

    // when
    let state = select_active_work(directory.path(), "old-legacy").expect("writable");

    // then
    assert!(state.is_some());
    assert_eq!(written(&directory), fixture("legacy-mirror.selected.json"));
}

#[test]
fn corrupt_files_are_errors_and_never_panic() {
    // given
    let cases = [
        ("", "json"),
        ("{\"works\":", "json"),
        ("\u{0}\u{1}garbage", "json"),
        ("42", "shape"),
        ("\"text\"", "shape"),
        ("{\"works\":{\"a\":1}}", "shape"),
    ];

    for (content, expected_kind) in cases {
        let directory = stage(content);

        // when
        let result = read_boulder_state(directory.path());

        // then
        let error = result.expect_err(content);
        let kind = match &error {
            BoulderStateError::InvalidJson { .. } => "json",
            BoulderStateError::InvalidShape { .. } => "shape",
            BoulderStateError::Read { .. }
            | BoulderStateError::Write { .. }
            | BoulderStateError::Remove { .. } => "io",
        };
        assert_eq!((content, kind), (content, expected_kind));
        assert_eq!(error.path(), directory.path().join(".omo/boulder.json"));
    }
}

#[test]
fn wrongly_typed_fields_are_tolerated_like_the_typescript_reader() {
    // given
    let directory = stage(
        r#"{"active_plan":5,"plan_name":null,"started_at":"x","session_ids":"nope","session_origins":[],"task_sessions":7,"works":null}"#,
    );

    // when
    let state = read_boulder_state(directory.path())
        .expect("readable")
        .expect("state present");

    // then
    assert_eq!(
        state.to_json_value_for_test(),
        serde_json::json!({
            "active_plan": 5,
            "plan_name": null,
            "started_at": "x",
            "session_ids": [],
            "session_origins": {},
            "task_sessions": {},
            "works": null,
        })
    );
}

trait JsonView {
    fn to_json_value_for_test(&self) -> serde_json::Value;
}

impl JsonView for boulder_state::BoulderState {
    fn to_json_value_for_test(&self) -> serde_json::Value {
        serde_json::from_str(&self.to_json_string_pretty()).expect("valid json")
    }
}
