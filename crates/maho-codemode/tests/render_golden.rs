use maho_codemode::tool::{eval_request::parse_eval_request, render::{EvalRenderContext,render_eval_call,render_eval_result}};
use maho_ext_api::AgentToolResult;
use serde_json::Value;

#[test]
fn pinned_upstream_call_and_result_rendering() {
    let cases:Vec<Value>=serde_json::from_str(include_str!("golden/codemode-render.json")).unwrap();
    for case in cases {
        let args=parse_eval_request(&case["args"]).unwrap();
        let context=EvalRenderContext {now:case["context"]["now"].as_f64().unwrap(),..Default::default()};
        let width=case["width"].as_u64().unwrap() as usize;
        let actual=if case["kind"]=="call" {render_eval_call(&args,&context,width)} else {
            let result:AgentToolResult=serde_json::from_value(case["result"].clone()).unwrap();
            render_eval_result(&result,&args,&context,width)
        };
        let expected:Vec<String>=serde_json::from_value(case["lines"].clone()).unwrap();
        assert_eq!(actual,expected,"{} at width {width}",case["kind"]);
    }
}
