use std::cell::Cell;
use std::rc::Rc;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::*;

fn clock(start: u64) -> (Rc<Cell<u64>>, impl Fn() -> u64) {
    let now = Rc::new(Cell::new(start));
    let reader = Rc::clone(&now);
    (now, move || reader.get())
}

fn message_start() -> ManagedChildEvent {
    ManagedChildEvent {
        message: Some(json!({ "role": "assistant", "content": [] })),
        ..ManagedChildEvent::of("message_start")
    }
}

fn message_end(message: Value) -> ManagedChildEvent {
    ManagedChildEvent {
        message: Some(message),
        ..ManagedChildEvent::of("message_end")
    }
}

fn assistant_usage(usage: Value) -> ManagedChildEvent {
    message_end(json!({ "role": "assistant", "content": [], "usage": usage }))
}

fn tool_start(
    id: Option<&str>,
    name: &str,
    args: Option<Value>,
    input: Option<Value>,
) -> ManagedChildEvent {
    ManagedChildEvent {
        tool_call_id: id.map(str::to_string),
        tool_name: Some(name.to_string()),
        args,
        input,
        ..ManagedChildEvent::of("tool_execution_start")
    }
}

fn tool_end(id: Option<&str>, name: &str, result: Option<Value>) -> ManagedChildEvent {
    ManagedChildEvent {
        tool_call_id: id.map(str::to_string),
        tool_name: Some(name.to_string()),
        result,
        is_error: Some(false),
        ..ManagedChildEvent::of("tool_execution_end")
    }
}

fn assert_close(actual: Option<f64>, expected: f64) {
    let actual = actual.expect("value present");
    assert!((actual - expected).abs() < 1e-10, "{actual} != {expected}");
}

// ---- run-stats.test.ts ----

#[test]
fn totals_and_tps_derive_from_generation_time() {
    let (now, read) = clock(1_000);
    let mut tracker = create_run_stats_tracker(1_000, read);
    now.set(2_000);
    tracker.accept(&message_start());
    now.set(3_000);
    tracker.accept(&message_end(json!({
        "role": "assistant", "content": [{ "type": "text", "text": "hi" }],
        "usage": { "output": 100, "totalTokens": 400 },
    })));
    tracker.accept(&tool_start(None, "read", Some(json!({})), None));
    now.set(9_000);
    tracker.accept(&tool_end(None, "read", None));
    now.set(10_000);
    tracker.accept(&message_end(json!({
        "role": "assistant", "content": [{ "type": "text", "text": "done" }],
        "usage": { "output": 50, "totalTokens": 600 },
    })));
    assert_eq!(
        tracker.snapshot(11_000),
        TaskRunStats {
            runtime_ms: 10_000,
            turns: 2,
            tool_calls: 1,
            output_tokens: Some(150),
            total_tokens: Some(1_000),
            generation_ms: Some(2_000),
            tokens_per_second: Some(75.0),
            ..TaskRunStats::default()
        }
    );
}

#[test]
fn non_assistant_and_missing_usage_report_only_counted_facts() {
    let mut tracker = create_run_stats_tracker(1_000, || 1_500);
    tracker.accept(&message_end(json!({ "role": "user", "content": "hello" })));
    tracker.accept(&message_end(
        json!({ "role": "assistant", "content": [{ "type": "text", "text": "ok" }] }),
    ));
    assert_eq!(
        tracker.snapshot(2_000),
        TaskRunStats {
            runtime_ms: 1_000,
            turns: 1,
            tool_calls: 0,
            generation_ms: Some(500),
            ..TaskRunStats::default()
        }
    );
}

#[test]
fn snake_case_usage_fields_are_read_as_fallback() {
    let (now, read) = clock(1_000);
    let mut tracker = create_run_stats_tracker(1_000, read);
    now.set(2_000);
    tracker.accept(&assistant_usage(
        json!({ "output_tokens": 30, "total_tokens": 90 }),
    ));
    let snapshot = tracker.snapshot(2_000);
    assert_eq!(snapshot.output_tokens, Some(30));
    assert_eq!(snapshot.total_tokens, Some(90));
    assert_eq!(snapshot.tokens_per_second, Some(30.0));
}

#[test]
fn sub_ten_tps_keeps_one_decimal() {
    let (now, read) = clock(1_000);
    let mut tracker = create_run_stats_tracker(1_000, read);
    now.set(5_000);
    tracker.accept(&assistant_usage(json!({ "output": 10, "totalTokens": 10 })));
    assert_eq!(tracker.snapshot(5_000).tokens_per_second, Some(2.5));
}

#[test]
fn window_is_the_arrival_clock_gap() {
    let (now, read) = clock(10_000);
    let mut tracker = create_run_stats_tracker(10_000, read);
    tracker.accept(&message_start());
    now.set(13_000);
    tracker.accept(&message_end(json!({
        "role": "assistant", "content": [{ "type": "text", "text": "done" }],
        "usage": { "output": 300, "totalTokens": 600 },
    })));
    let snapshot = tracker.snapshot(13_000);
    assert_eq!(snapshot.generation_ms, Some(3_000));
    assert_eq!(snapshot.tokens_per_second, Some(100.0));
}

