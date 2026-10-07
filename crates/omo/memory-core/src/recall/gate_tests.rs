use std::collections::BTreeSet;
use std::fs;

use tempfile::TempDir;

use super::{
    InvalidHintReason, PendingNudges, RecallNudge, ValidateNudgesOptions, describe_invalid_hint,
    is_valid_hint, validate_nudges,
};

const PATH: &str = "reference/a.md";

fn nudge(path: &str, hint: &str) -> RecallNudge {
    RecallNudge {
        path: path.to_string(),
        hint: hint.to_string(),
    }
}

fn options(candidates: &[&str], surfaced: &[&str], max_items: usize) -> ValidateNudgesOptions {
    ValidateNudgesOptions {
        candidates: candidates.iter().map(|value| value.to_string()).collect(),
        surfaced: surfaced.iter().map(|value| value.to_string()).collect(),
        max_items,
    }
}

fn single_option() -> ValidateNudgesOptions {
    options(&[PATH], &[], 1)
}

const ADDRESSING_HINTS: [&str; 24] = [
    "You must run the green-main guard before publishing.",
    "Your worktree rebase has to wait for the child task.",
    "The guard is yours to keep green before a publish.",
    "Rebase the worktree yourself once the child task finishes.",
    "Do not rebase this worktree until the child task finishes.",
    "Don't rebase this worktree while a child task writes in it.",
    "Never run the test suite on this machine.",
    "Always push the branch before asking for a remote run.",
    "Make sure the release note is updated first.",
    "Ensure the green-main guard passes before publishing.",
    "Verify these before continuing.",
    "Check the rollout guard before deploying.",
    "Run the remote runner instead of a local suite.",
    "Use the remote runner for every test path.",
    "Read the head-watch note before rebasing.",
    "Stop the rebase while a child task is mid-write.",
    "Avoid rebasing a worktree mid-write.",
    "Remember that the guard stays green before a publish.",
    "Keep the child task alive until it finishes writing.",
    "Prefer the remote runner over a local run.",
    "Skip the local test run on this machine.",
    "Consider queueing the rebase until the child finishes.",
    "\"Do not rebase the worktree while a child task writes in it.\"",
    "  don't rebase the worktree while a child task writes in it.",
];

const KOREAN_ADDRESSING_HINTS: [&str; 12] = [
    "원격 러너로 테스트를 실행하세요",
    "머지 전에 그린 메인 가드를 확인하십시오!",
    "이 머신에서는 테스트를 로컬로 돌리지 마세요.",
    "자식 작업이 끝날 때까지 리베이스하지 마십시오.",
    "커밋 메시지는 파일로 작성해 주세요.",
    "커밋 메시지는 파일로 작성해주세요.",
    "푸시 전에 타입체크를 하라.",
    "푸시 전에 타입체크를 해라.",
    "릴리스 전에 그린 메인 가드를 통과해야 합니다.",
    "원격 러너를 사용하십시요.",
    "브랜치를 강제로 푸시하지 마.",
    "이 머신에서 테스트를 돌리지 마라",
];

const OBSERVATIONAL_HINTS: [&str; 8] = [
    "The release note records that publish must follow the green-main guard.",
    "The head-watch note records that a rebase during a child task's write corrupted its edits.",
    "The note records that the team never runs the suite locally on this machine.",
    "The deploy note records that the guard must always stay green before a publish.",
    "The runbook records that checking the rollout guard is the release step.",
    "Runs of the suite are remote-only according to the stored note.",
    "The stored note keeps the remote runner as the only test path.",
    "The incident note records that reading the head-watch rule came after the corruption.",
];

const KOREAN_OBSERVATIONAL_HINTS: [&str; 3] = [
    "메모는 이 머신에서 테스트를 로컬로 실행하지 않는다고 기록한다.",
    "노트에 적힌 규칙은 자식 작업이 끝날 때까지 리베이스를 미루는 것이다.",
    "저장된 메모에는 커밋 메시지를 파일로 작성한 사례가 있다.",
];

const AUDIT_HINTS: [&str; 2] = [
    "No stored memory clears the bar for this planning step; the transcript already contains the full methodology, QA approach, and rollout.",
    "This memory covers OAuth login prompts and remote-test helpers, not the goal continuation timer delay.",
];

const FACTUAL_HINTS: [&str; 8] = [
    "The fix is on senpi main, not the extension.",
    "senpi monitors have a verified two-flag desync where registry.paused can remain set.",
    "The regression test does not cover Windows process cleanup.",
    "The outage is unrelated to the database migration.",
    "The patch does not address Windows process cleanup.",
    "The timeout does not pertain to database connections.",
    "The incident report is not about the database migration.",
    "The memory regression test does not cover Windows process cleanup.",
];

const DECISION_LANGUAGE_HINTS: [&str; 13] = [
    "NO STORED MEMORY fits this task.",
    "Nothing clears the bar here.",
    "This memory is not relevant to the task.",
    "No relevant memory is available.",
    "This memory is unrelated to the task.",
    "This memory does not cover the task.",
    "This memory does not address the task.",
    "This memory does not pertain to the task.",
    "This memory is not about the task.",
    "These memories are unrelated to the task.",
    "These memories do not cover the task.",
    "These memories are not about the task.",
    "These memories cover login prompts, not the timer.",
];

