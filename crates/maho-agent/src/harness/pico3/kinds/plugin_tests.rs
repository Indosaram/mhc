use super::*;
use crate::harness::context::{background_context, with_cancel};
use super::super::task_api::tests::AdapterRuntime;
use std::{cell::Cell, rc::Rc};

fn plugin_task() -> Task<PluginInput, PluginCheckpoint> {
    Task { id: 4, conversation_id: 2, kind: "pi.plugin".into(),
        input: PluginInput { handler: "echo".into(), input: JsonValue::Bool(true) },
        status: TaskStatus::Running, checkpoint: Some(PluginCheckpoint { phase: PluginPhase::Started }),
        abort: None, outcome: None, after: Vec::new(), owns: Vec::new(), background: None }
}

struct Echo { calls: Rc<Cell<u32>> }
impl PluginHandler for Echo {
    fn call<'a>(&'a self, input: JsonValue, api: &'a dyn ToolApi, _: &'a Context) -> LocalBoxFuture<'a, Result<JsonValue>> {
        Box::pin(async move {
            self.calls.set(self.calls.get().checked_add(1).expect("call count"));
            assert_eq!(api.task_id(), 4);
            Ok(input)
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn initial_checkpoints_before_handler_when_plugin_starts() {
    let mut runtime = AdapterRuntime::new();
    let calls = Rc::new(Cell::new(0));
    runtime.plugins.insert("echo".into(), Rc::new(Echo { calls: calls.clone() }));
    let kind = plugin();
    let task = plugin_task();
    let ctx = background_context();
    let step = kind.initial(&task, &runtime, &ctx).await.expect("initial");
    assert_eq!(calls.get(), 1);
    assert_eq!(runtime.tx.borrow().checkpoints.len(), 1);
    assert_eq!(runtime.tx.borrow().checkpoints[0].phase, "started");
    let Step::Done(close) = step else { panic!("done"); };
    let erased: Task = Stored::from_json(to_stored(&task).expect("stored").as_json().clone()).decode().expect("task");
    let mut tx = AdapterRuntime::new().tx.into_inner();
    let completion = close(&mut tx, &erased, &ctx).await.expect("close");
    assert!(matches!(completion, Completion::Completed { result: JsonValue::Bool(true) }));
}

#[tokio::test(flavor = "current_thread")]
async fn resume_runs_handler_without_checkpointing_again() {
    let mut runtime = AdapterRuntime::new();
    let calls = Rc::new(Cell::new(0));
    runtime.plugins.insert("echo".into(), Rc::new(Echo { calls: calls.clone() }));
    let kind = plugin();
    let task = plugin_task();
    let ctx = background_context();
    let step = kind.phase("started", &task, &runtime, &ctx).await.expect("resume");
    assert_eq!(calls.get(), 1);
    assert!(runtime.tx.borrow().checkpoints.is_empty());
    assert!(matches!(step, Step::Done(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn missing_handler_finishes_with_typed_failure() {
    let runtime = AdapterRuntime::new();
    let kind = plugin();
    let task = plugin_task();
    let ctx = background_context();
    let Step::Done(close) = kind.phase("started", &task, &runtime, &ctx).await.expect("resume") else { panic!("done"); };
    let erased: Task = Stored::from_json(to_stored(&task).expect("stored").as_json().clone()).decode().expect("task");
    let mut tx = AdapterRuntime::new().tx.into_inner();
    let completion = close(&mut tx, &erased, &ctx).await.expect("close");
    assert!(matches!(completion, Completion::Failed { failure } if failure.reason == PluginFailureReason::MissingHandler && failure.detail == "echo"));
}

#[tokio::test(flavor = "current_thread")]
async fn abort_returns_null_when_closure_is_applied() {
    let runtime = AdapterRuntime::new();
    let kind = plugin();
    let task = plugin_task();
    let ctx = background_context();
    let close = kind.abort(&task, &runtime, &ctx).await.expect("abort");
    let erased: Task = Stored::from_json(to_stored(&task).expect("stored").as_json().clone()).decode().expect("task");
    let mut tx = AdapterRuntime::new().tx.into_inner();
    let result = close(&mut tx, &erased, &ctx).await.expect("close");
    assert_eq!(result, JsonValue::Null);
}

#[test]
fn handler_result_is_completed_when_successful() {
    let ctx = background_context();
    let value = JsonValue::object(vec![("ok".into(), JsonValue::Bool(true))]);
    let completion = handler_completion(Ok(value.clone()), &ctx).expect("completion");
    assert!(matches!(completion, Completion::Completed { result } if result == value));
}

#[test]
fn handler_error_becomes_failure_when_not_cancelled() {
    let ctx = background_context();
    let completion = handler_completion(Err(Error::Closed), &ctx).expect("completion");
    assert!(matches!(completion, Completion::Failed { failure } if failure.reason == PluginFailureReason::Threw));
}

#[test]
fn handler_error_propagates_when_context_cancelled() {
    let (ctx, cancel) = with_cancel(&background_context());
    cancel.cancel(None);
    let result = handler_completion(Err(Error::Closed), &ctx);
    assert!(matches!(result, Err(Error::Closed)));
}

#[test]
fn success_remains_completed_when_handler_ignored_cancellation() {
    let (ctx, cancel) = with_cancel(&background_context());
    cancel.cancel(None);
    let completion = handler_completion(Ok(JsonValue::Null), &ctx).expect("completion");
    assert!(matches!(completion, Completion::Completed { result: JsonValue::Null }));
}

#[test]
fn checkpoint_parses_when_resuming_started_phase() {
    let json = r#"{"phase":"started"}"#;
    let checkpoint: PluginCheckpoint = serde_json::from_str(json).expect("checkpoint");
    assert!(matches!(checkpoint.phase, PluginPhase::Started));
}
