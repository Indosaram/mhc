use maho_codemode::tool::call_capture::*;
use serde_json::{Value, json};

#[test] fn surrogate_pairs_remain_intact() { assert_eq!(cap_code_points("😀x", 1), "😀…"); }
#[test] fn long_string_retained_but_capped() { assert_eq!(bound_tool_call_args(&json!({"blob":"x".repeat(10_000)})), BoundedToolCallArgs { args: Some(json!({"blob":format!("{}…", "x".repeat(512))})), truncated: true }); }
#[test] fn serialized_budget_omits_args() { let values = (0..20).map(|index| (format!("key-{index}"), json!("x".repeat(400)))).collect::<serde_json::Map<_,_>>(); assert_eq!(bound_tool_call_args(&Value::Object(values)), BoundedToolCallArgs { args: None, truncated: true }); }
#[test] fn depth_seven_subtree_replaced() { assert_eq!(bound_tool_call_args(&json!({"one":{"two":{"three":{"four":{"five":{"six":{"seven":"too deep"}}}}}}})), BoundedToolCallArgs { args: Some(json!({"one":{"two":{"three":{"four":{"five":{"six":"…"}}}}}})), truncated: true }); }
#[test] fn array_first_thirty_two_retained() { assert_eq!(bound_tool_call_args(&json!((0..40).collect::<Vec<_>>())), BoundedToolCallArgs { args: Some(json!((0..32).collect::<Vec<_>>())), truncated: true }); }
#[test] fn clean_args_unchanged() { let args = json!({"path":"/tmp/x.txt", "offset":1,"nested":[true,null]}); assert_eq!(bound_tool_call_args(&args), BoundedToolCallArgs { args: Some(args), truncated: false }); }
#[test] fn metric_name_cap() { assert_eq!(create_tool_call_metric(&"x".repeat(200), 10.0).name, format!("{}…", "x".repeat(128))); }
#[test] fn settlement_duration_clamped() { let mut metric = create_tool_call_metric("read", 10.0); settle_tool_call_metric(&mut metric, false, 5.0); assert_eq!(metric.ok, Some(false)); assert_eq!(metric.duration_ms, Some(0.0)); }
#[test] fn ansi_and_whitespace_removed_from_preview() { assert_eq!(tool_call_result_preview(&maho_ext_api::AgentToolResult::text("evil\u{1b}[31mred\u{1b}[0m\n  output")), Some("evilred output".into())); }
#[test] fn image_only_has_no_preview() { let mut result = maho_ext_api::AgentToolResult::text(""); result.content = vec![maho_ext_api::ContentBlock::Image(maho_ext_api::ImageContent { data: "ZmFrZQ==".into(), mime_type: "image/png".into() })]; assert_eq!(tool_call_result_preview(&result), None); }
#[test] fn enriched_calls_stop_at_cap_but_metrics_continue() {
    let mut capture = ToolCallCapture { call_id: "call".into(), args: Some(json!({"path":"a"})), started_at: 10.0, metric: create_tool_call_metric("read", 10.0), include_details: true, args_truncated: false };
    let mut calls = Vec::new();
    for _ in 0..31 { record_tool_call(&mut calls, true, &mut capture, Some("result"), None, 20.0); }
    assert_eq!(calls.len(), 31);
    assert!(calls[29].get("args").is_some());
    assert!(calls[30].get("callId").is_none());
    assert_eq!(calls[30]["durationMs"], 10.0);
    assert_eq!(capture.metric.duration_ms, Some(10.0));
}
