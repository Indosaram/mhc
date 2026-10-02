use maho_ai::types::{Message,ContentBlock,StopReason};
use maho_ext_compaction::{repair_tool_pairs::*,summarization_turn_order::*};
use serde_json::json;
fn assistant(id:&str,stop:&str,incomplete:bool)->Message {
    serde_json::from_value(json!({"role":"assistant","content":[{"type":"toolCall","id":id,"name":"bash","arguments":{},"incomplete":incomplete}],"api":"anthropic-messages","provider":"anthropic","model":"model","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":stop,"timestamp":2})).expect("assistant fixture")
}
fn result(id:&str)->Message {serde_json::from_value(json!({"role":"toolResult","toolCallId":id,"toolName":"bash","content":[{"type":"text","text":"actual"}],"isError":false,"timestamp":3})).expect("tool result fixture")}
fn user()->Message {serde_json::from_value(json!({"role":"user","content":"continue","timestamp":1})).expect("user fixture")}
#[test] fn valid_pairs_are_unchanged() {let input=[user(),assistant("call","toolUse",false),result("call")];assert_eq!(repair_orphaned_tool_results(&input,9),input);}
#[test] fn orphan_result_gets_placeholder() {let output=repair_orphaned_tool_results(&[result("orphan")],9);let Message::ToolResult(result)=&output[0] else {panic!("result")};assert_eq!(result.content,vec![ContentBlock::text(TOOL_RESULT_PLACEHOLDER)]);}
#[test] fn dangling_call_gets_synthetic_result() {let output=repair_orphaned_tool_results(&[assistant("call","toolUse",false)],9);let Message::ToolResult(result)=&output[1] else {panic!("result")};assert_eq!(result.tool_call_id,"call");assert_eq!(result.timestamp,3);assert!(!result.is_error);}
#[test] fn incomplete_call_gets_error_result() {let output=repair_orphaned_tool_results(&[assistant("call","toolUse",true)],9);let Message::ToolResult(result)=&output[1] else {panic!("result")};assert!(result.is_error);}
#[test] fn errored_assistant_does_not_get_synthetic_result() {assert_eq!(repair_orphaned_tool_results(&[assistant("call","error",false)],9).len(),1);}
#[test] fn aborted_assistant_does_not_get_synthetic_result() {assert_eq!(repair_orphaned_tool_results(&[assistant("call","aborted",false)],9).len(),1);}
#[test] fn duplicate_dangling_ids_get_one_result() {assert_eq!(repair_orphaned_tool_results(&[assistant("call","toolUse",false),assistant("call","toolUse",false)],9).len(),3);}
#[test] fn zero_timestamp_uses_supplied_clock() {let mut input=assistant("call","toolUse",false);if let Message::Assistant(assistant)=&mut input {assistant.timestamp=0;}let output=repair_orphaned_tool_results(&[input],99);let Message::ToolResult(result)=&output[1] else {panic!("result")};assert_eq!(result.timestamp,99);}
#[test] fn adjacent_assistants_merge_content() {let input=[user(),assistant("a","toolUse",false),assistant("b","toolUse",false),user()];let output=normalize_summarization_turn_order(&input);assert_eq!(output.len(),3);let Message::Assistant(assistant)=&output[1] else {panic!("assistant")};assert_eq!(assistant.content.len(),2);}
#[test] fn prefix_before_first_user_is_dropped() {let output=normalize_summarization_turn_order(&[assistant("a","toolUse",false),user(),assistant("b","toolUse",false)]);assert_eq!(output[0].role(),"user");}
#[test] fn only_trailing_user_preserves_history() {let input=[assistant("a","toolUse",false),user()];assert_eq!(normalize_summarization_turn_order(&input),input);}
#[test] fn no_user_preserves_history() {let input=[result("orphan")];assert_eq!(normalize_summarization_turn_order(&input),input);}
#[test] fn failed_assistants_are_not_merged() {let input=[user(),assistant("a","error",false),assistant("b","toolUse",false)];let output=normalize_summarization_turn_order(&input);assert_eq!(output.len(),3);let Message::Assistant(assistant)=&output[1] else {panic!("assistant")};assert_eq!(assistant.stop_reason,StopReason::Error);}
#[test] fn pair_repair_is_idempotent() {let once=repair_orphaned_tool_results(&[assistant("call","toolUse",true)],99);assert_eq!(repair_orphaned_tool_results(&once,100),once);}
#[test] fn incomplete_call_with_real_result_is_preserved() {let input=[assistant("call","toolUse",true),result("call")];assert_eq!(repair_orphaned_tool_results(&input,99),input);}
#[test] fn custom_incomplete_error_is_preserved() {
    let mut input=assistant("call","toolUse",true);if let Message::Assistant(message)=&mut input && let ContentBlock::ToolCall(call)=&mut message.content[0] {call.error_message=Some("custom reason".into());}
    let output=repair_orphaned_tool_results(&[input],99);let Message::ToolResult(result)=&output[1] else {panic!("result")};assert!(result.is_error);let ContentBlock::Text(text)=&result.content[0] else {panic!("text")};assert!(text.text.starts_with("custom reason."));
}
#[tokio::test] async fn faux_provider_accepts_repaired_messages() {
    use maho_ai::providers::faux::{register_faux_provider,RegisterFauxProviderOptions,faux_assistant_message,FauxAssistantMessageOptions};
    let registration=register_faux_provider(RegisterFauxProviderOptions {scheduler_hook:Some(std::sync::Arc::new(||Box::pin(async {}))),..Default::default()});
    registration.set_responses(vec![faux_assistant_message("acknowledged",FauxAssistantMessageOptions::default()).into()]);
    let messages=repair_orphaned_tool_results(&[user(),assistant("call","toolUse",false),result("orphan")],99);
    let context=maho_ai::types::Context {system_prompt:Some("test-system".into()),messages:messages.clone(),tools:None};
    let response=maho_ai::compat::complete(&registration.get_model(None).expect("faux model"),&context,None).await.expect("faux response");
    assert_eq!(response.stop_reason,StopReason::Stop);assert!(response.error_message.is_none());
    let calls=registration.get_call_log();assert_eq!(calls.len(),1);assert_eq!(calls[0].context.messages,messages);
}
