//! Port of senpi `packages/agent/src/harness/execution/tools.ts`.

use std::sync::Arc;

use maho_ai::types::{ContentBlock, ToolCall, ToolResultMessage, Usage};
use maho_ai::utils::validation::{validate_tool_arguments, validate_tool_arguments_value};
use serde_json::{Map, Value};

use crate::harness::context::{Context, with_abort_signal};
use crate::harness::execution::effect_gate::{Gate, GateRefusal};
use crate::harness::session::types::JsonValue;
use crate::harness::types::{
    AgentHarnessTool, AgentHarnessToolInvocation, AgentHarnessToolUpdateCallback,
};
use crate::types::AgentToolResult;

pub struct PreparedToolCall<TContext> {
    pub tool_call: ToolCall,
    pub tool: Arc<AgentHarnessTool<TContext>>,
    pub args: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImmediateToolOutcome {
    pub tool_call: ToolCall,
    pub result: Box<AgentToolResult>,
    pub is_error: bool,
    pub terminate: bool,
}

impl ImmediateToolOutcome {
    pub const fn kind(&self) -> &'static str {
        "immediate"
    }
}

pub enum ToolCallPreparation<TContext> {
    Prepared(PreparedToolCall<TContext>),
    Immediate(ImmediateToolOutcome),
}

