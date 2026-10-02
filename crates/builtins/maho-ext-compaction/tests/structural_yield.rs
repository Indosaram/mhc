use maho_ext_compaction::{r#yield::*, state::*, per_turn_cap::*};
use serde_json::json;
#[test] fn structural_savings_include_previous_summary_and_prefix() {
    let result=compute_structural_yield("prev",&[json!({"role":"user","content":"alpha"})],&[json!({"role":"user","content":"beta"})],"gamma",100.);
    assert_eq!(result.saved_tokens,2.); assert_eq!(result.savings_ratio,0.02); assert!(is_ineffective_compaction(result));
}
#[test] fn exact_floor_and_ratio_are_effective() {assert!(!is_ineffective_compaction(StructuralYield {tokens_before:24000.,saved_tokens:1024.,savings_ratio:0.1}));}
#[test] fn below_floor_is_ineffective() {assert!(is_ineffective_compaction(StructuralYield {tokens_before:2000.,saved_tokens:500.,savings_ratio:0.25}));}
#[test] fn below_ratio_is_ineffective() {assert!(is_ineffective_compaction(StructuralYield {tokens_before:40000.,saved_tokens:2000.,savings_ratio:0.05}));}
#[test] fn zero_negative_and_nan_follow_source_comparisons() {
    for tokens_before in [0.,-1.,f64::NAN] {assert!(is_ineffective_compaction(StructuralYield {tokens_before,saved_tokens:1.,savings_ratio:1.}));}
}
#[test] fn zero_savings_are_ineffective() {for tokens_before in [0.,100.] {assert!(is_ineffective_compaction(StructuralYield {tokens_before,saved_tokens:0.,savings_ratio:0.}));}}
#[test] fn ineffective_attempt_does_not_close_admission_and_resets() {
    let state=increment_ineffective(increment_accepted(increment_accepted(create_initial_state())));
    assert!(!should_reject_by_cap(&state).cancel);
    let state=reset_turn_counter(increment_accepted(state),"turn-1");
    assert_eq!(state.accepted_this_turn,0);assert_eq!(state.ineffective_attempts_this_turn,0);
}
