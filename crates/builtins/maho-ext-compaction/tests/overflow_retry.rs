use maho_ext_compaction::overflow_retry::*;
use serde_json::json;
#[test] fn retry_attempts_and_elapsed_budget_are_bounded() {assert!(allow_overflow_retry(2,239999.0));assert!(!allow_overflow_retry(3,0.0));assert!(!allow_overflow_retry(0,240000.0));}
#[test] fn history_budget_leaves_prompt_room_and_floor() {assert_eq!(summarization_history_budget(8000,1000),3800);assert_eq!(summarization_history_budget(100,1000),256);}
#[test] fn old_tool_pairs_are_removed_together() {
    let messages=[json!({"role":"assistant","content":[{"type":"toolCall","id":"old","name":"bash","arguments":{}}]}),json!({"role":"toolResult","toolCallId":"old","content":[{"type":"text","text":"x".repeat(4000)}]}),json!({"role":"user","content":"latest"})];
    let pruned=prune_old_messages_to_budget(&messages,10);assert_eq!(pruned,vec![messages[2].clone()]);
}
#[test] fn last_user_and_current_turn_are_preserved() {let messages=[json!({"role":"user","content":"x".repeat(4000)}),json!({"role":"assistant","content":[{"type":"text","text":"y".repeat(4000)}]})];assert_eq!(prune_old_messages_to_budget(&messages,1),messages);}
#[test] fn oversized_input_is_presized() {let messages:Vec<_>=(0..40).map(|index|json!({"role":"assistant","content":[{"type":"text","text":format!("history-{index} {}","x".repeat(1200))}]})).collect();let bounded=bound_summarization_input(&messages,8000,100);assert!(estimate_total_tokens(&bounded)<=4700);assert!(bounded.len()<messages.len());}
#[test] fn retry_shrinks_geometrically() {let messages:Vec<_>=(0..40).map(|_|json!({"role":"assistant","content":[{"type":"text","text":"x".repeat(1200)}]})).collect();let bounded=bound_summarization_input(&messages,8000,100);let shrunk=shrink_summarization_input_for_overflow_retry(&bounded,8000,100).expect("shrink");assert!(shrunk.len()<=bounded.len().div_ceil(2));}
#[test] fn unprunable_current_turn_still_changes_retry() {let messages=[json!({"role":"user","content":"latest"}),json!({"role":"assistant","content":[{"type":"text","text":"answer"}]})];assert_eq!(shrink_summarization_input_for_overflow_retry(&messages,8000,0),Some(vec![messages[1].clone()]));}
#[test] fn one_message_cannot_shrink() {assert!(shrink_summarization_input_for_overflow_retry(&[json!({"role":"user","content":"only"})],8000,0).is_none());}
#[test] fn cjk_density_is_charged_in_wire_fields() {let ascii=json!({"role":"user","content":"x".repeat(100)});let cjk=json!({"role":"user","content":"\u{ac00}".repeat(100)});assert_eq!(estimate_total_tokens(&[ascii]),25);assert_eq!(estimate_total_tokens(&[cjk]),75);}
