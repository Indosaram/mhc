use maho_ext_btw::side_query::build_side_query_context;
use maho_ai::types::{Model,Message};
use serde_json::json;
fn model(window:u64)->Model{serde_json::from_value(json!({"id":"fixture","name":"fixture","api":"faux","provider":"faux","baseUrl":"http://unused.invalid","reasoning":false,"input":["text"],"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},"contextWindow":window,"maxTokens":0})).expect("model")}
fn user(text:&str)->Message{serde_json::from_value(json!({"role":"user","content":text,"timestamp":0})).expect("user")}
#[test]
fn unknown_window_preserves_history_and_adds_question_without_tools(){
    let history=vec![user("old")];
    let context=build_side_query_context("system",history.clone(),"question",&model(0)).expect("context");
    assert_eq!(context.messages[0],history[0]);
    assert_eq!(context.messages.len(),2);
    assert_eq!(serde_json::to_value(&context.messages[1]).expect("question")["content"],"question");
    assert!(context.tools.expect("tools").is_empty());
}
#[test]
fn oversized_question_is_rejected_before_history_pruning(){
    let result=build_side_query_context("system",vec![user("old")],&"q".repeat(10_000),&model(200));
    assert!(result.is_err());
}
#[test]
fn pruning_keeps_final_question_and_does_not_mutate_supplied_history(){
    let history=(0..30).map(|_|user(&"h".repeat(400))).collect::<Vec<_>>();
    let saved=history.clone();
    let context=build_side_query_context("system",history.clone(),"question",&model(500)).expect("bounded context");
    assert_eq!(history,saved);
    assert!(context.messages.len()<history.len()+1);
    assert_eq!(serde_json::to_value(context.messages.last().expect("question")).expect("value")["content"],"question");
}
