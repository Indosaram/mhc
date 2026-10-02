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
