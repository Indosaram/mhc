use maho_ai::types::Message;
use maho_ext_compaction::fallback_failed_turn_normalization::mark_failed_turn_fragments;
use serde_json::json;
fn assistant(stop:&str)->Message {serde_json::from_value(json!({"role":"assistant","content":[{"type":"toolCall","id":"call","name":"read","arguments":{}}],"api":"faux-completion","provider":"faux","model":"m","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":stop,"timestamp":1})).expect("fixture")}
fn result()->Message {serde_json::from_value(json!({"role":"toolResult","toolCallId":"call","toolName":"read","content":[{"type":"text","text":"body"}],"isError":false,"timestamp":2})).expect("fixture")}
#[test] fn failure_and_orphan_result_are_marked() {let messages=[assistant("error"),result()];assert_eq!(mark_failed_turn_fragments(&messages),[true,true]);assert_eq!(messages[0],assistant("error"));}
#[test] fn redeclared_call_keeps_the_result() {assert_eq!(mark_failed_turn_fragments(&[assistant("error"),assistant("stop"),result()]),[true,false,false]);}
#[test] fn aborted_fragments_are_marked() {assert_eq!(mark_failed_turn_fragments(&[assistant("aborted"),result()]),[true,true]);}
#[test] fn successful_duplicate_results_are_both_kept() {assert_eq!(mark_failed_turn_fragments(&[assistant("toolUse"),result(),result()]),[false,false,false]);}
