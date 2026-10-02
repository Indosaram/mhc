use maho_ext_compaction::{emergency_prune::*,overflow_retry::estimate_total_tokens};
use serde_json::{Value,json};
fn history()->Vec<Value> {vec![json!({"role":"user","content":"initial request","timestamp":1}),json!({"role":"assistant","content":[{"type":"toolCall","id":"call","name":"bash","arguments":{"command":"echo call"}}]}),json!({"role":"toolResult","toolCallId":"call","toolName":"bash","content":[{"type":"text","text":format!("{}MID-SENTINEL-7f3a1c{}","A".repeat(9000),"B".repeat(9000))}],"isError":false,"timestamp":3}),json!({"role":"user","content":"latest request","timestamp":9000})]}
#[test] fn hysteresis_keeps_shape_stable_across_engage_boundary() {
    let input=history();let total=estimate_total_tokens(&input) as f64;let mut latch=EmergencyPruneLatch::default();let mut shapes=Vec::new();
    for ratio in [0.96,0.9,0.96,0.9,0.88] {shapes.push(hard_limit_emergency_prune(&input,(total/ratio).floor() as u64,Some(&mut latch)).messages);}
    assert!(shapes.windows(2).all(|pair|pair[0]==pair[1]));assert!(latch.engaged);
}
#[test] fn latch_releases_when_context_shrinks() {let mut latch=EmergencyPruneLatch::default();hard_limit_emergency_prune(&history(),5000,Some(&mut latch));assert!(latch.engaged);let small=[json!({"role":"user","content":"small","timestamp":1})];let output=hard_limit_emergency_prune(&small,5000,Some(&mut latch));assert!(!latch.engaged);assert_eq!(output.messages,small);assert!(!output.needs_aggressive_compaction);}
#[test] fn latch_stays_engaged_while_history_is_large() {let input=history();let mut latch=EmergencyPruneLatch::default();hard_limit_emergency_prune(&input,5000,Some(&mut latch));let output=hard_limit_emergency_prune(&input,5000,Some(&mut latch));assert!(latch.engaged);assert_ne!(output.messages,input);}
#[test] fn under_threshold_preserves_oversized_result() {let input=history();let output=hard_limit_emergency_prune(&input,128000,None);assert_eq!(output.messages,input);assert!(!output.needs_aggressive_compaction);}
#[test] fn over_threshold_truncates_and_requests_aggressive_compaction() {let input=vec![json!({"role":"user","content":"latest","timestamp":1}),history()[2].clone()];let output=hard_limit_emergency_prune(&input,2,None);assert!(output.needs_aggressive_compaction);assert!(output.messages[1]["content"][0]["text"].as_str().expect("text").contains("<truncated:"));}

#[test]
fn representative_overflow_drops_atomic_pairs_before_old_prose() {
    let mut input = history();
    input.insert(3, json!({"role":"assistant","content":[{"type":"text","text":"old explanation"}]}));
    input.insert(4, json!({"role":"user","content":"older follow-up","timestamp":4}));
    let expected = vec![input[3].clone(), input[4].clone(), input[5].clone()];
    let window = (estimate_total_tokens(&expected) as f64 / 0.95).ceil() as u64;
    let result = hard_limit_emergency_prune(&input, window, None);
    assert!(result.needs_aggressive_compaction);
    assert_eq!(result.messages, expected);
}

#[test]
fn many_old_messages_trim_to_the_real_estimator_budget() {
    let input: Vec<_> = (0..300).map(|index| json!({"role":"user","content":format!("old-{index:03}"),"timestamp":index})).collect();
    let expected = &input[270..];
    let window = (estimate_total_tokens(expected) as f64 / 0.95).ceil() as u64;
    let result = hard_limit_emergency_prune(&input, window, None);
    assert!(result.needs_aggressive_compaction);
    assert_eq!(result.messages, expected);
}
