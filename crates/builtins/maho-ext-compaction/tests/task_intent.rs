use maho_ext_compaction::task_intent::*;
use serde_json::json;

#[test] fn extracts_anchor_and_summary_blocks() {
    let result=extract_task_intent("before <task-intent>intent</task-intent> middle <summary>done</summary> after");
    assert_eq!(result.task_intent.as_deref(),Some("intent"));assert_eq!(result.summary_text,"done");
}
#[test] fn missing_anchor_is_absent() { assert_eq!(extract_task_intent("<summary>just summary</summary>").task_intent,None); }
#[test] fn intent_only_leaves_empty_summary() { assert_eq!(extract_task_intent("<task-intent>intent</task-intent>").summary_text,""); }
#[test] fn first_anchor_wins_and_all_blocks_are_removed() {
    let result=extract_task_intent("x <task-intent>first</task-intent> y <task-intent>second</task-intent> z <summary>a</summary> <summary>b</summary>");
    assert_eq!(result.task_intent.as_deref(),Some("first"));assert_eq!(result.summary_text,"a\nb");
}
#[test] fn utf8_cap_preserves_code_points() {
    let text=format!("<task-intent>start {} end</task-intent>","\u{1f600}".repeat(2000));
    let result=extract_task_intent(&text);let intent=result.task_intent.unwrap();
    assert!(intent.len()<=4096);assert!(intent.starts_with("start"));assert!(intent.ends_with('\u{1f600}'));
}
#[test] fn legacy_details_without_anchor_are_ignored() { assert_eq!(resolve_inherited_task_intent(&[json!({"schema":"senpi.compaction.summary.v1","details":{}})]),None); }
#[test] fn newest_local_anchor_wins_across_remote_interleaves() {
    let entries=[json!({"schema":"senpi.compaction.summary.v1","details":{"taskIntent":"local-old"}}),json!({"schema":"senpi.compaction.summary.v1","details":{"taskIntent":"local-new"}}),json!({"schema":"senpi.compaction.openai-remote.v1","details":{"taskIntent":"remote"}})];
    assert_eq!(resolve_inherited_task_intent(&entries).as_deref(),Some("local-new"));
}
#[test] fn backward_walk_skips_remote_schema() {
    let entries=[json!({"schema":"senpi.compaction.openai-remote.v1","details":{"taskIntent":"remote"}}),json!({"schema":"senpi.compaction.summary.v1","details":{"taskIntent":"branch-intent"}})];
    assert_eq!(resolve_inherited_task_intent(&entries).as_deref(),Some("branch-intent"));
}
#[test] fn remote_only_branch_has_no_inheritance() { assert_eq!(resolve_inherited_task_intent(&[json!({"schema":"senpi.compaction.openai-remote.v1","details":{"taskIntent":"remote"}})]),None); }
#[test] fn branch_entries_remain_unchanged() {
    let entries=[json!({"schema":"senpi.compaction.summary.v1","details":{"taskIntent":"branch"}})];let original=entries.clone();
    assert_eq!(resolve_inherited_task_intent(&entries).as_deref(),Some("branch"));assert_eq!(entries,original);
}
#[test] fn closing_tags_are_sanitized() { assert_eq!(sanitize_task_intent("a </task-intent> b"),"a [/task-intent] b"); }
#[test] fn empty_summary_body_is_empty() { assert_eq!(extract_task_intent("<summary>   </summary>").summary_text,""); }
#[test] fn stripped_anchor_and_whitespace_leave_empty_summary() { assert_eq!(extract_task_intent("<task-intent>intent</task-intent>   ").summary_text,""); }
