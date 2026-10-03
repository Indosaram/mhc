use maho_codemode::tool::{render::*,eval_request::parse_eval_request};
use maho_ext_api::AgentToolResult;
use serde_json::json;

#[test]
fn collapsed_call_keeps_last_four_code_lines() {
    let args=parse_eval_request(&json!({"language":"js","summary":" six  line preview ","code":"a\nb\nc\nd\ne\nf"})).unwrap();
    assert_eq!(render_eval_call(&args,&Default::default(),80),vec!["eval js","six line preview","2 earlier code lines","c","d","e","f"]);
    assert!(render_eval_call(&args,&EvalRenderContext {has_result:true,..Default::default()},80).is_empty());
}

#[test]
fn result_metadata_and_output_preview_follow_source() {
    let mut result=AgentToolResult::text((1..=10).map(|i|format!("line-{i}")).collect::<Vec<_>>().join("\n"));
    result.details=json!({"language":"js","durationMs":0,"phase":"summarizing","toolCallCount":2,"wallDurationMs":1000});
    let lines=render_eval_result(&result,&maho_codemode::tool::types::EvalToolRequest::List,&Default::default(),80);
    assert_eq!(lines[0],"eval js done");
    assert_eq!(lines[1],"phase summarizing | took 1s | 2 calls · 2.00 calls/s");
    assert_eq!(lines[3],"2 earlier output lines");
    assert_eq!(lines.last().unwrap(),"line-10");
}

#[test]
fn live_cell_clock_and_reset_badges_are_rendered() {
    let args=parse_eval_request(&json!({"language":"js","summary":"work","code":"1+1","reset":true,"timeout":5})).unwrap();
    let mut result=AgentToolResult::text("");
    result.details=json!({"cells":[{"language":"js","code":"1+1","status":"running","startedAt":1000,"output":"2"}]});
    let lines=render_eval_result(&result,&args,&EvalRenderContext {now:4000.0,..Default::default()},80);
    assert_eq!(lines[0],"╭─ eval js running ⠋ · 3s · reset · timeout 5s");
    assert!(lines.contains(&"│ 2".into()));
    assert_eq!(lines.last().unwrap(),"╰─");
}

#[test]
fn collapsed_calls_bound_error_and_show_exact_omissions() {
    let mut result=AgentToolResult::text("");
    result.details=json!({"language":"js","toolCalls":(0..7).map(|i|json!({"name":format!("call-{i}"),"ok":false,"error":"x".repeat(600)})).collect::<Vec<_>>()});
    let lines=render_eval_result(&result,&maho_codemode::tool::types::EvalToolRequest::List,&Default::default(),80);
    assert!(lines.contains(&"2 earlier tool calls".into()));
    assert_eq!(lines.iter().filter(|line|line.as_str()=="[tool error omitted]").count(),5);
    assert!(!lines.iter().any(|line|line.contains("tool.call-0")));
}