impl<TContext> ToolCallPreparation<TContext> {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Prepared(_) => "prepared",
            Self::Immediate(_) => "immediate",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BeforeToolDecision {
    pub args: Option<Map<String, Value>>,
    pub block: Option<BlockDecision>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockDecision {
    pub reason: String,
    pub terminate: Option<bool>,
}

pub struct ClearedToolCall<TContext> {
    pub tool_call: ToolCall,
    pub tool: Arc<AgentHarnessTool<TContext>>,
    pub args: Map<String, Value>,
}

pub enum ClearToolCallOutcome<TContext> {
    Cleared(ClearedToolCall<TContext>),
    Immediate(ImmediateToolOutcome),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutedToolCall {
    pub result: AgentToolResult,
    pub is_error: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AfterToolPatch {
    pub content: Option<Vec<ContentBlock>>,
    pub details: Option<JsonValue>,
    pub is_error: Option<bool>,
    pub usage: Option<Usage>,
    pub terminate: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinalizedToolCall {
    pub tool_call: ToolCall,
    pub result: AgentToolResult,
    pub is_error: bool,
    pub terminate: bool,
}

pub fn create_error_tool_result(message: &str) -> AgentToolResult {
    AgentToolResult {
        content: vec![ContentBlock::text(message)],
        details: Value::Null,
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: None,
    }
}

fn immediate_error(tool_call: ToolCall, message: &str, terminate: bool) -> ImmediateToolOutcome {
    ImmediateToolOutcome {
        tool_call,
        result: Box::new(create_error_tool_result(message)),
        is_error: true,
        terminate,
    }
}

pub fn prepare_tool_call<TContext>(
    call: ToolCall,
    tools: &[Arc<AgentHarnessTool<TContext>>],
) -> ToolCallPreparation<TContext> {
    let Some(tool) = tools
        .iter()
        .find(|candidate| candidate.name() == call.name)
        .cloned()
    else {
        return ToolCallPreparation::Immediate(immediate_error(
            call.clone(),
            &format!("Tool {} is unavailable", quote(&call.name)),
            false,
        ));
    };

    let prepared_arguments = match &tool.prepare_arguments {
        Some(prepare) => match prepare(Value::Object(call.arguments.clone())) {
            Ok(arguments) => arguments,
            Err(error) => {
                return ToolCallPreparation::Immediate(immediate_error(call, &error, false));
            }
        },
        None => Value::Object(call.arguments.clone()),
    };

    match validate_tool_arguments_value(&tool.tool, &call.name, &prepared_arguments) {
        Ok(args) => ToolCallPreparation::Prepared(PreparedToolCall {
            tool_call: call,
            tool,
            args: match args {
                Value::Object(map) => map,
                _ => Map::new(),
            },
        }),
        Err(error) => {
            ToolCallPreparation::Immediate(immediate_error(call, &error.to_string(), false))
        }
    }
}

pub fn apply_before_tool_decision<TContext>(
    prepared: PreparedToolCall<TContext>,
    decision: Option<BeforeToolDecision>,
) -> ClearToolCallOutcome<TContext> {
    if let Some(block) = decision
        .as_ref()
        .and_then(|decision| decision.block.clone())
    {
        return ClearToolCallOutcome::Immediate(immediate_error(
            prepared.tool_call,
            &block.reason,
            block.terminate.unwrap_or(false),
        ));
    }

    let Some(args) = decision.and_then(|decision| decision.args) else {
        return ClearToolCallOutcome::Cleared(ClearedToolCall {
            tool_call: prepared.tool_call,
            tool: prepared.tool,
            args: prepared.args,
        });
    };

    let replacement = ToolCall {
        arguments: args,
        ..prepared.tool_call.clone()
    };
    match validate_tool_arguments(&prepared.tool.tool, &replacement) {
        Ok(validated) => ClearToolCallOutcome::Cleared(ClearedToolCall {
            tool_call: prepared.tool_call,
            tool: prepared.tool,
            args: match validated {
                Value::Object(map) => map,
                _ => Map::new(),
            },
        }),
        Err(error) => ClearToolCallOutcome::Immediate(immediate_error(
            prepared.tool_call,
            &error.to_string(),
            false,
        )),
    }
}

pub async fn execute_tool_call<TContext: Clone>(
    call: ClearedToolCall<TContext>,
    gate: &Gate,
    on_update: AgentHarnessToolUpdateCallback,
    tool_context: TContext,
    invocation: Arc<dyn AgentHarnessToolInvocation>,
    context: Context,
) -> Result<ExecutedToolCall, GateRefusal> {
    gate.admit(|| ())?;
    let admitted_context = with_abort_signal(gate.signal(), &context);
    if let Some(signal) = admitted_context
        .abort_signal()
        .filter(|signal| signal.aborted())
    {
        return Err(GateRefusal::Closed(signal.reason().map_or_else(
            || "The operation was aborted".to_owned(),
            |reason| reason.message,
        )));
    }
    let accepting_updates = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let update_flag = accepting_updates.clone();
    let update_callback: AgentHarnessToolUpdateCallback = Arc::new(move |partial, options| {
        if update_flag.load(std::sync::atomic::Ordering::SeqCst) {
            on_update(partial, options);
        }
    });
    let execute = call.tool.execute.clone();
    let tool_call_id = call.tool_call.id.clone();
    let arguments = Value::Object(call.args.clone());
    let result = execute(
        tool_call_id,
        arguments,
        update_callback,
        tool_context,
        invocation,
        admitted_context,
    )
    .await;
    accepting_updates.store(false, std::sync::atomic::Ordering::SeqCst);
    Ok(match result {
        Ok(result) => ExecutedToolCall {
            result,
            is_error: false,
        },
        Err(error) => ExecutedToolCall {
            result: create_error_tool_result(&error),
            is_error: true,
        },
    })
}

pub fn finalize_tool_call<TContext>(
    call: &ClearedToolCall<TContext>,
    executed: ExecutedToolCall,
    patch: Option<AfterToolPatch>,
) -> FinalizedToolCall {
    let executed_is_error = executed.is_error;
    let is_error = patch
        .as_ref()
        .and_then(|patch| patch.is_error)
        .unwrap_or(executed_is_error);
    let result = match patch {
        Some(patch) => AgentToolResult {
            structured_content: if patch.content.is_some() {
                None
            } else {
                executed.result.structured_content
            },
            content: patch.content.unwrap_or(executed.result.content),
            details: patch.details.unwrap_or(executed.result.details),
            usage: patch.usage.or(executed.result.usage),
            nested_calls: executed.result.nested_calls,
            added_tool_names: executed.result.added_tool_names,
            terminate: patch.terminate.or(executed.result.terminate),
            is_error: executed.result.is_error,
        },
        None => executed.result,
    };
    FinalizedToolCall {
        tool_call: call.tool_call.clone(),
        is_error,
        terminate: result.terminate == Some(true),
        result,
    }
}

pub fn tool_result_from_message(message: &ToolResultMessage, terminate: bool) -> AgentToolResult {
    AgentToolResult {
        content: message.content.clone(),
        details: message.details.clone().unwrap_or(Value::Null),
        structured_content: None,
        usage: message.usage,
        nested_calls: message.nested_calls.clone(),
        added_tool_names: None,
        terminate: if terminate { Some(true) } else { None },
        is_error: None,
    }
}

pub fn create_tool_result_message(call: &FinalizedToolCall) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: call.tool_call.id.clone(),
        tool_name: call.tool_call.name.clone(),
        content: call.result.content.clone(),
        details: if call.result.details.is_null() {
            None
        } else {
            Some(call.result.details.clone())
        },
        usage: call.result.usage,
        added_tool_names: None,
        nested_calls: call.result.nested_calls.clone(),
        is_error: call.is_error,
        timestamp: now_ms(),
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

fn quote(name: &str) -> String {
    serde_json::to_string(name).unwrap_or_else(|_| format!("\"{name}\""))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(prepared: Option<Value>) -> Arc<AgentHarnessTool<()>> {
        Arc::new(AgentHarnessTool {
            label: "echo".into(),
            prepare_arguments: prepared.map(|value| {
                Arc::new(move |_| Ok(value.clone())) as crate::types::ToolArgumentShim
            }),
            execute: Arc::new(|_, _, _, _, _, _| {
                Box::pin(async { panic!("preparation must not execute tools") })
            }),
            replay: None,
            tool: serde_json::from_value(json!({
                "name": "echo",
                "description": "Echo tool",
                "parameters": {
                    "type": "object",
                    "properties": { "count": { "type": "number" } },
                    "required": ["count"]
                }
            }))
            .expect("tool"),
        })
    }

    fn call() -> ToolCall {
        serde_json::from_value(json!({
            "id": "tool-1", "name": "echo", "arguments": { "count": 1 }
        }))
        .expect("tool call")
    }

    #[test]
    fn rejects_non_object_prepared_arguments_instead_of_using_original_arguments() {
        for value in [
            Value::Null,
            json!(false),
            json!(7),
            json!("invalid"),
            json!([]),
        ] {
            let call = call();
            let tools = [tool(Some(value))];

            let outcome = prepare_tool_call(call.clone(), &tools);

            let ToolCallPreparation::Immediate(outcome) = outcome else {
                panic!("expected validation failure")
            };
            assert!(outcome.is_error);
            assert!(!outcome.terminate);
            assert_eq!(outcome.tool_call, call);
        }
    }

    #[test]
    fn validates_and_coerces_prepared_objects_while_retaining_original_call() {
        let call = call();
        let tools = [tool(Some(json!({ "count": "42" })))];

        let outcome = prepare_tool_call(call.clone(), &tools);

        let ToolCallPreparation::Prepared(outcome) = outcome else {
            panic!("expected prepared call")
        };
        assert_eq!(Value::Object(outcome.args), json!({ "count": 42 }));
        assert_eq!(outcome.tool_call, call);
    }

    #[test]
    fn validates_original_arguments_when_preparation_is_absent() {
        let call = call();
        let tools = [tool(None)];

        let outcome = prepare_tool_call(call.clone(), &tools);

        let ToolCallPreparation::Prepared(outcome) = outcome else {
            panic!("expected prepared call")
        };
        assert_eq!(outcome.args, call.arguments);
        assert_eq!(outcome.tool_call, call);
    }

    // Port of senpi packages/agent/test/harness/execution-tools.test.ts.
    mod execution_tools {
        use super::super::*;
        use crate::harness::context::BACKGROUND_CONTEXT;
        use crate::harness::execution::effect_gate::{Cancellation, create_gate};
        use serde_json::json;
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};

        type Execute = AgentHarnessToolExecute<()>;
        use crate::harness::types::AgentHarnessToolExecute;

        fn echo_execute() -> Execute {
            Arc::new(|_, args, _, _, _, _| {
                Box::pin(async move {
                    let value = args["value"].as_str().unwrap_or_default().to_owned();
                    Ok(AgentToolResult {
                        details: json!({ "value": value }),
                        ..AgentToolResult::text(value)
                    })
                })
            })
        }

        fn echo(
            prepare: Option<crate::types::ToolArgumentShim>,
            execute: Option<Execute>,
        ) -> Arc<AgentHarnessTool<()>> {
            Arc::new(AgentHarnessTool {
                label: "Echo".into(),
                prepare_arguments: prepare,
                execute: execute.unwrap_or_else(echo_execute),
                replay: None,
                tool: serde_json::from_value(json!({
                    "name": "echo",
                    "description": "Echo input",
                    "parameters": {
                        "type": "object",
                        "properties": { "value": { "type": "string" } },
                        "required": ["value"]
                    }
                }))
                .expect("tool"),
            })
        }

        fn call(arguments: Value) -> ToolCall {
            serde_json::from_value(
                json!({ "id": "call-1", "name": "echo", "arguments": arguments }),
            )
            .expect("tool call")
        }

        fn text(result: &AgentToolResult) -> String {
            result
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        fn prepared(tool: Arc<AgentHarnessTool<()>>) -> PreparedToolCall<()> {
            match prepare_tool_call(call(json!({ "value": "input" })), &[tool]) {
                ToolCallPreparation::Prepared(prepared) => prepared,
                ToolCallPreparation::Immediate(_) => panic!("expected prepared call"),
            }
        }

        fn cleared(tool: Arc<AgentHarnessTool<()>>) -> ClearedToolCall<()> {
            match apply_before_tool_decision(prepared(tool), None) {
                ClearToolCallOutcome::Cleared(cleared) => cleared,
                ClearToolCallOutcome::Immediate(_) => panic!("expected cleared call"),
            }
        }

        fn immediate(outcome: ToolCallPreparation<()>) -> ImmediateToolOutcome {
            match outcome {
                ToolCallPreparation::Immediate(outcome) => outcome,
                ToolCallPreparation::Prepared(_) => panic!("expected immediate outcome"),
            }
        }

        struct Invocation;
        impl AgentHarnessToolInvocation for Invocation {
            fn invocation_id(&self) -> &str {
                "result-1"
            }
            fn operation_id(&self) -> &str {
                "operation-1"
            }
            fn turn_id(&self) -> &str {
                "turn-1"
            }
            fn get_memo<'a>(
                &'a self,
                _: &'a str,
            ) -> maho_ai::types::BoxFuture<'a, Option<JsonValue>> {
                Box::pin(async { None })
            }
            fn set_memo<'a>(
                &'a self,
                _: &'a str,
                _: Option<JsonValue>,
            ) -> maho_ai::types::BoxFuture<'a, ()> {
                Box::pin(async {})
            }
        }

        fn ignore_updates() -> AgentHarnessToolUpdateCallback {
            Arc::new(|_, _| {})
        }

        #[test]
        fn prepares_arguments_before_validation_and_preserves_the_provider_call() {
            let provider_call = call(json!({ "legacy": "prepared" }));
            let prepare: crate::types::ToolArgumentShim =
                Arc::new(|args| Ok(json!({ "value": args["legacy"].clone() })));
            let ToolCallPreparation::Prepared(prepared) =
                prepare_tool_call(provider_call.clone(), &[echo(Some(prepare), None)])
            else {
                panic!("expected prepared call");
            };
            assert_eq!(prepared.tool_call, provider_call);
            assert_eq!(Value::Object(prepared.args), json!({ "value": "prepared" }));
            assert_eq!(
                Value::Object(provider_call.arguments),
                json!({ "legacy": "prepared" })
            );
        }

        #[test]
        fn returns_immediate_errors_for_unknown_tools_preparation_throws_and_invalid_arguments() {
            let unknown = immediate(prepare_tool_call(call(json!({ "value": "input" })), &[]));
            let failing: crate::types::ToolArgumentShim =
                Arc::new(|_| Err("cannot prepare".into()));
            let preparation_failure = immediate(prepare_tool_call(
                call(json!({ "value": "input" })),
                &[echo(Some(failing), None)],
            ));
            let invalid = immediate(prepare_tool_call(call(json!({})), &[echo(None, None)]));
            assert_eq!(text(&unknown.result), "Tool \"echo\" is unavailable");
            assert_eq!(text(&preparation_failure.result), "cannot prepare");
            assert!(text(&invalid.result).contains("Validation failed for tool \"echo\""));
        }

        #[test]
        fn blocks_calls_and_revalidates_replacement_arguments() {
            let blocked = apply_before_tool_decision(
                prepared(echo(None, None)),
                Some(BeforeToolDecision {
                    args: None,
                    block: Some(BlockDecision {
                        reason: "denied".into(),
                        terminate: Some(true),
                    }),
                }),
            );
            let ClearToolCallOutcome::Immediate(blocked) = blocked else {
                panic!("expected blocked call");
            };
            assert!(blocked.is_error && blocked.terminate);
            assert_eq!(text(&blocked.result), "denied");
            let replaced = apply_before_tool_decision(
                prepared(echo(None, None)),
                Some(BeforeToolDecision {
                    args: json!({ "value": "replacement" }).as_object().cloned(),
                    block: None,
                }),
            );
            let ClearToolCallOutcome::Cleared(replaced) = replaced else {
                panic!("expected replacement");
            };
            assert_eq!(
                Value::Object(replaced.args),
                json!({ "value": "replacement" })
            );
            let invalid = apply_before_tool_decision(
                prepared(echo(None, None)),
                Some(BeforeToolDecision {
                    args: Some(Map::new()),
                    block: None,
                }),
            );
            assert!(matches!(invalid, ClearToolCallOutcome::Immediate(_)));
        }

        #[tokio::test]
        async fn executes_with_updates_passes_the_signal_and_ignores_late_updates() {
            let (gate, _control) = create_gate();
            let gate_signal = gate.signal();
            let calls = Arc::new(AtomicUsize::new(0));
            let late: Arc<Mutex<Option<AgentHarnessToolUpdateCallback>>> =
                Arc::new(Mutex::new(None));
            let (counter, late_slot) = (calls.clone(), late.clone());
            let execute: Execute = Arc::new(move |_, args, on_update, _, _, context| {
                counter.fetch_add(1, Ordering::SeqCst);
                let signal = context.abort_signal().expect("admitted signal");
                assert!(signal.same_signal(&gate_signal));
                on_update(
                    AgentToolResult {
                        details: json!({ "value": args["value"] }),
                        ..AgentToolResult::text("partial")
                    },
                    None,
                );
                *late_slot.lock().expect("late") = Some(on_update);
                Box::pin(async { Ok(AgentToolResult::text("done")) })
            });
            let updates = Arc::new(Mutex::new(Vec::new()));
            let record = updates.clone();
            let result = execute_tool_call(
                cleared(echo(None, Some(execute))),
                &gate,
                Arc::new(move |update, _| record.lock().expect("updates").push(update)),
                (),
                Arc::new(Invocation),
                BACKGROUND_CONTEXT.clone(),
            )
            .await
            .expect("admitted");
            let late_update = late.lock().expect("late").take().expect("late callback");
            late_update(AgentToolResult::text("late"), None);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(!result.is_error);
            assert_eq!(text(&result.result), "done");
            let updates = updates.lock().expect("updates");
            assert_eq!(updates.len(), 1);
            assert_eq!(text(&updates[0]), "partial");
        }

        /// The TS sync/async throw pair collapses to one Rust case: a tool reports failure only
        /// through its `Err` result (a panic is not a supported tool failure channel).
        #[tokio::test]
        async fn converts_tool_throws_to_error_output() {
            let (gate, _control) = create_gate();
            let execute: Execute =
                Arc::new(|_, _, _, _, _, _| Box::pin(async { Err("tool failed".to_owned()) }));
            let result = execute_tool_call(
                cleared(echo(None, Some(execute))),
                &gate,
                ignore_updates(),
                (),
                Arc::new(Invocation),
                BACKGROUND_CONTEXT.clone(),
            )
            .await
            .expect("admitted");
            assert!(result.is_error);
            assert_eq!(text(&result.result), "tool failed");
        }

        #[tokio::test]
        async fn lets_abort_first_gate_refusal_escape_without_invoking_the_tool() {
            let calls = Arc::new(AtomicUsize::new(0));
            let counter = calls.clone();
            let execute: Execute = Arc::new(move |_, _, _, _, _, _| {
                counter.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(AgentToolResult::text("ran")) })
            });
            let (gate, control) = create_gate();
            control.begin_abort(Cancellation::new());
            let refusal = execute_tool_call(
                cleared(echo(None, Some(execute))),
                &gate,
                ignore_updates(),
                (),
                Arc::new(Invocation),
                BACKGROUND_CONTEXT.clone(),
            )
            .await
            .expect_err("abort-first refusal");
            assert!(matches!(refusal, GateRefusal::Abort(_)));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }

