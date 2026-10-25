use crate::definition::*;
use maho_agent::{AgentTool, AgentToolResult, ToolExecutionMode as AgentExecutionMode};
use maho_ai::types::{ContentBlock, ImageContent, TextAudience, TextContent};
use std::sync::Arc;
pub type ContextFactory = Arc<
    dyn Fn(&str, Option<&maho_ai::utils::abort::AbortSignal>) -> Arc<dyn ToolContext> + Send + Sync,
>;
fn into_agent_result(result: ToolResult) -> AgentToolResult {
    AgentToolResult {
        content: result
            .content
            .into_iter()
            .map(|part| match part {
                ToolContent::Text { text, audience } => ContentBlock::Text(TextContent {
                    text,
                    audience: audience.map(|_| TextAudience::Model),
                    text_signature: None,
                }),
                ToolContent::Image { data, mime_type } => {
                    ContentBlock::Image(ImageContent { data, mime_type })
                }
            })
            .collect(),
        terminate: result.details.as_ref().and_then(|details| details.get("terminate")).and_then(serde_json::Value::as_bool),
        details: result.details.unwrap_or(serde_json::Value::Null),
        structured_content: result.structured_content,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        is_error: result.is_error,
    }
}
fn from_agent_result(result: AgentToolResult) -> Result<ToolResult, ToolError> {
    let mut content = Vec::new();
    for part in result.content {
        match part {
            ContentBlock::Text(text) => content.push(ToolContent::Text {
                text: text.text,
                audience: text.audience.map(|_| "model".into()),
            }),
            ContentBlock::Image(image) => content.push(ToolContent::Image {
                data: image.data,
                mime_type: image.mime_type,
            }),
            ContentBlock::Thinking(_)
            | ContentBlock::ToolCall(_)
            | ContentBlock::ProviderNative(_) => {
                return Err(ToolError::Message(
                    "Tool results must contain text or image content".into(),
                ));
            }
        }
    }
    Ok(ToolResult {
        content,
        details: (!result.details.is_null()).then_some(result.details),
        structured_content: result.structured_content,
        is_error: result.is_error,
    })
}
pub fn wrap_tool_definition(
    definition: ToolDefinition,
    context_factory: Option<ContextFactory>,
) -> AgentTool {
    let prepare = definition.prepare_arguments.clone().map(|prepare| {
        Arc::new(move |args| prepare(args).map_err(|error| error.to_string()))
            as maho_agent::ToolArgumentShim
    });
    let execute = definition.execute;
    AgentTool {
        label: definition.label,
        prepare_arguments: prepare,
        replay: None,
        execution_mode: definition.execution_mode.map(|mode| match mode {
            ToolExecutionMode::Sequential => AgentExecutionMode::Sequential,
            ToolExecutionMode::Parallel => AgentExecutionMode::Parallel,
        }),
        tool: maho_ai::types::Tool {
            name: definition.name,
            description: definition.description,
            parameters: definition.parameters,
            output_schema: definition.output_schema,
            freeform: definition.freeform,
            constrained_sampling: definition.constrained_sampling,
        },
        execute: Arc::new(move |id, params, signal, on_update| {
            let execute = Arc::clone(&execute);
            let context = context_factory
                .as_ref()
                .map(|factory| factory(id.as_str(), signal.as_ref()));
            Box::pin(async move {
                let local_signal = AbortSignal::default();
                let listener = signal.as_ref().map(|signal| {
                    let local = local_signal.clone();
                    let listener = signal.add_abort_listener(move |_| local.abort());
                    if signal.aborted() {
                        local_signal.abort();
                    }
                    listener
                });
                let on_update = on_update.map(|update| {
                    Arc::new(move |result| {
                        update(into_agent_result(result));
                        Ok(())
                    }) as ToolUpdateCallback
                });
                let result = execute(ToolCall {
                    id: &id,
                    params,
                    signal: local_signal,
                    on_update,
                    context: context.as_deref(),
                })
                .await;
                if let (Some(signal), Some(listener)) = (signal, listener) {
                    signal.remove_abort_listener(listener);
                }
                match result {
                    Ok(result) => into_agent_result(result),
                    Err(error) => {
                        let mut result = AgentToolResult::text(error.to_string());
                        result.is_error = Some(true);
                        result
                    }
                }
            })
        }),
    }
}
pub fn wrap_tool_definitions(
    definitions: Vec<ToolDefinition>,
    context_factory: Option<ContextFactory>,
) -> Vec<AgentTool> {
    definitions
        .into_iter()
        .map(|definition| wrap_tool_definition(definition, context_factory.clone()))
        .collect()
}
pub fn create_tool_definition_from_agent_tool(tool: AgentTool) -> ToolDefinition {
    let execute = tool.execute;
    let shim = tool.prepare_arguments;
    let mut definition = ToolDefinition::new(
        &tool.tool.name,
        &tool.tool.description,
        tool.tool.parameters,
        Arc::new(move |call| {
            let execute = Arc::clone(&execute);
            Box::pin(async move {
                let controller = maho_ai::utils::abort::AbortController::new();
                if call.signal.is_aborted() {
                    controller.abort(None);
                }
                let update = call.on_update.map(|update| {
                    Arc::new(move |result| {
                        match from_agent_result(result).and_then(|result| update(result)) {
                            Ok(()) => {}
                            Err(error) => eprintln!("Tool update failed: {error}"),
                        }
                    }) as maho_agent::AgentToolUpdateCallback
                });
                let operation = execute(
                    call.id.into(),
                    call.params,
                    Some(controller.signal()),
                    update,
                );
                tokio::pin!(operation);
                tokio::select! {
                    result = &mut operation => from_agent_result(result),
                    () = call.signal.cancelled() => { controller.abort(None); from_agent_result(operation.await) },
                }
            })
        }),
    );
    definition.label = tool.label;
    definition.freeform = tool.tool.freeform;
    definition.output_schema = tool.tool.output_schema;
    definition.constrained_sampling = tool.tool.constrained_sampling;
    definition.prepare_arguments = shim.map(|shim| {
        Arc::new(move |args| shim(args).map_err(ToolError::Message)) as PrepareArguments
    });
    definition.execution_mode = tool.execution_mode.map(|mode| match mode {
        AgentExecutionMode::Sequential => ToolExecutionMode::Sequential,
        AgentExecutionMode::Parallel => ToolExecutionMode::Parallel,
    });
    definition
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Mutex;
    #[test]
    fn error_results_round_trip_without_losing_payloads() {
        // Given: an error-valued result carrying both display and structured output.
        let original = ToolResult {
            content: vec![
                ToolContent::text("failure"),
                ToolContent::Image {
                    data: "aW1hZ2U=".into(),
                    mime_type: "image/png".into(),
                },
            ],
            details: Some(serde_json::json!({"exit": 7})),
            structured_content: Some(serde_json::json!({"exit_code": 7, "output": "failure"})),
            is_error: Some(true),
        };
        // When: the result crosses both wrapper directions.
        let agent_result = into_agent_result(original.clone());
        assert_eq!(agent_result.is_error, Some(true));
        let restored = from_agent_result(agent_result).expect("error-valued result");
        // Then: error status, image, details and structured output survive together.
        assert_eq!(restored, original);
    }

    #[tokio::test]
    async fn preparation_failure_stops_execution_with_permissive_schema() {
        // Given: a permissive schema cannot reject the old fake error arguments.
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let capture = executed.clone();
        let mut definition = ToolDefinition::new(
            "test",
            "test",
            serde_json::json!({"type":"object"}),
            Arc::new(move |_| {
                capture.store(true, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(ToolResult::default()) })
            }),
        );
        definition.prepare_arguments = Some(Arc::new(|_| {
            Err(ToolError::Message("prepare-sentinel".into()))
        }));
        let tool = wrap_tool_definition(definition, None);
        // When: a real agent pipeline prepares and attempts to run the wrapped tool.
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            maho_agent::agent_loop::run_tool_call(
                maho_agent::AgentToolCall {
                    id: "failed-prepare".into(),
                    name: "test".into(),
                    ..Default::default()
                },
                maho_agent::agent_loop::RunToolCallOptions {
                    assistant_message: serde_json::from_value(serde_json::json!({
                        "content": [], "api": "openai-completions", "provider": "test", "model": "test",
                        "usage": maho_ai::types::Usage::default(), "stopReason": "toolUse", "timestamp": 0
                    })).expect("assistant fixture"),
                    context: Arc::new(maho_agent::AgentContext::default()),
                    tools: vec![tool],
                    before_tool_call: None,
                    after_tool_call: None,
                    signal: None,
                    on_update: None,
                },
            ),
        )
        .await
        .expect("bounded preparation");
        // Then: preparation is the failure; the executor is never entered.
        assert!(outcome.is_error);
        assert_eq!(
            maho_ai::utils::text::content_text(&outcome.result.content, ""),
            "prepare-sentinel"
        );
        assert!(!executed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn reverse_wrapper_preserves_preparation_failure() {
        // Given: a failing preparation hook on an agent tool.
        let mut definition = ToolDefinition::new(
            "test",
            "test",
            serde_json::json!({}),
            Arc::new(|_| Box::pin(async { Ok(ToolResult::default()) })),
        );
        definition.prepare_arguments = Some(Arc::new(|_| Err(ToolError::Aborted)));
        let restored =
            create_tool_definition_from_agent_tool(wrap_tool_definition(definition, None));
        // When: the reverse-wrapped hook prepares arguments.
        let result = restored.prepare_arguments.expect("preparation hook")(serde_json::json!({}));
        // Then: it returns failure rather than executable arguments.
        assert!(result.is_err());
    }

    struct Session;
    impl ToolSessionManager for Session {
        fn session_id(&self) -> &str {
            "session"
        }
        fn session_file(&self) -> Option<&Path> {
            None
        }
    }
    struct Context {
        session: Session,
    }
    impl ToolContext for Context {
        fn cwd(&self) -> &Path {
            Path::new(".")
        }
        fn model(&self) -> Option<&maho_ai::model::Model> {
            None
        }
        fn thinking_level(&self) -> Option<maho_ai::types::ThinkingLevel> {
            None
        }
        fn session_manager(&self) -> &dyn ToolSessionManager {
            &self.session
        }
        fn goal_store_file(&self) -> Option<&Path> {
            None
        }
    }
    #[test]
    fn wrapper_context_factory_receives_call_id_and_abort_signal() {
        let observed = Arc::new(Mutex::new(None));
        let capture = Arc::clone(&observed);
        let factory: ContextFactory = Arc::new(move |id, signal| {
            *capture.lock().expect("capture lock") = Some((id.to_owned(), signal.is_some()));
            Arc::new(Context { session: Session })
        });
        let definition = ToolDefinition::new(
            "test",
            "test",
            serde_json::json!({}),
            Arc::new(|_| Box::pin(async { Ok(ToolResult::default()) })),
        );
        let wrapped = wrap_tool_definition(definition, Some(factory));
        let runtime_signal = maho_ai::utils::abort::AbortController::new();
        let future = (wrapped.execute)(
            "call-1".into(),
            serde_json::json!({}),
            Some(runtime_signal.signal()),
            None,
        );
        std::mem::drop(future);
        assert_eq!(
            *observed.lock().expect("capture lock"),
            Some(("call-1".into(), true))
        );
    }
}
