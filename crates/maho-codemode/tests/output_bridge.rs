use maho_codemode::bridges::output_bridge::*;
use maho_ext_api::{AgentToolResult, ExecuteToolError, ExecuteToolErrorCode, ExecuteToolFuture, ExecuteToolOptions};
use serde_json::{Value, json};
use std::sync::Mutex;

#[derive(Default)]
struct Fixture { calls: Mutex<Vec<(String, Value)>>, unavailable: bool, error: Option<ExecuteToolErrorCode> }

impl OutputExecuteTool for Fixture {
    fn is_tool_available(&self, _: &str) -> Option<bool> { Some(!self.unavailable) }
    fn execute_tool<'a>(&'a self, name: &'a str, params: Value, options: ExecuteToolOptions) -> ExecuteToolFuture<'a> {
        Box::pin(async move {
            self.calls.lock().map_err(|error| ExecuteToolError { code: ExecuteToolErrorCode::Blocked, tool_name: name.into(), message: error.to_string(), active_tools: vec![] })?.push((name.into(), params.clone()));
            if let Some(code) = &self.error { return Err(ExecuteToolError { code: code.clone(), tool_name: name.into(), message: "task failure".into(), active_tools: vec![] }); }
            if let Some(signal) = options.signal {
                signal.cancelled().await;
                return Err(ExecuteToolError { code: ExecuteToolErrorCode::Blocked, tool_name: name.into(), message: signal.reason().map_or_else(|| "aborted".into(), |reason| reason.message), active_tools: vec![] });
            }
            let target = params.get("task_id").or_else(|| params.get("name")).and_then(Value::as_str).unwrap_or("");
            Ok(AgentToolResult::text(format!("TRANSCRIPT:{target}:{}\r\nsecond\r\nthird", params["mode"].as_str().unwrap_or(""))))
        })
    }
}

#[tokio::test]
async fn configured_tool_returns_scalar_transcript() {
    let fixture = Fixture::default();
    let result = run_eval_output(json!({"ids":["st_123"]}), "named_output", &fixture, ExecuteToolOptions::default()).await.unwrap();
    assert_eq!(result, EvalOutputResult::Transcript("TRANSCRIPT:st_123:full\nsecond\nthird".into()));
    assert_eq!(*fixture.calls.lock().unwrap(), [("named_output".into(), json!({"task_id":"st_123", "mode":"full"}))]);
}

#[tokio::test]
async fn multiple_ids_preserve_input_order_and_tail_mode() {
    let fixture = Fixture::default();
    let result = run_eval_output(json!({"ids":["st_123","reviewer"], "format":"tail"}), "task_output", &fixture, ExecuteToolOptions::default()).await.unwrap();
    assert_eq!(result, EvalOutputResult::Transcripts(vec!["TRANSCRIPT:st_123:tail\nsecond\nthird".into(), "TRANSCRIPT:reviewer:tail\nsecond\nthird".into()]));
    assert_eq!(fixture.calls.lock().unwrap()[1].1, json!({"name":"reviewer", "mode":"tail"}));
}

#[tokio::test]
async fn one_indexed_offset_and_limit_slice_lines() {
    assert_eq!(run_eval_output(json!({"ids":["st_123"], "offset":2,"limit":2}), "task_output", &Fixture::default(), ExecuteToolOptions::default()).await.unwrap(), EvalOutputResult::Transcript("second\nthird".into()));
}

#[tokio::test]
async fn invalid_arguments_reject_before_execution() {
    let fixture = Fixture::default();
    for args in [json!({"ids":[]}), json!({"ids":[""]}), json!({"ids":["a"],"format":"json"}), json!({"ids":["a"],"offset":0}), json!({"ids":["a"],"limit":1.5}), json!({"ids":["a"],"block":true})] {
        assert!(matches!(run_eval_output(args, "task_output", &fixture, ExecuteToolOptions::default()).await, Err(OutputBridgeError::Arguments(_))));
    }
    assert!(fixture.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unavailable_tool_does_not_execute() {
    let fixture = Fixture { unavailable: true, ..Default::default() };
    assert!(matches!(run_eval_output(json!({"ids":["a"]}), "task_output", &fixture, ExecuteToolOptions::default()).await, Err(OutputBridgeError::Unavailable(_))));
    assert!(fixture.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn task_errors_propagate() {
    let fixture = Fixture { error: Some(ExecuteToolErrorCode::Blocked), ..Default::default() };
    assert!(matches!(run_eval_output(json!({"ids":["a"]}), "task_output", &fixture, ExecuteToolOptions::default()).await, Err(OutputBridgeError::Tool(_))));
}

#[tokio::test]
async fn unknown_and_inactive_tools_report_unavailable() {
    for code in [ExecuteToolErrorCode::UnknownTool, ExecuteToolErrorCode::InactiveTool] {
        let fixture = Fixture { error: Some(code), ..Default::default() };
        assert!(matches!(run_eval_output(json!({"ids":["a"]}), "task_output", &fixture, ExecuteToolOptions::default()).await, Err(OutputBridgeError::Unavailable(_))));
    }
}

#[tokio::test]
async fn empty_out_of_range_slice_is_scalar() {
    assert_eq!(run_eval_output(json!({"ids":["a"], "offset":20}), "task_output", &Fixture::default(), ExecuteToolOptions::default()).await.unwrap(), EvalOutputResult::Transcript(String::new()));
}

#[tokio::test]
async fn abort_signal_reaches_task_executor() {
    let fixture = Fixture::default();
    let controller = maho_ai::utils::abort::AbortController::new();
    let options = ExecuteToolOptions { signal: Some(controller.signal()), ..Default::default() };
    let call = run_eval_output(json!({"ids":["st_123"]}), "task_output", &fixture, options);
    tokio::pin!(call);
    std::future::poll_fn(|context| {
        assert!(call.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    }).await;
    assert_eq!(fixture.calls.lock().unwrap().len(), 1);
    controller.abort(Some(maho_ai::utils::abort::AbortReason::new("AbortError", "cancelled by caller")));
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), call).await.unwrap();
    assert!(matches!(result, Err(OutputBridgeError::Tool(error)) if error.message == "cancelled by caller"));
}
