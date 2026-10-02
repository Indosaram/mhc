use maho_ext_compaction::extension_wiring::*;
use serde_json::json;
#[test]
fn abort_link_forwards_cancellation_and_drop_removes_subscription() {
    use maho_ai::utils::abort::AbortController;
    let source = AbortController::new();
    let target = AbortController::new();
    let link = link_abort_signal(Some(&source.signal()), &target).unwrap();
    source.abort(None);
    assert!(target.signal().aborted());
    drop(link);
    let source = AbortController::new();
    let target = AbortController::new();
    let link = link_abort_signal(Some(&source.signal()), &target).unwrap();
    drop(link);
    source.abort(None);
    assert!(!target.signal().aborted());
    assert!(link_abort_signal(Some(&source.signal()), &target).is_none());
    assert!(target.signal().aborted());
}
#[test] fn pending_prompt_counts_utf16_and_images() {assert_eq!(estimate_pending_prompt_tokens(Some("😀abc"),2),2402);}
#[test] fn prompt_window_caps_output_reserve_at_half() {assert_eq!(get_prompt_context_window(100000.,Some(80000.)),50000.);assert_eq!(get_prompt_context_window(100000.,Some(10000.)),90000.);for max in [None,Some(0.),Some(f64::NAN)] {assert_eq!(get_prompt_context_window(100000.,max),100000.);}}
#[test] fn additional_usage_updates_percent_and_keeps_unknown_usage() {assert_eq!(with_additional_tokens(&json!({"tokens":50,"contextWindow":100,"percent":50}),10.),json!({"tokens":60.,"contextWindow":100,"percent":60.}));let unknown=json!({"tokens":null,"contextWindow":100});assert_eq!(with_additional_tokens(&unknown,10.),unknown);}
#[test] fn feedback_only_ends_unapplied_nonrejected_results() {assert!(compaction_feedback(true,"stale",false,None).is_none());assert!(compaction_feedback(false,"rejected",false,None).is_none());assert!(compaction_feedback(false,"stale",true,Some("timeout")).expect("feedback").get("errorMessage").is_none());}
#[test] fn checkpoint_window_includes_exact_sixty_seconds() {let entries=[json!({"type":"custom","customType":"compaction.agent-checkpoint","data":{"timestamp":1000}})];assert!(recent_checkpoint(&entries,61000.).is_some());assert!(recent_checkpoint(&entries,61001.).is_none());}
