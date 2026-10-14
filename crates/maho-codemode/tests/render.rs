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

#[test]
fn registered_renderers_draw_the_eval_card_instead_of_raw_arguments() {
    let renderers=std::sync::Arc::new(eval_tool_renderers());
    let args=json!({"language":"js","summary":"collect project facts","code":"const a = 1;\nconsole.log(a)"});
    let context=maho_ext_api::ToolRenderContext {args:args.clone(),tool_call_id:"call".into(),invalidate:std::rc::Rc::new(||{}),last_component:None,state:(),cwd:"/tmp".into(),
        execution_started:false,args_complete:true,is_partial:true,expanded:false,show_images:false,image_protocol:None,is_error:false,has_result:Some(false),spinner_frame:None};
    let mut session=maho_ext_api::ToolRendererSession {renderers,context};
    let lines=session.render_call(&Default::default(),80).expect("eval call renderer");
    assert_eq!(lines,vec!["eval js","collect project facts","const a = 1;","console.log(a)"]);
    assert!(!lines.iter().any(|line|line.contains("\"summary\"")));
}


#[test]
fn live_result_card_repaints_itself_each_second_until_terminal() {
    use std::{cell::Cell,rc::Rc,time::{Duration,Instant}};
    let renderers=std::sync::Arc::new(eval_tool_renderers());
    let args=json!({"language":"js","summary":"work","code":"1+1"});
    let invalidations=Rc::new(Cell::new(0));
    let counter=invalidations.clone();
    let context=maho_ext_api::ToolRenderContext {args,tool_call_id:"live".into(),invalidate:Rc::new(move ||counter.set(counter.get()+1)),last_component:None,state:(),cwd:"/tmp".into(),
        execution_started:true,args_complete:true,is_partial:true,expanded:false,show_images:false,image_protocol:None,is_error:false,has_result:Some(true),spinner_frame:None};
    let mut session=maho_ext_api::ToolRendererSession {renderers,context};
    let mut running=AgentToolResult::text("");
    running.details=json!({"cells":[{"language":"js","code":"1+1","status":"running","startedAt":1000,"output":""}]});
    session.render_result(&running,&Default::default(),80).expect("eval result renderer");
    assert!(maho_tui::ui_intervals::run_due_ui_intervals(Instant::now()+Duration::from_millis(1_001)));
    assert_eq!(invalidations.get(),1);
    let mut done=AgentToolResult::text("2");
    done.details=json!({"cells":[{"language":"js","code":"1+1","status":"complete","startedAt":1000,"durationMs":5,"output":"2"}]});
    session.context.is_partial=false;
    session.render_result(&done,&Default::default(),80).expect("eval result renderer");
    assert!(!maho_tui::ui_intervals::run_due_ui_intervals(Instant::now()+Duration::from_secs(5)));
    assert_eq!(invalidations.get(),1);
}