#[test]
fn collapsed_only_window_omits_tps() {
    let (_now, read) = clock(1_000);
    let mut tracker = create_run_stats_tracker(1_000, read);
    tracker.accept(&message_end(json!({
        "role": "assistant", "content": [{ "type": "text", "text": "done" }],
        "usage": { "output": 400, "totalTokens": 900 },
    })));
    let snapshot = tracker.snapshot(3_000);
    assert_eq!(snapshot.generation_ms, None);
    assert_eq!(snapshot.tokens_per_second, None);
}

#[test]
fn latest_and_whole_run_cache_rates_stay_distinct() {
    let (now, read) = clock(1_000);
    let mut tracker = create_run_stats_tracker(1_000, read);
    now.set(2_000);
    tracker.accept(&assistant_usage(json!({
        "input": 10, "output": 20, "cacheRead": 30, "cacheWrite": 10, "totalTokens": 70, "cost": 0.0125,
    })));
    tracker.accept(&assistant_usage(json!({
        "input": 10, "output": 20, "cacheRead": 90, "cacheWrite": 0, "totalTokens": 120, "cost": 0.01,
    })));
    let snapshot = tracker.snapshot(2_000);
    assert_close(snapshot.cost_usd, 0.0225);
    assert_close(snapshot.cache_hit_rate_last, 90.0 / 100.0);
    assert_close(snapshot.cache_hit_rate_run, 120.0 / 150.0);
}

#[test]
fn object_cost_total_is_summed() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    tracker.accept(&assistant_usage(json!({
        "input": 5, "output": 5, "cacheRead": 5, "cacheWrite": 0,
        "cost": { "input": 0.1, "output": 0.2, "total": 0.3 },
    })));
    assert_close(tracker.snapshot(2_000).cost_usd, 0.3);
}

#[test]
fn usage_without_cache_or_cost_omits_both() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    tracker.accept(&assistant_usage(json!({ "output": 40, "totalTokens": 40 })));
    let snapshot = tracker.snapshot(2_000);
    assert_eq!(snapshot.cost_usd, None);
    assert_eq!(snapshot.cache_hit_rate_last, None);
    assert_eq!(snapshot.cache_hit_rate_run, None);
}

#[test]
fn snake_case_cache_fields_with_zero_reads_rate_zero() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    tracker.accept(&assistant_usage(json!({
        "input_tokens": 100, "output_tokens": 10, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 20,
    })));
    assert_eq!(tracker.snapshot(2_000).cache_hit_rate_last, Some(0.0));
    assert_eq!(tracker.snapshot(2_000).cache_hit_rate_run, Some(0.0));
}

#[test]
fn non_finite_cost_and_negative_cache_inputs_cannot_poison_stats() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    // JSON cannot carry Infinity here; a non-finite f64 becomes `null`, the host-boundary shape.
    tracker.accept(&assistant_usage(json!({
        "input": -10, "output": 5, "cacheRead": 30, "cacheWrite": -5, "totalTokens": 20,
        "cost": f64::INFINITY,
    })));
    let snapshot = tracker.snapshot(2_000);
    assert_eq!(snapshot.cost_usd, None);
    assert_eq!(snapshot.cache_hit_rate_last, Some(1.0));
    assert_eq!(snapshot.cache_hit_rate_run, Some(1.0));
    let serialized = serde_json::to_string(&snapshot).expect("json");
    assert!(!serialized.contains("null"));
}

#[test]
fn object_cost_with_non_finite_total_is_omitted() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    tracker.accept(&assistant_usage(json!({
        "input": 10, "cacheRead": 0, "cacheWrite": 0, "cost": { "total": f64::INFINITY },
    })));
    assert_eq!(tracker.snapshot(2_000).cost_usd, None);
}

#[test]
fn overflowing_cumulative_totals_are_omitted() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    let overflowing_turn =
        assistant_usage(json!({ "input": 0, "cacheRead": 1e308, "cacheWrite": 0, "cost": 1e308 }));
    tracker.accept(&overflowing_turn);
    tracker.accept(&overflowing_turn);
    let snapshot = tracker.snapshot(2_000);
    assert_eq!(snapshot.cost_usd, None);
    assert_eq!(snapshot.cache_hit_rate_last, Some(1.0));
    assert_eq!(snapshot.cache_hit_rate_run, None);
    let serialized = serde_json::to_string(&snapshot).expect("json");
    assert!(!serialized.contains("null"));
}

#[test]
fn latest_valid_request_rate_is_retained() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    tracker.accept(&assistant_usage(
        json!({ "input": 10, "cacheRead": 90, "cacheWrite": 0 }),
    ));
    tracker.accept(&assistant_usage(json!({ "output": 10, "totalTokens": 10 })));
    assert_close(tracker.snapshot(2_000).cache_hit_rate_last, 0.9);
    assert_close(tracker.snapshot(2_000).cache_hit_rate_run, 0.9);
}

