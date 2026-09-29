//! Port of `src/plan-progress.test.ts`, plus `readCurrentTopLevelTask` coverage.

use boulder_state::{
    PlanProgress, TopLevelTaskRef, TopLevelTaskSection, get_plan_progress,
    read_current_top_level_task,
};
use pretty_assertions::assert_eq;

fn write_plan(directory: &tempfile::TempDir, markdown: &str) -> std::path::PathBuf {
    let plan_path = directory.path().join("plan.md");
    std::fs::write(&plan_path, markdown).expect("write plan");
    plan_path
}

const STRUCTURED_PLAN: [&str; 12] = [
    "# Plan",
    "- [ ] Preamble checkbox",
    "## TODOs",
    "- [x] 1. Completed task",
    "  - [ ] 1.1 Nested task",
    "- [ ] Missing numeric label",
    "- [ ] 2. Remaining task",
    "## Acceptance Criteria",
    "- [ ] Ignored checkbox",
    "## Final Verification Wave",
    "- [X] F1. Verified task",
    "- [ ] F2. Remaining verification",
];

#[test]
fn structured_plan_counts_only_labeled_top_level_todo_and_final_wave_tasks() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    let plan_path = write_plan(&directory, &STRUCTURED_PLAN.join("\n"));

    // when
    let progress = get_plan_progress(&plan_path);

    // then
    assert_eq!(
        progress,
        PlanProgress {
            total: 4,
            completed: 2,
            is_complete: false
        }
    );
}

#[test]
fn simple_plan_without_structured_headings_counts_all_top_level_checkboxes() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    let plan_path = write_plan(
        &directory,
        &[
            "# Plan",
            "- [x] Completed",
            "- [ ] Remaining",
            "  - [ ] Nested ignored by simple fallback",
        ]
        .join("\n"),
    );

    // when
    let progress = get_plan_progress(&plan_path);

    // then
    assert_eq!(
        progress,
        PlanProgress {
            total: 2,
            completed: 1,
            is_complete: false
        }
    );
}

#[test]
fn structured_plan_reports_first_unchecked_top_level_task() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    let plan_path = write_plan(&directory, &STRUCTURED_PLAN.join("\n"));

    // when
    let task = read_current_top_level_task(&plan_path);

    // then
    assert_eq!(
        task,
        Some(TopLevelTaskRef {
            key: "todo:2".to_string(),
            section: TopLevelTaskSection::Todo,
            label: "2".to_string(),
            title: "Remaining task".to_string(),
        })
    );
}

#[test]
fn final_wave_task_key_is_lower_cased() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    let plan_path = write_plan(
        &directory,
        "## TODOs\n- [x] 1. Done\n## Final Verification Wave\n- [ ] f3. Verify\n",
    );

    // when
    let task = read_current_top_level_task(&plan_path);

    // then
    assert_eq!(
        task,
        Some(TopLevelTaskRef {
            key: "final-wave:f3".to_string(),
            section: TopLevelTaskSection::FinalWave,
            label: "f3".to_string(),
            title: "Verify".to_string(),
        })
    );
}

#[test]
fn unstructured_or_missing_plan_has_no_top_level_task() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    let plan_path = write_plan(&directory, "# Plan\n- [ ] First\n");

    // when / then
    assert_eq!(read_current_top_level_task(&plan_path), None);
    assert_eq!(
        read_current_top_level_task(&directory.path().join("missing.md")),
        None
    );
}