        fn usage(input: u64, total: u64) -> Usage {
            Usage {
                input,
                output: 2,
                total_tokens: total,
                ..Default::default()
            }
        }

        #[test]
        fn applies_patches_field_by_field_and_constructs_the_tool_result_message() {
            let call = cleared(echo(None, None));
            let executed = ExecutedToolCall {
                result: AgentToolResult {
                    details: json!({ "original": true }),
                    usage: Some(usage(1, 3)),
                    ..AgentToolResult::text("original")
                },
                is_error: true,
            };
            let finalized = finalize_tool_call(
                &call,
                executed,
                Some(AfterToolPatch {
                    content: Some(vec![ContentBlock::text("patched")]),
                    details: Some(json!({ "patched": true })),
                    usage: Some(usage(5, 7)),
                    is_error: Some(false),
                    terminate: Some(true),
                }),
            );
            let before = now_ms();
            let message = create_tool_result_message(&finalized);
            assert!(!finalized.is_error && finalized.terminate);
            assert_eq!(text(&finalized.result), "patched");
            assert_eq!(finalized.result.details, json!({ "patched": true }));
            assert_eq!(finalized.result.usage, Some(usage(5, 7)));
            assert_eq!(finalized.result.terminate, Some(true));
            assert_eq!(message.tool_call_id, "call-1");
            assert_eq!(message.tool_name, "echo");
            assert_eq!(message.content, vec![ContentBlock::text("patched")]);
            assert_eq!(message.details, Some(json!({ "patched": true })));
            assert_eq!(message.usage, Some(usage(5, 7)));
            assert!(!message.is_error);
            assert!(message.timestamp >= before);
        }