#[test]
fn measured_plus_collapsed_window_omits_tps() {
    let (now, read) = clock(1_000);
    let mut tracker = create_run_stats_tracker(1_000, read);
    now.set(2_000);
    tracker.accept(&message_start());
    now.set(4_000);
    tracker.accept(&assistant_usage(
        json!({ "output": 100, "totalTokens": 200 }),
    ));
    tracker.accept(&assistant_usage(
        json!({ "output": 300, "totalTokens": 600 }),
    ));
    let snapshot = tracker.snapshot(10_000);
    assert_eq!(snapshot.runtime_ms, 9_000);
    assert_eq!(snapshot.output_tokens, Some(400));
    assert_eq!(snapshot.total_tokens, Some(800));
    assert_eq!(snapshot.generation_ms, Some(2_000));
    assert_eq!(snapshot.tokens_per_second, None);
}

// ---- run-stats-eval.test.ts ----

fn eval_result_with_two_tools() -> Value {
    json!({
        "content": [{ "type": "text", "text": "done" }],
        "details": {
            "language": "py", "durationMs": 10,
            "toolCalls": [{ "name": "read", "ok": true }, { "name": "bash", "ok": true }],
            "truncated": false,
        },
    })
}

#[test]
fn eval_with_two_nested_calls_totals_three() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    tracker.accept(&tool_start(
        Some("eval-1"),
        "eval",
        Some(json!({ "action": "run", "language": "py", "code": "read(); bash()" })),
        None,
    ));
    tracker.accept(&tool_end(
        Some("eval-1"),
        "eval",
        Some(eval_result_with_two_tools()),
    ));
    assert_eq!(tracker.snapshot(2_000).tool_calls, 3);
}

#[test]
fn eval_without_nested_calls_counts_once() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    tracker.accept(&tool_start(
        Some("eval-empty"),
        "eval",
        Some(json!({ "language": "py", "code": "print('done')" })),
        None,
    ));
    tracker.accept(&tool_end(
        Some("eval-empty"),
        "eval",
        Some(json!({ "content": [], "details": { "toolCalls": [] } })),
    ));
    assert_eq!(tracker.snapshot(2_000).tool_calls, 1);
}

#[test]
fn eval_control_call_counts_only_itself() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    tracker.accept(&tool_start(
        Some("eval-peek"),
        "eval",
        Some(json!({ "action": "peek", "cell_id": "cell-1" })),
        None,
    ));
    tracker.accept(&tool_end(
        Some("eval-peek"),
        "eval",
        Some(eval_result_with_two_tools()),
    ));
    assert_eq!(tracker.snapshot(2_000).tool_calls, 1);
}

#[test]
fn input_only_eval_control_call_counts_only_itself() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    tracker.accept(&tool_start(
        Some("eval-input-peek"),
        "eval",
        None,
        Some(json!({ "action": "peek", "cell_id": "cell-1" })),
    ));
    tracker.accept(&tool_end(
        Some("eval-input-peek"),
        "eval",
        Some(eval_result_with_two_tools()),
    ));
    assert_eq!(tracker.snapshot(2_000).tool_calls, 1);
}

#[test]
fn malformed_eval_summaries_count_only_valid_calls() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    tracker.accept(&tool_start(
        Some("eval-malformed"),
        "eval",
        Some(json!({ "language": "py", "code": "read(); bash()" })),
        None,
    ));
    tracker.accept(&tool_end(
        Some("eval-malformed"),
        "eval",
        Some(json!({ "content": [], "details": { "toolCalls": [
            { "name": "read", "ok": true },
            null,
            { "name": "bash" },
            { "name": "", "ok": true },
            { "name": "bash", "ok": false },
        ] } })),
    ));
    assert_eq!(tracker.snapshot(2_000).tool_calls, 3);
}

#[test]
fn non_eval_tool_with_details_counts_once() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    tracker.accept(&tool_start(
        Some("read-1"),
        "read",
        Some(json!({ "path": "src/foo.ts" })),
        None,
    ));
    tracker.accept(&tool_end(
        Some("read-1"),
        "read",
        Some(eval_result_with_two_tools()),
    ));
    assert_eq!(tracker.snapshot(2_000).tool_calls, 1);
}

#[test]
fn duplicated_eval_end_counts_nested_once() {
    let mut tracker = create_run_stats_tracker(1_000, || 2_000);
    let end_event = tool_end(
        Some("eval-duplicate"),
        "eval",
        Some(eval_result_with_two_tools()),
    );
    tracker.accept(&tool_start(
        Some("eval-duplicate"),
        "eval",
        Some(json!({ "language": "py", "code": "read(); bash()" })),
        None,
    ));
    tracker.accept(&end_event);
    tracker.accept(&end_event);
    assert_eq!(tracker.snapshot(2_000).tool_calls, 3);
}
