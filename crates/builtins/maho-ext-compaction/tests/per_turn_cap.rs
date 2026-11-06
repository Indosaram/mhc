use maho_ext_api::{CompactionReason, SessionEntry};
use maho_ext_compaction::{
    circuit_breaker::record_failure,
    per_turn_cap::*,
    state::*,
    r#yield::{StructuralYield, is_ineffective_compaction, recorded_yield},
};
use serde_json::json;

fn accepted(state: CompactionExtensionState, saved_tokens: f64) -> CompactionExtensionState {
    record_accepted_compaction(
        state,
        Some(StructuralYield {
            saved_tokens,
            savings_ratio: saved_tokens / 100_000.0,
            tokens_before: 100_000.0,
        }),
    )
}

fn entry(details: serde_json::Value) -> SessionEntry {
    SessionEntry {
        id: "entry".into(),
        parent_id: None,
        timestamp: "1970-01-01T00:00:00.000Z".into(),
        kind: "compaction".into(),
        data: json!({"tokensBefore": 40_000, "summary": "digest", "details": details}),
    }
}

#[test]
fn ineffective_attempts_keep_admission_open_below_the_limit() {
    let mut state = create_initial_state();
    for _ in 0..INEFFECTIVE_ADMISSION_LIMIT - 1 {
        state = increment_ineffective(state);
    }
    assert_eq!(
        state.ineffective_attempts_this_turn,
        INEFFECTIVE_ADMISSION_LIMIT - 1
    );
    assert!(!should_reject_by_cap(&state).cancel);
}

#[test]
fn reaching_the_limit_refuses_further_automatic_compaction() {
    let mut state = create_initial_state();
    for _ in 0..INEFFECTIVE_ADMISSION_LIMIT {
        state = increment_ineffective(state);
    }
    assert!(should_reject_by_cap(&state).cancel);
}

#[test]
fn accepted_compactions_never_consume_admission() {
    let mut state = create_initial_state();
    for _ in 0..100 {
        state = accepted(state, 50_000.0);
    }
    assert_eq!(state.accepted_absolute, 100);
    assert_eq!(state.ineffective_attempts_this_turn, 0);
    assert!(!should_reject_by_cap(&state).cancel);
}

#[test]
fn accepted_compactions_that_save_nothing_count_as_ineffective() {
    let mut state = create_initial_state();
    for _ in 0..INEFFECTIVE_ADMISSION_LIMIT {
        state = accepted(state, 0.0);
    }
    assert_eq!(
        state.ineffective_attempts_this_turn,
        INEFFECTIVE_ADMISSION_LIMIT
    );
    assert!(should_reject_by_cap(&state).cancel);
    assert_eq!(state.last_yield.map(|last| last.saved_tokens), Some(0.0));
}

#[test]
fn accepted_compactions_clear_the_breaker() {
    let mut state = create_initial_state();
    for _ in 0..3 {
        state = record_failure(state, 0.0, Some(CompactionReason::Extension), None);
    }
    assert_eq!(state.tripped_at, Some(0.0));
    let state = accepted(state, 50_000.0);
    assert_eq!(state.consecutive_failures, 0);
    assert_eq!(state.tripped_at, None);
}

#[test]
fn a_turn_end_keeps_the_ineffective_count() {
    let mut state = create_initial_state();
    for _ in 0..INEFFECTIVE_ADMISSION_LIMIT {
        state = increment_ineffective(state);
    }
    let ended = end_turn(state);
    assert_eq!(ended.accepted_this_turn, 0);
    assert_eq!(ended.ineffective_attempts_this_turn, INEFFECTIVE_ADMISSION_LIMIT);
    assert!(should_reject_by_cap(&ended).cancel);
}

#[test]
fn an_effective_compaction_refills_the_ineffective_budget() {
    let mut state = create_initial_state();
    for _ in 0..INEFFECTIVE_ADMISSION_LIMIT {
        state = accepted(state, 0.0);
    }
    assert!(should_reject_by_cap(&state).cancel);
    let state = accepted(state, 50_000.0);
    assert_eq!(state.ineffective_attempts_this_turn, 0);
    assert!(!should_reject_by_cap(&state).cancel);
}

#[test]
fn recorded_yield_reads_the_entry_details() {
    let measured = recorded_yield(&entry(json!({"structuralYield": {"savedTokens": 0, "savingsRatio": 0.0}})))
        .expect("recorded yield");
    assert_eq!(measured.saved_tokens, 0.0);
    assert_eq!(measured.tokens_before, 40_000.0);
    assert!(is_ineffective_compaction(measured));
}

#[test]
fn an_entry_without_a_structural_yield_measures_nothing() {
    assert!(recorded_yield(&entry(json!({}))).is_none());
}
