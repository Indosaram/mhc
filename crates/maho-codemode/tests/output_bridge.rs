use maho_codemode::bridges::output_bridge::*;
use maho_ext_api::{
    AgentToolResult, ExecuteToolError, ExecuteToolErrorCode, ExecuteToolFuture, ExecuteToolOptions,
};
use serde_json::{Value, json};
use std::sync::Mutex;

#[derive(Default)]
struct Fixture {
    calls: Mutex<Vec<(String, Value)>>,
    unavailable: bool,
    error: Option<ExecuteToolErrorCode>,
}

impl OutputExecuteTool for Fixture {
    fn is_tool_available(&self, _: &str) -> Option<bool> {
        Some(!self.unavailable)
    }
    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        params: Value,
        options: ExecuteToolOptions,
    ) -> ExecuteToolFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .map_err(|error| ExecuteToolError {
                    code: ExecuteToolErrorCode::Blocked,
                    tool_name: name.into(),
                    message: error.to_string(),
                    active_tools: vec![],
                })?
                .push((name.into(), params.clone()));
            if let Some(code) = &self.error {
                return Err(ExecuteToolError {
                    code: code.clone(),
                    tool_name: name.into(),
                    message: "task failure".into(),
                    active_tools: vec![],
                });
            }
            if let Some(signal) = options.signal {
                signal.cancelled().await;
                return Err(ExecuteToolError {
                    code: ExecuteToolErrorCode::Blocked,
                    tool_name: name.into(),
                    message: signal
                        .reason()
                        .map_or_else(|| "aborted".into(), |reason| reason.message),
                    active_tools: vec![],
                });
            }
            let target = params
                .get("task_id")
                .or_else(|| params.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            Ok(AgentToolResult::text(format!(
                "TRANSCRIPT:{target}:{}\r\nsecond\r\nthird",
                params["mode"].as_str().unwrap_or("")
            )))
        })
    }
}

#[tokio::test]
async fn configured_tool_returns_scalar_transcript() {
    let fixture = Fixture::default();
    let result = run_eval_output(
        json!({"ids":["st_123"]}),
        "named_output",
        &fixture,
        ExecuteToolOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        result,
        EvalOutputResult::Transcript("TRANSCRIPT:st_123:full\nsecond\nthird".into())
    );
    assert_eq!(
        *fixture.calls.lock().unwrap(),
        [(
            "named_output".into(),
            json!({"task_id":"st_123", "mode":"full"})
        )]
    );
}

#[tokio::test]
async fn multiple_ids_preserve_input_order_and_tail_mode() {
    let fixture = Fixture::default();
    let result = run_eval_output(
        json!({"ids":["st_123","reviewer"], "format":"tail"}),
        "task_output",
        &fixture,
        ExecuteToolOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        result,
        EvalOutputResult::Transcripts(vec![
            "TRANSCRIPT:st_123:tail\nsecond\nthird".into(),
            "TRANSCRIPT:reviewer:tail\nsecond\nthird".into()
        ])
    );
    assert_eq!(
        fixture.calls.lock().unwrap()[1].1,
        json!({"name":"reviewer", "mode":"tail"})
    );
}

#[tokio::test]
async fn one_indexed_offset_and_limit_slice_lines() {
    assert_eq!(
        run_eval_output(
            json!({"ids":["st_123"], "offset":2,"limit":2}),
            "task_output",
            &Fixture::default(),
            ExecuteToolOptions::default()
        )
        .await
        .unwrap(),
        EvalOutputResult::Transcript("second\nthird".into())
    );
}

