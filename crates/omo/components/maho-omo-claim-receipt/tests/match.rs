//! Port of `claim-receipt/match.test.ts`.

use crate::support::{
    answer, edit_result, eval_run, failing_test, passing_test, tool_call, tool_result, user_input,
    CellFixture,
};
use maho_omo_claim_receipt::classify::classify_claims;
use maho_omo_claim_receipt::evidence::build_timeline;
use maho_omo_claim_receipt::r#match::{match_claims, ClaimStatus, ClaimVerdict, UnsupportedReason};
use serde_json::{json, Value};

fn verdict_for(before: Vec<Value>, text: &str, after: Vec<Value>) -> ClaimVerdict {
    let answer_at = before.len() as f64 + 1.0;
    let mut entries = vec![user_input("fix it")];
    entries.extend(before);
    entries.push(answer(text));
    entries.extend(after);
    let timeline = build_timeline(&entries);
    let claims = classify_claims(text);
    match_claims(&timeline, &claims, answer_at)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("one verdict for {text:?}"))
}

fn unknown_run(call_id: &str) -> Vec<Value> {
    eval_run(
        call_id,
        &CellFixture {
            code: "await sh(`bun test`)",
            output: "running",
            status: None,
        },
        Vec::new(),
    )
}

#[test]
fn a_claim_before_any_check_is_unsupported() {
    let verdict = verdict_for(edit_result("e1", "src/a.ts"), "All tests pass.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Unsupported);
    assert_eq!(verdict.reason, Some(UnsupportedReason::NoCheck));
}

#[test]
fn a_claim_after_a_failed_check_is_unsupported() {
    let before = [edit_result("e1", "src/a.ts"), failing_test("t1")].concat();
    let verdict = verdict_for(before, "All tests pass.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Unsupported);
    assert_eq!(verdict.reason, Some(UnsupportedReason::LastCheckFailed));
}

#[test]
fn a_claim_after_a_passing_check_is_supported_with_that_receipt() {
    let before = [edit_result("e1", "src/a.ts"), passing_test("t1")].concat();
    let verdict = verdict_for(before, "All tests pass.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Supported);
    let receipt = verdict.receipt.expect("receipt");
    assert_eq!(receipt.kind.as_str(), "test");
    assert_eq!(receipt.outcome.as_str(), "passed");
}

#[test]
fn an_edit_after_a_passing_check_makes_the_claim_stale() {
    let before = [passing_test("t1"), edit_result("e1", "src/a.ts")].concat();
    let verdict = verdict_for(before, "All tests pass.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Unsupported);
    assert_eq!(verdict.reason, Some(UnsupportedReason::StaleAfterEdit));
    let invalidated = verdict.invalidated_by.expect("invalidated by");
    assert_eq!(invalidated.paths, vec!["src/a.ts".to_owned()]);
}

#[test]
fn a_later_failure_outweighs_an_earlier_pass() {
    let before = [passing_test("t1"), failing_test("t2")].concat();
    let verdict = verdict_for(before, "All tests pass.", Vec::new());
    assert_eq!(verdict.reason, Some(UnsupportedReason::LastCheckFailed));
}

#[test]
fn a_pass_after_the_claim_cannot_erase_the_failure_visible_when_it_was_made() {
    let verdict = verdict_for(
        failing_test("t1"),
        "All tests pass.",
        passing_test("t2"),
    );
    assert_eq!(verdict.status, ClaimStatus::Unsupported);
    assert_eq!(verdict.reason, Some(UnsupportedReason::LastCheckFailed));
}

#[test]
fn a_notes_only_edit_after_a_pass_keeps_the_claim_supported() {
    let before = [passing_test("t1"), edit_result("e1", "notes/RESULT.md")].concat();
    let verdict = verdict_for(before, "All tests pass.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Supported);
}

#[test]
fn a_later_unknown_check_does_not_hide_an_earlier_visible_failure() {
    let before = [failing_test("t1"), unknown_run("t2")].concat();
    let verdict = verdict_for(before, "All tests pass.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Unsupported);
    assert_eq!(verdict.reason, Some(UnsupportedReason::LastCheckFailed));
}

#[test]
fn a_quoted_result_after_a_later_unreadable_run_is_not_contradicted_by_the_earlier_failure() {
    let before = [failing_test("t1"), unknown_run("t2")].concat();
    let verdict = verdict_for(before, "The rerun shows 343 pass, 0 fail.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Unknown);
}

#[test]
fn an_unknown_check_after_a_pass_stays_unknown() {
    let before = [passing_test("t1"), unknown_run("t2")].concat();
    let verdict = verdict_for(before, "All tests pass.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Unknown);
}

#[test]
fn a_claim_whose_kind_of_check_never_ran_here_is_unknown_while_a_done_claim_accepts_any_check() {
    let typecheck_only = eval_run(
        "k1",
        &CellFixture {
            code: "await sh(`tsgo --noEmit`)",
            output: "TYPECHECK_EXIT=0",
            status: None,
        },
        Vec::new(),
    );
    let before = [edit_result("e1", "src/a.ts"), typecheck_only.clone()].concat();
    assert_eq!(
        verdict_for(before, "All tests pass.", Vec::new()).status,
        ClaimStatus::Unknown
    );
    let before = [edit_result("e1", "src/a.ts"), typecheck_only].concat();
    assert_eq!(
        verdict_for(before, "Done.", Vec::new()).status,
        ClaimStatus::Supported
    );
}

#[test]
fn a_done_claim_with_no_code_edit_is_not_a_code_completion_claim() {
    assert_eq!(
        verdict_for(Vec::new(), "조사 완료했습니다.", Vec::new()).status,
        ClaimStatus::Unknown
    );
}

#[test]
fn a_concrete_result_report_with_no_recognised_check_is_unknown_but_a_visible_contradiction_flags_it() {
    assert_eq!(
        verdict_for(
            edit_result("e1", "src/a.ts"),
            "The rerun exited 0 with 343 passing.",
            Vec::new()
        )
        .status,
        ClaimStatus::Unknown
    );
    let before = [edit_result("e1", "src/a.ts"), failing_test("t1")].concat();
    let verdict = verdict_for(before, "The rerun exited 0 with 343 passing.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Unsupported);
    assert_eq!(verdict.reason, Some(UnsupportedReason::LastCheckFailed));
}

#[test]
fn a_file_read_does_not_count_as_a_check_for_test_done_or_build_claims() {
    let read = vec![
        tool_call("k1", "read", json!({ "path": "src/a.ts" })),
        tool_result("k1", "read", "code", json!({}), false),
    ];
    let before = [edit_result("e1", "src/a.ts"), read.clone()].concat();
    let verdict = verdict_for(before, "All tests pass.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Unsupported);
    assert_eq!(verdict.reason, Some(UnsupportedReason::NoCheck));
    let before = [read, edit_result("e1", "src/a.ts")].concat();
    let verdict = verdict_for(before, "Done.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Unsupported);
    assert_eq!(verdict.reason, Some(UnsupportedReason::NoCheck));
}

#[test]
fn an_exists_claim_is_supported_by_a_lookup_and_never_failed_by_an_unrelated_check() {
    let read = vec![
        tool_call("k1", "read", json!({ "path": "src/config.ts" })),
        tool_result("k1", "read", "flag", json!({}), false),
    ];
    let verdict = verdict_for(read.clone(), "That flag already exists in config.ts.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Supported);
    assert_eq!(verdict.receipt.expect("receipt").kind.as_str(), "lookup");
    assert_eq!(
        verdict_for(
            failing_test("t1"),
            "That flag already exists in config.ts.",
            Vec::new()
        )
        .status,
        ClaimStatus::Unknown
    );
    let verdict = verdict_for(Vec::new(), "That flag already exists in config.ts.", Vec::new());
    assert_eq!(verdict.status, ClaimStatus::Unsupported);
    assert_eq!(verdict.reason, Some(UnsupportedReason::NoCheck));
}

#[test]
fn a_works_correctly_claim_with_no_code_edit_in_the_session_is_not_flagged() {
    assert_eq!(
        verdict_for(Vec::new(), "The parser works correctly.", Vec::new()).status,
        ClaimStatus::Unknown
    );
    let verdict = verdict_for(
        edit_result("e1", "src/a.ts"),
        "The parser works correctly.",
        Vec::new(),
    );
    assert_eq!(verdict.status, ClaimStatus::Unsupported);
    assert_eq!(verdict.reason, Some(UnsupportedReason::NoCheck));
}

#[test]
fn an_admitted_claim_is_recorded_as_admitted_never_unsupported() {
    let verdict = verdict_for(
        edit_result("e1", "src/a.ts"),
        "I have not run the tests; they should pass.",
        Vec::new(),
    );
    assert_eq!(verdict.status, ClaimStatus::Admitted);
}
