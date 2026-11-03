use maho_codemode::bridges::schema_hint::{append_schema_hint, render_schema_hint};
use serde_json::json;

#[test]
fn required_properties_precede_optional_properties() {
    let rendered = render_schema_hint("t", &json!({"properties":{"optional":{"type":"string"},"needed":{"type":"number"}},"required":["needed"]})).unwrap();
    assert!(rendered.find("needed").unwrap() < rendered.find("optional").unwrap());
}

#[test]
fn const_union_and_long_enum_labels() {
    let rendered = render_schema_hint("t", &json!({"properties":{"kind":{"const":"batch"},"mode":{"enum":["a","b","c","d","e","f","g","h","i","j"]},"either":{"oneOf":[{"type":"string"},{"type":"number"}]}}})).unwrap();
    assert!(rendered.contains("kind?: \"batch\""));
    assert!(rendered.contains("2 more)"));
    assert!(rendered.contains("either?: string | number"));
}

#[test]
fn large_schema_respects_budget_and_names_full_schema_tool() {
    let properties = (0..60)
        .map(|index| {
            (
                format!("property_{index}"),
                json!({"type":"string","description":"x".repeat(70)}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let rendered = render_schema_hint("big_tool", &json!({"properties":properties})).unwrap();
    assert!(rendered.encode_utf16().count() <= 1200);
    assert!(rendered.contains("[truncated; call tool_schema(\"big_tool\") for the full schema]"));
}

#[test]
fn empty_schema_leaves_error_untouched() {
    assert_eq!(
        append_schema_hint("validation failed", "t", &json!({})),
        "validation failed"
    );
    assert!(render_schema_hint("t", &json!([])).is_none());
}

#[test]
fn array_items_show_nested_required_fields() {
    let rendered = render_schema_hint("t", &json!({"properties":{"steps":{"type":"array","items":{"type":"object","properties":{"tool":{"type":"string"}},"required":["tool"]}}},"required":["steps"]})).unwrap();
    assert!(rendered.contains("steps: array<object>"));
    assert!(rendered.contains("    tool: string"));
}

#[test]
fn description_truncation_counts_utf16_units() {
    let description = "😀".repeat(41);
    let rendered = render_schema_hint(
        "t",
        &json!({"properties":{"value":{"type":"string","description":description}}}),
    )
    .unwrap();
    let description = rendered.split(" — ").nth(1).unwrap();
    assert_eq!(description.encode_utf16().count(), 81);
    assert_eq!(
        description
            .chars()
            .filter(|character| *character == '😀')
            .count(),
        40
    );
}

mod cell_handler_failures {
    //! The four `tool.execute` cases of senpi test/tool-schema-hint.test.ts, driven through the real CellHandler.
    use maho_codemode::{
        bridges::{output_bridge::OutputExecuteTool, schema_bridge::EvalSchemaToolInfo},
        tool::{
            cell_handler::{CellBridgeRuntime, CellHandler},
            cell_runtime::{CellResultBuilder, CellState},
            image::EvalOutputOptions,
            image_resize::{EvalImageContent, EvalImageSdk, ImageFuture, ResizedImage},
            types::{EvalLanguage, EvalToolInput},
        },
    };
    use maho_ext_api::{
        AgentToolResult, ExecuteToolError, ExecuteToolErrorCode, ExecuteToolFuture,
        ExecuteToolOptions,
    };
    use serde_json::{Value, json};
    use std::sync::Arc;

    struct Sdk;
    impl EvalImageSdk for Sdk {
        fn resize_image<'a>(
            &'a self,
            _: Vec<u8>,
            _: &'a str,
            _: Option<usize>,
        ) -> ImageFuture<'a, Option<ResizedImage>> {
            Box::pin(async { Err("unexpected image resize".into()) })
        }
        fn convert_to_png<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
        ) -> ImageFuture<'a, Option<EvalImageContent>> {
            Box::pin(async { Err("unexpected conversion".into()) })
        }
    }

    struct Failing(ExecuteToolErrorCode, String);
    impl OutputExecuteTool for Failing {
        fn execute_tool<'a>(
            &'a self,
            name: &'a str,
            _: Value,
            _: ExecuteToolOptions,
        ) -> ExecuteToolFuture<'a> {
            let error = ExecuteToolError {
                code: self.0.clone(),
                tool_name: name.into(),
                message: self.1.clone(),
                active_tools: vec![],
            };
            Box::pin(async move { Err::<AgentToolResult, _>(error) })
        }
    }

    fn batch_tool() -> EvalSchemaToolInfo {
        EvalSchemaToolInfo {
            name: "mcp_computer_use_batch".into(),
            description: Some("Run 1-20 canonical steps.".into()),
            parameters: Some(
                json!({"type":"object","properties":{"steps":{"type":"array","description":"Canonical steps to run in order.","items":{"type":"object","properties":{"tool":{"type":"string"},"input":{"type":"object"}},"required":["tool"]}}},"required":["steps"]}),
            ),
            kernel_prelude: None,
        }
    }

    async fn reply_error(tool: &str, code: ExecuteToolErrorCode, message: &str) -> String {
        let state = CellState {
            input: EvalToolInput {
                language: EvalLanguage::Js,
                code: String::new(),
                summary: "hint".into(),
                action: None,
                timeout: None,
                on_timeout: None,
                reset: None,
                isolate: None,
            },
            runtime: None,
            started_at: 0.0,
            run_started_at: None,
            queued_behind: None,
            on_update: None,
            tool_calls: vec![],
            tool_call_metrics: vec![],
            status_events: vec![],
            active: true,
            output: String::new(),
            phase: None,
            error: None,
            duration_ms: 0.0,
            status: "running".into(),
        };
        let builder = CellResultBuilder::new(
            state,
            EvalOutputOptions {
                artifact_path: None,
                head_bytes: 20480,
                max_columns: 768,
                provider: None,
                api: None,
                image_sdk: Arc::new(Sdk),
            },
        );
        let (sender, mut replies) = tokio::sync::mpsc::unbounded_channel();
        let controller = maho_ai::utils::abort::AbortController::new();
        let mut handler = CellHandler::new(
            builder,
            CellBridgeRuntime {
                completion_handles: Arc::new(Default::default()),
                eval_handle_host: None,
                owner_session_id: None,
                executor: Arc::new(Failing(code, message.into())),
                tools: Some(Arc::new(|| Ok(vec![batch_tool()]))),
                complete: None,
                settings: Default::default(),
                signal: controller.signal(),
                deliver_reply: Arc::new(move |reply| {
                    sender.send(reply).expect("reply receiver");
                }),
            },
        );
        handler.handle(&json!({"type":"tool-call","toolName":tool,"callId":"call","args":{"app":"Safari"}})).await.expect("handled");
        let reply = replies.recv().await.expect("reply");
        assert_eq!(reply["ok"], false);
        reply["error"]["message"]
            .as_str()
            .expect("message")
            .to_owned()
    }

    const VALIDATION: &str = "Validation failed for tool \"mcp_computer_use_batch\":\n  - steps: must have required properties steps";

    #[tokio::test]
    async fn appends_the_tool_parameter_schema_resolved_from_the_live_catalog() {
        let message = reply_error(
            "mcp_computer_use_batch",
            ExecuteToolErrorCode::InvalidParams,
            VALIDATION,
        )
        .await;
        assert!(message.contains("must have required properties steps"));
        assert!(message.contains("Expected parameters:"));
        assert!(message.contains("required: steps"));
        assert!(message.contains("steps: array<object>"));
        assert!(message.contains("tool: string"));
    }

    #[tokio::test]
    async fn leaves_the_message_untouched_when_the_tool_is_not_in_the_catalog() {
        let message =
            reply_error("unlisted_tool", ExecuteToolErrorCode::InvalidParams, "bad").await;
        assert!(!message.contains("Expected parameters:"));
    }

    #[tokio::test]
    async fn delivers_a_blocked_calls_denial_without_the_parameter_schema() {
        let denial = "The user rejected permission to use this specific tool call.";
        let message = reply_error(
            "mcp_computer_use_batch",
            ExecuteToolErrorCode::Blocked,
            denial,
        )
        .await;
        assert_eq!(message, denial);
    }

    #[tokio::test]
    async fn still_appends_the_parameter_schema_to_an_invalid_arguments_failure() {
        let message = reply_error(
            "mcp_computer_use_batch",
            ExecuteToolErrorCode::InvalidParams,
            "steps: must have required properties steps",
        )
        .await;
        assert!(message.contains("must have required properties steps"));
        assert!(message.contains("Expected parameters:"));
    }
}

#[test]
fn schema_hint_long_tool_name_does_not_panic() {
    let tool_name = "a".repeat(1200);
    let rendered = render_schema_hint(&tool_name, &json!({"properties":{"k":{"type":"string"}}}));
    assert!(rendered.is_some());
    let rendered = rendered.unwrap();
    assert!(rendered.contains(&tool_name));
    assert!(rendered.contains("[truncated; call tool_schema("));
}