#[tokio::test]
async fn invalid_arguments_reject_before_execution() {
    let fixture = Fixture::default();
    for args in [
        json!({"ids":[]}),
        json!({"ids":[""]}),
        json!({"ids":["a"],"format":"json"}),
        json!({"ids":["a"],"offset":0}),
        json!({"ids":["a"],"limit":1.5}),
        json!({"ids":["a"],"block":true}),
    ] {
        assert!(matches!(
            run_eval_output(args, "task_output", &fixture, ExecuteToolOptions::default()).await,
            Err(OutputBridgeError::Arguments(_))
        ));
    }
    assert!(fixture.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unavailable_tool_does_not_execute() {
    let fixture = Fixture {
        unavailable: true,
        ..Default::default()
    };
    assert!(matches!(
        run_eval_output(
            json!({"ids":["a"]}),
            "task_output",
            &fixture,
            ExecuteToolOptions::default()
        )
        .await,
        Err(OutputBridgeError::Unavailable(_))
    ));
    assert!(fixture.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn task_errors_propagate() {
    let fixture = Fixture {
        error: Some(ExecuteToolErrorCode::Blocked),
        ..Default::default()
    };
    assert!(matches!(
        run_eval_output(
            json!({"ids":["a"]}),
            "task_output",
            &fixture,
            ExecuteToolOptions::default()
        )
        .await,
        Err(OutputBridgeError::Tool(_))
    ));
}

#[tokio::test]
async fn unknown_and_inactive_tools_report_unavailable() {
    for code in [
        ExecuteToolErrorCode::UnknownTool,
        ExecuteToolErrorCode::InactiveTool,
    ] {
        let fixture = Fixture {
            error: Some(code),
            ..Default::default()
        };
        assert!(matches!(
            run_eval_output(
                json!({"ids":["a"]}),
                "task_output",
                &fixture,
                ExecuteToolOptions::default()
            )
            .await,
            Err(OutputBridgeError::Unavailable(_))
        ));
    }
}

#[tokio::test]
async fn empty_out_of_range_slice_is_scalar() {
    assert_eq!(
        run_eval_output(
            json!({"ids":["a"], "offset":20}),
            "task_output",
            &Fixture::default(),
            ExecuteToolOptions::default()
        )
        .await
        .unwrap(),
        EvalOutputResult::Transcript(String::new())
    );
}

#[tokio::test]
async fn abort_signal_reaches_task_executor() {
    let fixture = Fixture::default();
    let controller = maho_ai::utils::abort::AbortController::new();
    let options = ExecuteToolOptions {
        signal: Some(controller.signal()),
        ..Default::default()
    };
    let call = run_eval_output(json!({"ids":["st_123"]}), "task_output", &fixture, options);
    tokio::pin!(call);
    std::future::poll_fn(|context| {
        assert!(call.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert_eq!(fixture.calls.lock().unwrap().len(), 1);
    controller.abort(Some(maho_ai::utils::abort::AbortReason::new(
        "AbortError",
        "cancelled by caller",
    )));
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), call)
        .await
        .unwrap();
    assert!(
        matches!(result, Err(OutputBridgeError::Tool(error)) if error.message == "cancelled by caller")
    );
}

/// senpi output-bridge.test.ts "returns task output from a real JavaScript cell": `output()` inside a
/// real Bun worker cell reaches the configured task output tool through the eval callable.
#[tokio::test]
async fn returns_task_output_from_a_real_javascript_cell() {
    use maho_codemode::{
        kernels::js::context_manager::JavaScriptKernel,
        tool::{
            detached_cell_manager::EvalDetachedCellManager, eval_tool::create_eval_tool,
            eval_tool_options::*, image_resize::*, types::*,
        },
    };
    use maho_tools::definition::{AbortSignal, ToolCall};
    use std::{collections::HashMap, sync::Arc};

    struct Manager(Arc<JavaScriptKernel>);
    impl EvalKernelManager for Manager {
        fn get_kernel(&self, _: EvalLanguage) -> EvalKernelFuture<'_, Arc<dyn EvalKernel>> {
            Box::pin(async { Ok(self.0.clone() as Arc<dyn EvalKernel>) })
        }
    }
    struct Images;
    impl EvalImageSdk for Images {
        fn resize_image<'a>(
            &'a self,
            _: Vec<u8>,
            _: &'a str,
            _: Option<usize>,
        ) -> ImageFuture<'a, Option<ResizedImage>> {
            Box::pin(async { Err("text-only scenario".into()) })
        }
        fn convert_to_png<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
        ) -> ImageFuture<'a, Option<EvalImageContent>> {
            Box::pin(async { Err("text-only scenario".into()) })
        }
    }

    let fixture = Arc::new(Fixture::default());
    let kernel = Arc::new(
        JavaScriptKernel::start(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            "output-bridge-e2e",
            1,
            None,
        )
        .await
        .expect("kernel"),
    );
    /// Records calls and answers without waiting on the cell signal (the cell always passes one).
    struct Shared(Arc<Fixture>);
    impl OutputExecuteTool for Shared {
        fn execute_tool<'a>(
            &'a self,
            name: &'a str,
            params: Value,
            _: ExecuteToolOptions,
        ) -> ExecuteToolFuture<'a> {
            Box::pin(async move {
                self.0
                    .calls
                    .lock()
                    .expect("calls")
                    .push((name.into(), params.clone()));
                let target = params["task_id"].as_str().unwrap_or("");
                Ok(AgentToolResult::text(format!(
                    "TRANSCRIPT:{target}:{}\r\nsecond\r\nthird",
                    params["mode"].as_str().unwrap_or("")
                )))
            })
        }
    }
    let tool = create_eval_tool(Arc::new(CreateEvalToolOptions {
        completion_handles: Arc::new(Default::default()),
        eval_handle_host: None,
        owner_session_id: None,
        kernel_manager: Arc::new(Manager(kernel.clone())),
        executor: Arc::new(Shared(fixture.clone())),
        list_tools: None,
        complete: None,
        settings: Default::default(),
        artifacts_dir: None,
        image_sdk: Arc::new(Images),
        cell_manager: Arc::new(std::sync::Mutex::new(EvalDetachedCellManager::new(
            Default::default(),
        ))),
        on_cell_settled: None,
        prompt: Default::default(),
        runtimes: HashMap::new(),
        mode: "print".into(),
        active_kernel_preludes: None,
    }))
    .expect("eval tool");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        (tool.execute)(ToolCall {
            id: "real-output-cell",
            params: json!({"language":"js","code":"const transcript = await output(\"st_123\"); return transcript;","summary":"output echo"}),
            signal: AbortSignal::default(),
            on_update: None,
            context: None,
        }),
    )
    .await;
    let closed = kernel.close().await;
    let result = result.expect("bounded cell").expect("eval result");
    closed.expect("kernel close");
    let text = result
        .content
        .iter()
        .filter_map(|part| match part {
            maho_tools::definition::ToolContent::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("TRANSCRIPT:st_123:full"), "{text}");
    assert_eq!(
        *fixture.calls.lock().unwrap(),
        [(
            "task_output".to_owned(),
            json!({"task_id":"st_123","mode":"full"})
        )]
    );
}

