use maho_ext_compaction::{context_pipeline::*, emergency_prune::EmergencyPruneLatch};
use serde_json::json;

#[test]
fn sdk_lane_stands_down_admission_pruning_and_reminder() {
    let messages = [json!({"role":"user","content":"task","timestamp":0})];
    let mut latch = EmergencyPruneLatch::default();
    let output = build_compaction_context(CompactionContextInput { messages: &messages, context_window: 100, prompt_context_window: 50, usage_tokens: Some(1000.), provider_native_path: false, tool_admission_enabled: true, breaker_fallback: true, lane_owns_compaction: true, emergency_prune_latch: &mut latch, reminder: Some("reminder"), now: 0 }).unwrap();
    let serialized = serde_json::to_value(&output.messages).unwrap();
    assert_eq!(serialized[0]["content"], "task");
    assert!(!latch.engaged);
    assert!(output.emergency_prune_tokens.is_none());
}

#[test]
fn local_lane_injects_reminder_after_budget_projection() {
    let messages = [json!({"role":"user","content":"task","timestamp":0})];
    let mut latch = EmergencyPruneLatch::default();
    let output = build_compaction_context(CompactionContextInput { messages: &messages, context_window: 10000, prompt_context_window: 9000, usage_tokens: Some(1.), provider_native_path: false, tool_admission_enabled: true, breaker_fallback: false, lane_owns_compaction: false, emergency_prune_latch: &mut latch, reminder: Some("reminder"), now: 0 }).unwrap();
    let serialized = serde_json::to_value(&output.messages).unwrap();
    assert_eq!(serialized[0]["content"][0]["text"], "reminder");
    assert_eq!(serialized[0]["content"][1]["text"], "task");
}