const FACTUAL_FRAGMENTS: [&str; 3] = [
    "The unrelated token is a field name in the fixture.",
    "The cover image is on main, not the extension.",
    "The relevant flag is disabled by default.",
];

#[test]
fn given_a_hint_that_breaks_the_shape_budget_when_described_then_the_shape_reason_is_named() {
    assert_eq!(describe_invalid_hint(""), Some(InvalidHintReason::Empty));
    assert_eq!(
        describe_invalid_hint(&"x".repeat(201)),
        Some(InvalidHintReason::TooLong)
    );
    assert_eq!(
        describe_invalid_hint("line one\nline two"),
        Some(InvalidHintReason::Multiline)
    );
    assert_eq!(
        describe_invalid_hint("This memory is not relevant to the task."),
        Some(InvalidHintReason::DecisionCommentary)
    );
}

#[test]
fn given_the_agent_addressing_hints_when_described_then_the_reason_is_addresses_agent() {
    for hint in ADDRESSING_HINTS.iter().chain(KOREAN_ADDRESSING_HINTS.iter()) {
        assert_eq!(
            describe_invalid_hint(hint),
            Some(InvalidHintReason::AddressesAgent),
            "hint should address the agent: {hint}"
        );
    }
}

#[test]
fn given_the_observational_hints_when_described_then_no_reason_is_returned() {
    for hint in OBSERVATIONAL_HINTS
        .iter()
        .chain(KOREAN_OBSERVATIONAL_HINTS.iter())
    {
        assert_eq!(describe_invalid_hint(hint), None, "hint should be clean: {hint}");
    }
}

#[test]
fn given_an_agent_addressing_hint_when_revalidated_then_it_is_dropped_without_reserving_its_path() {
    let corrected = nudge(PATH, OBSERVATIONAL_HINTS[0]);
    for hint in ADDRESSING_HINTS.iter().chain(KOREAN_ADDRESSING_HINTS.iter()) {
        let options = single_option();
        assert!(validate_nudges(&[nudge(PATH, hint)], &options).is_empty());
        assert_eq!(
            validate_nudges(&[nudge(PATH, hint), corrected.clone()], &options),
            vec![corrected.clone()]
        );
    }
}

#[test]
fn given_the_observational_hints_when_revalidated_then_they_survive_unchanged() {
    let options = single_option();
    for hint in OBSERVATIONAL_HINTS
        .iter()
        .chain(KOREAN_OBSERVATIONAL_HINTS.iter())
    {
        assert_eq!(
            validate_nudges(&[nudge(PATH, hint)], &options),
            vec![nudge(PATH, hint)]
        );
    }
}

#[test]
fn given_a_meta_hint_when_revalidated_then_it_is_rejected_without_spending_the_cap() {
    for hint in AUDIT_HINTS {
        let options = single_option();
        assert!(!is_valid_hint(hint));
        assert!(validate_nudges(&[nudge(PATH, hint)], &options).is_empty());
        let corrected = nudge(PATH, FACTUAL_HINTS[0]);
        assert_eq!(
            validate_nudges(&[nudge(PATH, hint), corrected.clone()], &options),
            vec![corrected]
        );
    }
}

#[test]
fn given_decision_language_meta_hints_when_revalidated_then_they_are_rejected() {
    let options = single_option();
    for hint in DECISION_LANGUAGE_HINTS {
        assert!(
            validate_nudges(&[nudge(PATH, hint)], &options).is_empty(),
            "decision-language hint should be rejected: {hint}"
        );
    }
}

#[test]
fn given_factual_fragments_when_revalidated_then_decision_language_fragments_do_not_reject_them() {
    let options = single_option();
    for hint in FACTUAL_FRAGMENTS {
        assert_eq!(
            validate_nudges(&[nudge(PATH, hint)], &options),
            vec![nudge(PATH, hint)],
            "factual hint should survive: {hint}"
        );
    }
}

#[test]
fn given_factual_hints_when_revalidated_then_they_survive_both_layers_unchanged() {
    let options = single_option();
    for hint in FACTUAL_HINTS {
        assert!(is_valid_hint(hint));
        assert_eq!(
            validate_nudges(&[nudge(PATH, hint)], &options),
            vec![nudge(PATH, hint)]
        );
    }
}

#[test]
fn given_an_instruction_shaped_hint_from_an_older_session_when_checked_then_the_replay_predicate_accepts_it()
{
    let stored = "Use the idle wake path.";
    assert!(is_valid_hint(stored));
    assert_eq!(
        describe_invalid_hint(stored),
        Some(InvalidHintReason::AddressesAgent)
    );
}

#[test]
fn given_a_hint_that_breaks_the_shape_budget_when_checked_then_the_replay_predicate_rejects_it() {
    assert!(!is_valid_hint(""));
    assert!(!is_valid_hint(&"x".repeat(201)));
    assert!(!is_valid_hint("line one\nline two"));
    assert!(!is_valid_hint("This memory is not relevant to the task."));
}

