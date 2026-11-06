use crate::omo_native_parallel::ParallelTelemetryRegistry;
use serde_json::{Value, json};
fn event() -> Value {
    json!({"version":1,"detailLevel":"full","cellId":"cell","language":"js","ok":true,"startedAt":1000,"completedAt":1030,"durationMs":30,"kernelDurationMs":24,"detached":false,"toolCallCount":2,"pendingToolCallCount":0,"toolCalls":[],"distinctToolsCalled":["read","bash"],"toolAggregates":{"read":{"count":1,"totalDurationMs":12,"okCount":1,"errorCount":0,"pendingCount":0},"bash":{"count":1,"totalDurationMs":18,"okCount":1,"errorCount":0,"pendingCount":0}},"toolAggregatesTruncated":false})
}
fn record(r: &mut ParallelTelemetryRegistry, session: &str, kind: &str, at: u32) {
    r.record(
        session,
        json!({"kind":kind,"toolCallId":"cell","toolName":"eval","atMs":at}),
    );
}
#[test]
fn exact_session_rollup() {
    let mut r = ParallelTelemetryRegistry::default();
    record(&mut r, "s", "start", 1000);
    r.record_eval_execution(&event());
    let s = r.snapshot("s").unwrap();
    assert_eq!(s.eval_execution.event_count, 1);
    assert_eq!(s.eval_execution.nested_tool_call_count, 2);
    assert_eq!(s.eval_execution.nested_tool_call_ok_count, 2);
}
#[test]
fn detached_after_outer_end() {
    let mut r = ParallelTelemetryRegistry::default();
    record(&mut r, "s", "start", 1000);
    record(&mut r, "s", "end", 1010);
    let mut e = event();
    e["detached"] = json!(true);
    r.record_eval_execution(&e);
    assert_eq!(r.snapshot("s").unwrap().eval_execution.detached_count, 1);
}
#[test]
fn duplicate_consumed_once() {
    let mut r = ParallelTelemetryRegistry::default();
    record(&mut r, "s", "start", 1000);
    r.record_eval_execution(&event());
    r.record_eval_execution(&event());
    assert_eq!(r.snapshot("s").unwrap().eval_execution.event_count, 1);
}
#[test]
fn collision_fails_closed() {
    let mut r = ParallelTelemetryRegistry::default();
    record(&mut r, "a", "start", 1000);
    record(&mut r, "b", "start", 1000);
    r.record_eval_execution(&event());
    assert_eq!(r.snapshot("a").unwrap().eval_execution.event_count, 0);
    assert_eq!(r.snapshot("b").unwrap().eval_execution.event_count, 0);
}
#[test]
fn rejected_consumes_correlation() {
    let mut r = ParallelTelemetryRegistry::default();
    record(&mut r, "s", "start", 1000);
    let mut e = event();
    e["version"] = json!(2);
    r.record_eval_execution(&e);
    r.record_eval_execution(&event());
    let s = r.snapshot("s").unwrap();
    assert_eq!(s.eval_execution.event_count, 0);
    assert_eq!(s.eval_execution.rejected_count, 1);
}
#[test]
fn unavailable_bus_keeps_wave() {
    let mut r = ParallelTelemetryRegistry::default();
    record(&mut r, "s", "start", 1000);
    record(&mut r, "s", "end", 1030);
    let s = r.snapshot("s").unwrap();
    assert!(!s.eval_execution_event_bus_available);
    assert_eq!(s.assembly.counters.paired_calls, 1);
    assert_eq!(s.eval_execution.event_count, 0);
}
#[test]
fn late_eval_after_clear_no_resurrection() {
    let mut r = ParallelTelemetryRegistry::default();
    record(&mut r, "s", "start", 1000);
    r.clear("s");
    r.record_eval_execution(&event());
    assert_eq!(r.size(), 0);
}

// omo-native-parallel-eval.test.ts: an event-capable host installs exactly one eval event handler.
// The native EventBus exposes no handler count, so a single emit must be consumed exactly once.
#[tokio::test]
async fn event_capable_host_installs_exactly_one_eval_handler() {
    use crate::telemetry_test_support::{api, context, dispatch};
    use maho_ext_api::ExtensionEvent;
    use std::sync::{Arc, Mutex};
    let home = tempfile::tempdir().expect("telemetry home");
    let mut api = api();
    let registry = Arc::new(Mutex::new(ParallelTelemetryRegistry::default()));
    let _subscription = crate::omo_native_parallel::register_omo_native_parallel_telemetry(
        &mut api,
        Arc::clone(&registry),
        Arc::new(|| 1000.0),
    );
    let ctx = context(home.path(), "s");
    dispatch(
        &api,
        ExtensionEvent::ToolExecutionStart {
            parent_tool_call_id: None,
            tool_call_id: "cell".into(),
            tool_name: "eval".into(),
            args: json!({}),
        },
        &ctx,
    )
    .await;
    api.events
        .emit(crate::omo_native_eval::SENPI_EVAL_EXECUTION_EVENT, &event());
    let state = registry.lock().expect("registry");
    let snapshot = state.snapshot("s").expect("session snapshot");
    assert!(snapshot.eval_execution_event_bus_available);
    assert_eq!(snapshot.eval_execution.event_count, 1);
}