        #[test]
        fn preserves_unusual_json_object_keys_in_tool_result_details() {
            let call = cleared(echo(None, None));
            let details: Value =
                serde_json::from_str(r#"{"__proto__":{"preserved":true}}"#).expect("details");
            let finalized = finalize_tool_call(
                &call,
                ExecutedToolCall {
                    result: AgentToolResult {
                        content: Vec::new(),
                        details: details.clone(),
                        ..AgentToolResult::text("")
                    },
                    is_error: false,
                },
                None,
            );
            let message = create_tool_result_message(&finalized);
            assert!(
                message
                    .details
                    .as_ref()
                    .and_then(|d| d.get("__proto__"))
                    .is_some()
            );
            assert_eq!(message.details, Some(details));
        }

        /// `content: undefined` cannot be constructed in Rust; the closest untyped-tool shape is a
        /// JSON result without `content`, which must normalize to an empty list.
        #[test]
        fn normalizes_missing_content_from_untyped_tools() {
            let call = cleared(echo(None, None));
            let untyped: Result<AgentToolResult, _> =
                serde_json::from_value(json!({ "details": {} }));
            let result = untyped.unwrap_or_else(|_| AgentToolResult {
                content: Vec::new(),
                ..AgentToolResult::text("")
            });
            let finalized = finalize_tool_call(
                &call,
                ExecutedToolCall {
                    result,
                    is_error: false,
                },
                None,
            );
            assert_eq!(
                create_tool_result_message(&finalized).content,
                Vec::<ContentBlock>::new()
            );
        }
    }
}