#[test]
fn given_pending_nudges_when_written_and_taken_then_they_round_trip_and_the_file_is_removed() {
    let dir = TempDir::new().expect("temp dir");
    let pending = PendingNudges::new(dir.path());
    let nudges = vec![nudge("notes/valid.md", FACTUAL_HINTS[0])];
    pending.write("session-1", &nudges).expect("write");

    assert_eq!(pending.take("session-1"), nudges);
    assert!(fs::read_dir(dir.path()).expect("read dir").next().is_none());
}

#[test]
fn given_a_hand_edited_payload_with_an_invalid_hint_when_taken_then_nothing_returns_and_the_file_is_deleted()
{
    let dir = TempDir::new().expect("temp dir");
    fs::write(
        dir.path().join("session-1.json"),
        "{\n  \"version\": 1,\n  \"sessionId\": \"session-1\",\n  \"writtenAt\": \"2026-10-06T00:00:00.000Z\",\n  \"nudges\": [\n    { \"path\": \"reference/a.md\", \"hint\": \"password=hunter2\" }\n  ]\n}\n",
    )
    .expect("write");

    let taken = PendingNudges::new(dir.path()).take("session-1");
    assert!(taken.is_empty());
    assert!(fs::read_dir(dir.path()).expect("read dir").next().is_none());
}

#[test]
fn given_a_pending_payload_with_a_meta_hint_when_taken_then_the_whole_payload_is_rejected() {
    let dir = TempDir::new().expect("temp dir");
    let pending = PendingNudges::new(dir.path());
    pending
        .write(
            "session-1",
            &[
                nudge("notes/valid.md", FACTUAL_HINTS[0]),
                nudge(PATH, AUDIT_HINTS[0]),
            ],
        )
        .expect("write");

    assert!(pending.take("session-1").is_empty());
    assert!(fs::read_dir(dir.path()).expect("read dir").next().is_none());
}

#[test]
fn given_a_foreign_session_payload_when_taken_then_it_is_left_for_its_real_owner() {
    let dir = TempDir::new().expect("temp dir");
    let pending = PendingNudges::new(dir.path());
    pending
        .write("session-1", &[nudge(PATH, FACTUAL_HINTS[0])])
        .expect("write");

    assert!(pending.take("session-2").is_empty());
    assert_eq!(pending.take("session-1"), vec![nudge(PATH, FACTUAL_HINTS[0])]);
}

#[test]
fn given_an_expired_payload_when_taken_then_it_is_dropped_and_deleted() {
    let dir = TempDir::new().expect("temp dir");
    fs::write(
        dir.path().join("session-1.json"),
        "{\n  \"version\": 1,\n  \"sessionId\": \"session-1\",\n  \"writtenAt\": \"2020-01-01T00:00:00.000Z\",\n  \"nudges\": [\n    { \"path\": \"reference/a.md\", \"hint\": \"The note records the deploy gate.\" }\n  ]\n}\n",
    )
    .expect("write");

    assert!(PendingNudges::new(dir.path()).take("session-1").is_empty());
    assert!(fs::read_dir(dir.path()).expect("read dir").next().is_none());
}

#[test]
fn given_a_foreign_payload_when_deleted_then_it_is_left_alone() {
    let dir = TempDir::new().expect("temp dir");
    let pending = PendingNudges::new(dir.path());
    pending
        .write("session-1", &[nudge(PATH, FACTUAL_HINTS[0])])
        .expect("write");

    pending.delete("session-2");
    assert_eq!(pending.take("session-1"), vec![nudge(PATH, FACTUAL_HINTS[0])]);
}

#[test]
fn given_zero_max_items_when_validating_then_nothing_is_accepted() {
    let options = options(&[PATH], &[], 0);
    assert!(validate_nudges(&[nudge(PATH, FACTUAL_HINTS[0])], &options).is_empty());
}

#[test]
fn given_a_surfaced_path_when_validating_then_it_is_not_repeated() {
    let options = options(&[PATH], &[PATH], 1);
    assert!(validate_nudges(&[nudge(PATH, FACTUAL_HINTS[0])], &options).is_empty());
}

#[test]
fn given_an_unoffered_path_when_validating_then_it_is_rejected() {
    let options = options(&[PATH], &[], 1);
    assert!(validate_nudges(&[nudge("notes/other.md", FACTUAL_HINTS[0])], &options).is_empty());
}

#[test]
fn given_more_candidates_than_the_cap_when_validating_then_order_is_preserved_up_to_the_cap() {
    let options = options(&["a.md", "b.md", "c.md"], &[], 2);
    let accepted = validate_nudges(
        &[
            nudge("a.md", FACTUAL_HINTS[0]),
            nudge("b.md", FACTUAL_HINTS[1]),
            nudge("c.md", FACTUAL_HINTS[2]),
        ],
        &options,
    );
    let paths: BTreeSet<String> = accepted.into_iter().map(|entry| entry.path).collect();
    assert_eq!(paths, BTreeSet::from(["a.md".to_string(), "b.md".to_string()]));
}
