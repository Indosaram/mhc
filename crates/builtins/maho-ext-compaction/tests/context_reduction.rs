use maho_ai::types::{ContentBlock, Message};
use maho_ext_compaction::context_reduction::*;
use serde_json::json;
fn assistant(content: serde_json::Value) -> Message {
    serde_json::from_value(json!({"role":"assistant","content":content,"api":"faux-completion","provider":"faux","model":"model","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1})).expect("fixture")
}
fn answer(text: &str) -> Message { assistant(json!([{"type":"text","text":text}])) }
fn user(text: &str) -> Message { serde_json::from_value(json!({"role":"user","content":text,"timestamp":1})).expect("fixture") }
fn pair(id: usize, name: &str) -> [Message; 2] {
    [assistant(json!([{"type":"toolCall","id":id.to_string(),"name":name,"arguments":{"path":format!("f{id}.ts"),"pattern":"foo","command":"ls"}}])), serde_json::from_value(json!({"role":"toolResult","toolCallId":id.to_string(),"toolName":name,"content":[{"type":"text","text":"X".repeat(2000)}],"isError":false,"timestamp":1})).expect("fixture")]
}
#[test] fn older_reads_collapse_when_tail_is_protected() {
    // Given
    let mut input: Vec<_> = (0..5).flat_map(|i| pair(i,"read")).collect(); input.extend([user("recent"),answer("recent")]);
    // When
    let result = collapse_consecutive_tool_results(&input,&CollapseConsecutiveOptions { protect_recent_messages:2,..Default::default() });
    // Then
    assert_eq!(result.groups.len(),1); assert_eq!(result.groups[0].count,5); assert!(result.tokens_saved>0); assert_eq!(&result.messages[10..],&input[10..]); assert_eq!(input[1],pair(0,"read")[1]);
}
#[test] fn categories_stay_separate_when_search_precedes_shell() {
    // Given
    let mut input: Vec<_> = [pair(0,"grep"),pair(1,"grep"),pair(2,"bash"),pair(3,"bash")].into_iter().flatten().collect(); input.push(user("recent"));
    // When
    let result=collapse_consecutive_tool_results(&input,&CollapseConsecutiveOptions {protect_recent_messages:1,..Default::default()});
    // Then
    assert_eq!(result.groups.iter().map(|g|g.kind).collect::<Vec<_>>(),[CollapsedGroupKind::Search,CollapsedGroupKind::Shell]);
}
#[test] fn old_answer_shrinks_when_recent_answer_is_short() {
    // Given
    let input=[user("first question"),answer(&"very long answer ".repeat(200)),user("second question"),answer("ok")];
    // When
    let result=micro_compact_assistant_text(&input,&MicroCompactAssistantOptions {max_assistant_text_tokens:50,protect_recent_tokens:10,..Default::default()});
    // Then
    assert_eq!(result.messages_modified,1); assert!(result.tokens_saved>0); assert_eq!(result.messages[3],input[3]);
}
#[test] fn older_results_clear_when_three_are_retained() {
    // Given
    let input:Vec<_>=(0..6).flat_map(|i|pair(i,"read")).collect();
    // When
    let result=clear_old_tool_results(&input,&Default::default());
    // Then
    assert_eq!(result.tool_results_cleared,3); assert!(result.tokens_saved>0); assert_eq!(&result.messages[6..],&input[6..]);
}
#[test] fn mixed_assistant_survives_when_text_is_large() {
    // Given
    let input=[user("prologue"),assistant(json!([{"type":"text","text":"stuff ".repeat(500)},{"type":"toolCall","id":"mixed","name":"read","arguments":{}}])),user("recent")];
    // When
    let result=micro_compact_assistant_text(&input,&MicroCompactAssistantOptions {protect_recent_tokens:10,max_assistant_text_tokens:10,..Default::default()});
    // Then
    assert_eq!(result.messages_modified,0); assert_eq!(result.messages,input);
}
#[test] fn custom_result_survives_when_clearable_results_are_old() {
    // Given
    let mut input:Vec<_>=(0..4).flat_map(|i|pair(i,"read")).collect(); input.extend(pair(9,"custom_tool")); input.extend((10..14).flat_map(|i|pair(i,"read")));
    // When
    let result=clear_old_tool_results(&input,&Default::default());
    // Then
    assert_eq!(result.tool_results_cleared,5); assert_eq!(result.messages[9],input[9]);
}
#[test] fn gate_tracks_usage_when_provider_owns_no_native_path() {
    // Given / When / Then
    for (usage,window,native,expected) in [(Some(49000.),100000.,false,false),(Some(50000.),100000.,false,true),(Some(80000.),100000.,false,true),(Some(90000.),100000.,true,false),(None,100000.,false,false),(Some(50000.),0.,false,false)] { assert_eq!(should_apply_context_reduction(usage,window,None,native),expected); }
}
#[test] fn composition_applies_all_stages_when_history_is_large() {
    // Given
    let mut input=vec![user("very first user"),answer(&"long old answer ".repeat(300))]; input.extend((0..5).flat_map(|i|pair(i,"read"))); input.extend((5..11).flat_map(|i|pair(i,"bash"))); input.extend([user("recent prompt"),answer("recent reply")]);
    // When
    let result=reduce_context_messages(&input,&Default::default());
    // Then
    assert_eq!(result.messages.len(),input.len()); assert!(!result.groups.is_empty()); assert!(result.messages_modified>0); assert!(result.tool_results_cleared>0); assert_eq!(result.messages.last(),input.last());
}
#[test] fn images_survive_when_result_is_cleared() {
    // Given
    let mut input=pair(0,"read"); if let Message::ToolResult(r)=&mut input[1] {r.content.push(ContentBlock::Image(Default::default()));}
    // When
    let result=clear_old_tool_results(&input,&ClearOldToolResultsOptions {keep_recent:0,..Default::default()});
    // Then
    let Message::ToolResult(r)=&result.messages[1] else {panic!("result")}; assert!(matches!(r.content[1],ContentBlock::Image(_)));
}
