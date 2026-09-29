//! Port of `src/plan-checklist.test.ts`.

use boulder_state::{PlanChecklist, get_plan_checklist, parse_plan_checklist};
use pretty_assertions::assert_eq;

const SCAFFOLD_PLAN_MARKDOWN: &str = "# Scaffold Parser Parity

## Todos
- [ ] 1. Implement checklist parser parity
  - [ ] Nested acceptance detail
- [x] 2. Preserve completed implementation rows
- [ ] Missing numeric prefix must be ignored

## Acceptance Criteria
- [ ] Outside tracked sections must be ignored

## Final verification wave
- [ ] F1. Exercise the Codex Stop surface
- [X] F2. Preserve completed final verification rows
- [ ] 3. Wrong final-wave label must be ignored";

fn checklist(
    completed: usize,
    remaining: usize,
    total: usize,
    next: Option<&str>,
) -> PlanChecklist {
    PlanChecklist {
        total,
        completed,
        remaining,
        next_task_label: next.map(str::to_string),
    }
}

#[test]
fn scaffold_headings_and_canonical_rows_give_structured_totals_and_next_label() {
    // given
    let markdown = SCAFFOLD_PLAN_MARKDOWN;

    // when
    let parsed = parse_plan_checklist(markdown);

    // then
    assert_eq!(
        parsed,
        checklist(2, 2, 4, Some("1. Implement checklist parser parity"))
    );
}

#[test]
fn no_counted_sections_counts_all_top_level_checkboxes() {
    // given
    let markdown = ["# Plan", "- [ ] First", "- [x] Done", "  - [ ] Nested"].join("\n");

    // when
    let parsed = parse_plan_checklist(&markdown);

    // then
    assert_eq!(parsed, checklist(1, 1, 2, Some("First")));
}

#[test]
fn heading_free_legacy_star_checklist_keeps_fallback_behavior() {
    // given
    let markdown = ["# Plan", "* [ ] First", "* [x] Done", "  * [ ] Nested"].join("\n");

    // when
    let parsed = parse_plan_checklist(&markdown);

    // then
    assert_eq!(parsed, checklist(1, 1, 2, Some("First")));
}

#[test]
fn completed_implementation_and_pending_final_verifier_makes_verifier_next() {
    // given
    let markdown = [
        "## Todos",
        "- [x] 1. Implementation complete",
        "## Final verification wave",
        "- [ ] F1. Verify the result",
    ]
    .join("\n");

    // when
    let parsed = parse_plan_checklist(&markdown);

    // then
    assert_eq!(parsed, checklist(1, 1, 2, Some("F1. Verify the result")));
}

#[test]
fn noncanonical_structured_rows_only_count_exact_positive_number_grammar() {
    // given
    let markdown = [
        "## Todos",
        "- [ ] 0. Zero is invalid",
        "- [ ] 01. Leading zero is invalid",
        "* [ ] 2. Star marker is invalid",
        "-[ ] 3. Missing spaces are invalid",
        "- [ ] 4. Canonical implementation",
        "## Final verification wave",
        "- [ ] F0. Zero final verifier is invalid",
        "- [ ] F01. Leading zero final verifier is invalid",
        "- [x] F2. Canonical final verifier",
    ]
    .join("\n");

    // when
    let parsed = parse_plan_checklist(&markdown);

    // then
    assert_eq!(
        parsed,
        checklist(1, 1, 2, Some("4. Canonical implementation"))
    );
}

#[test]
fn fenced_examples_and_higher_level_heading_are_out_of_section_scope() {
    // given
    let markdown = [
        "## Todos",
        "- [ ] 1. Counted implementation",
        "```md",
        "- [ ] 2. Fenced example",
        "```",
        "# Appendix",
        "- [ ] 3. Appendix checkbox",
        "## Final verification wave",
        "- [x] F1. Counted verifier",
    ]
    .join("\n");

    // when
    let parsed = parse_plan_checklist(&markdown);

    // then
    assert_eq!(
        parsed,
        checklist(1, 1, 2, Some("1. Counted implementation"))
    );
}

#[test]
fn child_heading_inside_todos_keeps_rows_in_parent_section() {
    // given
    let markdown = [
        "## TODOs",
        "- [x] 1. First task",
        "### Notes",
        "- [ ] 2. Second task",
    ]
    .join("\n");

    // when
    let parsed = parse_plan_checklist(&markdown);

    // then
    assert_eq!(parsed, checklist(1, 1, 2, Some("2. Second task")));
}

#[test]
fn four_backtick_fence_is_not_closed_by_shorter_fences() {
    // given
    let markdown = [
        "## Todos",
        "- [x] 1. Counted implementation",
        "````md",
        "```ts",
        "- [ ] 2. Fenced example",
        "```",
        "````",
        "## Final verification wave",
        "- [ ] F1. Counted verifier",
    ]
    .join("\n");

    // when
    let parsed = parse_plan_checklist(&markdown);

    // then
    assert_eq!(parsed, checklist(1, 1, 2, Some("F1. Counted verifier")));
}

#[test]
fn atx_closing_markers_keep_headings_structured() {
    // given
    let markdown = [
        "## TODOs ##",
        "- [ ] 1. Implement",
        "## Final Verification Wave ###",
        "- [ ] F1. Verify",
    ]
    .join("\n");

    // when
    let parsed = parse_plan_checklist(&markdown);

    // then
    assert_eq!(parsed, checklist(0, 2, 2, Some("1. Implement")));
}

#[test]
fn inline_backtick_code_does_not_open_a_fence() {
    // given
    let markdown = ["## TODOs", "```example```", "- [ ] 1. Implement"].join("\n");

    // when
    let parsed = parse_plan_checklist(&markdown);

    // then
    assert_eq!(parsed.total, 1);
    assert_eq!(parsed.next_task_label.as_deref(), Some("1. Implement"));
}

#[test]
fn heading_free_fenced_checkbox_is_ignored_by_legacy_fallback() {
    // given
    let markdown = ["````md", "- [ ] 1. Example only", "````"].join("\n");

    // when
    let parsed = parse_plan_checklist(&markdown);

    // then
    assert_eq!(parsed, checklist(0, 0, 0, None));
}

#[test]
fn missing_plan_path_gives_empty_checklist() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");

    // when
    let parsed = get_plan_checklist(&directory.path().join("missing.md"));

    // then
    assert_eq!(parsed, checklist(0, 0, 0, None));
}

#[test]
fn complete_plan_has_no_next_task() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    let plan_path = directory.path().join("plan.md");
    std::fs::write(
        &plan_path,
        "## Todos\n- [x] 1. First\n- [X] 2. Second\n## Final verification wave\n- [x] F1. Final\n",
    )
    .expect("write plan");

    // when
    let parsed = get_plan_checklist(&plan_path);

    // then
    assert_eq!(parsed, checklist(3, 0, 3, None));
}