// typebox 1.3.34 `Errors` summaries captured at senpi 903b8336 (output-bridge.ts parseOutputArgs).
#[tokio::test]
async fn invalid_arguments_report_typebox_errors_in_order() {
    let fixture = Fixture::default();
    for (args, summary) in [
        (json!({}), "/ must have required properties ids"),
        (
            json!({"ids":["",""],"offset":-1}),
            "/ids/0 must not have fewer than 1 characters; /ids/1 must not have fewer than 1 characters; /offset must be >= 1",
        ),
        (
            json!({"ids":["a"],"limit":0,"offset":0,"extra":1}),
            "/extra schema is false; / must not have additional properties; /offset must be >= 1; /limit must be >= 1",
        ),
        (
            json!({"ids":["a"],"format":"x"}),
            "/format must be equal to constant; /format must be equal to constant; /format must match a schema in anyOf",
        ),
        (json!({"ids":"a"}), "/ids must be array"),
    ] {
        let error = run_eval_output(args, "task_output", &fixture, ExecuteToolOptions::default())
            .await
            .err()
            .expect("invalid arguments");
        assert_eq!(
            error.to_string(),
            format!("output() received invalid arguments: {summary}")
        );
    }
    assert!(fixture.calls.lock().unwrap().is_empty());
}
