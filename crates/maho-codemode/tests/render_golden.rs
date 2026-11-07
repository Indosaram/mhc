use maho_codemode::tool::{
    eval_request::parse_eval_request,
    render::{EvalRenderContext, render_eval_call, render_eval_result},
};
use maho_ext_api::AgentToolResult;
use serde_json::Value;

#[test]
fn pinned_upstream_call_and_result_rendering() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("golden/codemode-render.json")).unwrap();
    for case in cases {
        // A summary-less call does not parse; the result card then renders with List args, like EvalCard.
        let parsed = parse_eval_request(&case["args"]);
        if case["kind"] == "call" && parsed.is_err() {
            continue;
        }
        let args = parsed.unwrap_or(maho_codemode::tool::types::EvalToolRequest::List);
        let context = EvalRenderContext {
            now: case["context"]["now"].as_f64().unwrap(),
            expanded: case["context"]["expanded"] == true,
            ..Default::default()
        };
        let width = case["width"].as_u64().unwrap() as usize;
        let actual = if case["kind"] == "call" {
            render_eval_call(&args, &context, width)
        } else {
            let result: AgentToolResult = serde_json::from_value(case["result"].clone()).unwrap();
            render_eval_result(&result, &args, &context, width)
        };
        let expected: Vec<String> = serde_json::from_value(case["lines"].clone()).unwrap();
        assert_eq!(
            actual, expected,
            "{} {} at width {width} expanded {}",
            case["name"], case["kind"], context.expanded
        );
    }
}
