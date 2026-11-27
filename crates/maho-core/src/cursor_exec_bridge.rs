//! Cursor exec-channel to local tool bridge.
//!
//! This is the Rust counterpart of senpi's `cursor-exec-bridge.ts`.  The
//! Cursor API owns conversion from `ToolResultMessage` to protobuf results;
//! bridge handlers therefore deliberately return `ToolResultMessage`s.

use std::collections::HashMap;
use std::sync::Arc;

use maho_agent::execution::prepared::executor_pipeline::PreparationSource;
use maho_agent::execution::prepared::memory_batch_store::open_memory_batch_store;
use maho_agent::execution::prepared::pipeline_prepared::create_prepared_tool_host;
use maho_agent::execution::tool_host::{
    AfterToolFreeze, ExecutionScope, ToolHostCall, ToolHostLocalContext, ToolHostOptions,
    WitnessRecorder,
};
use maho_agent::types::{
    AfterToolCall, AfterToolCallContext, AgentContext, AgentEvent, AgentMessage, AgentTool,
    AgentToolCall, BatchStoreFactory, BeforeToolCall, BeforeToolCallContext, BeforeToolCallResult,
    ToolExecutionMode,
};
use maho_ai::api::cursor_agent::r#gen::agent_pb::*;
use maho_ai::api::cursor_agent::types::{
    CursorExecHandlerResult, CursorExecHandlers, CursorMcpCall, CursorPiCall,
};
use maho_ai::types::{
    AssistantMessage, ContentBlock, Message, StopReason, ToolCall, ToolResultMessage, Usage,
};
use maho_ai::utils::abort::AbortSignal;
use serde_json::{Map, Value};

pub type ToolResolver = Arc<dyn Fn(&str) -> Option<AgentTool> + Send + Sync>;
pub type EventEmitter =
    Arc<dyn Fn(AgentEvent, AbortSignal) -> maho_ai::types::BoxFuture<'static, ()> + Send + Sync>;
pub type ResultEmitter = Arc<
    dyn Fn(
            String,
            String,
            Value,
            maho_agent::types::AgentToolResult,
            bool,
        ) -> maho_ai::types::BoxFuture<'static, ()>
        + Send
        + Sync,
>;
pub type Preflight = Arc<
    dyn Fn(String, String, Value) -> maho_ai::types::BoxFuture<'static, Option<String>>
        + Send
        + Sync,
>;
pub type SettleToolCall = Arc<dyn Fn(String) + Send + Sync>;
pub type ScopeGetter = Arc<dyn Fn() -> ExecutionScope + Send + Sync>;

pub struct CursorExecBridgeOptions {
    pub get_tool: ToolResolver,
    pub emit_event: EventEmitter,
    pub emit_tool_result: Option<ResultEmitter>,
    pub settle_tool_call: Option<SettleToolCall>,
    pub preflight_tool_call: Option<Preflight>,
    pub get_abort_signal: Arc<dyn Fn() -> Option<AbortSignal> + Send + Sync>,
    pub open_batch_store: Option<BatchStoreFactory>,
    pub get_execution_scope: Option<ScopeGetter>,
    pub record_witness: Option<WitnessRecorder>,
    pub after_tool_freeze: Option<AfterToolFreeze>,
}

const NO_ACTIVE_RUN: &str = "Tool execution has no active run";

fn error_result(id: String, name: String, message: impl Into<String>) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: id,
        tool_name: name,
        content: vec![ContentBlock::text(message)],
        details: None,
        usage: None,
        added_tool_names: None,
        nested_calls: None,
        is_error: true,
        timestamp: now_ms(),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn empty_result(id: String, name: String) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: id,
        tool_name: name,
        content: vec![ContentBlock::text("")],
        details: None,
        usage: None,
        added_tool_names: None,
        nested_calls: None,
        is_error: false,
        timestamp: now_ms(),
    }
}

fn map_cursor_pi_grep_args(args: &PiGrepExecArgs) -> Value {
    let mut map = Map::new();
    map.insert("pattern".into(), args.pattern.clone().into());
    if let Some(value) = &args.path {
        map.insert("path".into(), value.clone().into());
    }
    if let Some(value) = &args.glob {
        map.insert("glob".into(), value.clone().into());
    }
    if let Some(value) = args.ignore_case {
        map.insert("ignoreCase".into(), value.into());
    }
    if let Some(value) = args.literal {
        map.insert("literal".into(), value.into());
    }
    if let Some(value) = args.context {
        map.insert("context".into(), value.into());
    }
    if let Some(value) = args.limit {
        map.insert("limit".into(), value.into());
    }
    Value::Object(map)
}

#[allow(dead_code)]
fn empty_result_shape_test_helper() -> ToolResultMessage {
    empty_result("id".into(), "read".into())
}

fn issuing_message(tool_call: &AgentToolCall) -> AssistantMessage {
    AssistantMessage {
        content: vec![ContentBlock::ToolCall(ToolCall {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            arguments: tool_call.arguments.clone(),
            thought_signature: tool_call.thought_signature.clone(),
            error_message: None,
            incomplete: None,
            namespace: None,
        })],
        prompt_cache_breakpoint: None,
        prompt_cache_diagnostics: None,
        api: "cursor-agent".into(),
        provider: "cursor".into(),
        model: "cursor-exec".into(),
        response_model: None,
        response_id: None,
        thinking_level: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        stop_details: None,
        deferred: None,
        error_message: None,
        error_cause: None,
        provider_diagnostic: None,
        abort_source: None,
        failure_source: None,
        credential_served_recently: None,
        raw_stop_reason: None,
        end_turn: None,
        timing: None,
        timestamp: now_ms(),
    }
}

fn is_tool_result_message_event(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::MessageStart {
            message: AgentMessage::Llm(Message::ToolResult(_)),
            ..
        } | AgentEvent::MessageEnd {
            message: AgentMessage::Llm(Message::ToolResult(_)),
        }
    )
}

async fn execute_tool(
    options: &CursorExecBridgeOptions,
    name: &str,
    id: String,
    args: Value,
) -> ToolResultMessage {
    let Some(run_signal) = (options.get_abort_signal)() else {
        return error_result(id, name.to_owned(), NO_ACTIVE_RUN);
    };
    if run_signal.aborted() {
        return error_result(id, name.to_owned(), NO_ACTIVE_RUN);
    }
    let Some(tool) = (options.get_tool)(name) else {
        return error_result(
            id,
            name.to_owned(),
            format!("Tool \"{name}\" is not available in this session"),
        );
    };
    let args = if args.is_null() {
        Value::Object(Map::new())
    } else {
        args
    };
    let Some(args_object) = args.as_object() else {
        return error_result(id, name.to_owned(), "Tool arguments must be an object");
    };
    let clean_args = maho_ai::api::cursor_agent::pi_args::omit_undefined_args(args_object.clone());
    let tool_call = AgentToolCall {
        id: id.clone(),
        name: name.to_owned(),
        arguments: clean_args,
        ..Default::default()
    };

    let signal_check = Arc::clone(&options.get_abort_signal);
    let owned_signal = run_signal.clone();
    let orig_execute = Arc::clone(&tool.execute);
    let owned_tool = AgentTool {
        execute: Arc::new(move |call_id, params, sig, on_update| {
            let signal_check = Arc::clone(&signal_check);
            let owned_signal = owned_signal.clone();
            let orig_execute = Arc::clone(&orig_execute);
            Box::pin(async move {
                if owned_signal.aborted()
                    || !signal_check().is_some_and(|active| active.same_signal(&owned_signal))
                {
                    return maho_agent::types::AgentToolResult {
                        content: vec![ContentBlock::text(NO_ACTIVE_RUN)],
                        is_error: Some(true),
                        ..maho_agent::types::AgentToolResult::text(NO_ACTIVE_RUN)
                    };
                }
                orig_execute(call_id, params, sig, on_update).await
            })
        }),
        ..tool
    };

    let preflight_fn = options.preflight_tool_call.clone();
    let preflight_signal_check = Arc::clone(&options.get_abort_signal);
    let preflight_run_signal = run_signal.clone();
    let before_tool_call: BeforeToolCall =
        Arc::new(move |context: BeforeToolCallContext, _signal| {
            let preflight_fn = preflight_fn.clone();
            let preflight_signal_check = Arc::clone(&preflight_signal_check);
            let preflight_run_signal = preflight_run_signal.clone();
            Box::pin(async move {
                let blocked_reason = if let Some(preflight) = preflight_fn {
                    preflight(
                        context.tool_call.id.clone(),
                        context.tool_call.name.clone(),
                        context.args.clone(),
                    )
                    .await
                } else {
                    None
                };
                if preflight_run_signal.aborted()
                    || !preflight_signal_check()
                        .is_some_and(|active| active.same_signal(&preflight_run_signal))
                {
                    return Some(BeforeToolCallResult {
                        block: Some(true),
                        reason: Some(NO_ACTIVE_RUN.to_owned()),
                        terminate: None,
                        args: None,
                    });
                }
                blocked_reason.map(|reason| BeforeToolCallResult {
                    block: Some(true),
                    reason: Some(reason),
                    terminate: None,
                    args: None,
                })
            })
        });

    let emit_result_fn = options.emit_tool_result.clone();
    let after_tool_call: Option<AfterToolCall> = emit_result_fn.map(|emit| {
        let cb: AfterToolCall = Arc::new(move |context: AfterToolCallContext, _signal| {
            let emit = Arc::clone(&emit);
            let fut: maho_ai::types::BoxFuture<
                'static,
                Option<maho_agent::types::AfterToolCallResult>,
            > = Box::pin(async move {
                emit(
                    context.tool_call.id,
                    context.tool_call.name,
                    context.args,
                    context.result,
                    context.is_error,
                )
                .await;
                None
            });
            fut
        });
        cb
    });

    let source = PreparationSource {
        resolve_unknown_tool_call: None,
        before_tool_call: Some(before_tool_call),
        after_tool_call,
        removed_tool_hints: Some(HashMap::new()),
        after_tool_freeze: options
            .after_tool_freeze
            .clone()
            .unwrap_or_else(|| Arc::new(crate::execution::preflight_carry::carry_preflight_facts)),
    };
    let open_store = options
        .open_batch_store
        .clone()
        .unwrap_or_else(open_memory_batch_store);
    let host = create_prepared_tool_host(source, open_store);
    let mut scope = options
        .get_execution_scope
        .as_ref()
        .map_or_else(ExecutionScope::default, |getter| getter());
    if scope.id.is_empty() {
        scope.id = "cursor-exec".to_owned();
    }
    let emit_event = Arc::clone(&options.emit_event);
    let emit_signal = run_signal.clone();
    let assistant_message = issuing_message(&tool_call);
    let call_id = tool_call.id.clone();

    let host_result = host
        .execute(
            ToolHostCall {
                scope,
                calls: vec![tool_call],
                mode: ToolExecutionMode::Sequential,
            },
            ToolHostOptions {
                signal: Some(run_signal),
                emit: Arc::new(move |event| {
                    let emit_event = Arc::clone(&emit_event);
                    let emit_signal = emit_signal.clone();
                    Box::pin(async move {
                        if !is_tool_result_message_event(&event) {
                            emit_event(event, emit_signal).await;
                        }
                    })
                }),
                record_witness: options.record_witness.clone(),
                local: Some(ToolHostLocalContext {
                    context: AgentContext {
                        system_prompt: String::new(),
                        system_prompt_stable_prefix: None,
                        messages: vec![],
                        tools: Some(vec![owned_tool]),
                        declared_tools: None,
                    },
                    assistant_message,
                }),
            },
        )
        .await;

    if let Some(settle) = &options.settle_tool_call {
        settle(call_id);
    }

    match host_result {
        Ok(res) => res
            .messages
            .into_iter()
            .next()
            .unwrap_or_else(|| error_result(id, name.to_owned(), NO_ACTIVE_RUN)),
        Err(err) => error_result(id, name.to_owned(), err),
    }
}

/// `args.fileText ?? decode(args.fileBytes)`: mirrors the `write` display block the API
/// synthesizes, which falls back to the raw bytes when no text is present.
fn write_content(file_text: &str, file_bytes: &[u8]) -> String {
    if file_text.is_empty() {
        String::from_utf8_lossy(file_bytes).into_owned()
    } else {
        file_text.to_owned()
    }
}

/// `cursorPiLimit` over an `optional int32` frame field.
fn pi_limit_i32(limit: Option<i32>) -> Option<i32> {
    maho_ai::api::cursor_agent::pi_args::pi_limit(limit.map(f64::from)).map(|value| value as i32)
}

fn handler_result<T>(result: ToolResultMessage) -> CursorExecHandlerResult<T> {
    CursorExecHandlerResult::ToolResult(result)
}

fn object(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

pub fn create_cursor_exec_bridge(options: CursorExecBridgeOptions) -> CursorExecHandlers {
    let shared = Arc::new(options);
    macro_rules! handler {
        ($shared:expr, $ty:ty, $name:expr, $id:expr, $args:expr) => {{
            let shared = $shared.clone();
            let name: String = ($name).into();
            Box::pin(async move {
                handler_result::<$ty>(execute_tool(&shared, &name, $id, $args).await)
            })
        }};
    }
    CursorExecHandlers {
        read: Some(Arc::new({
            let shared = shared.clone();
            move |a: ReadArgs| {
                handler!(
                    &shared,
                    ReadResult,
                    "read",
                    a.tool_call_id,
                    object([
                        ("path", a.path.into()),
                        ("offset", a.offset.map(Value::from).unwrap_or(Value::Null)),
                        ("limit", a.limit.map(Value::from).unwrap_or(Value::Null))
                    ])
                )
            }
        })),
        ls: Some(Arc::new({
            let shared = shared.clone();
            move |a: LsArgs| {
                handler!(
                    &shared,
                    LsResult,
                    "ls",
                    a.tool_call_id,
                    object([(
                        "path",
                        maho_ai::api::cursor_agent::pi_args::pi_ls_path(Some(&a.path)).into()
                    )])
                )
            }
        })),
        grep: Some(Arc::new({
            let shared = shared.clone();
            move |a: GrepArgs| {
                handler!(
                    &shared,
                    GrepResult,
                    "grep",
                    a.tool_call_id,
                    object([
                        ("pattern", a.pattern.into()),
                        (
                            "path",
                            a.path
                                .filter(|path| !path.is_empty())
                                .map(Value::from)
                                .unwrap_or(Value::Null)
                        ),
                        (
                            "glob",
                            a.glob
                                .filter(|glob| !glob.is_empty())
                                .map(Value::from)
                                .unwrap_or(Value::Null)
                        ),
                        (
                            "ignoreCase",
                            (a.case_insensitive == Some(true))
                                .then_some(true)
                                .map(Value::from)
                                .unwrap_or(Value::Null)
                        )
                    ])
                )
            }
        })),
        write: Some(Arc::new({
            let shared = shared.clone();
            move |a: WriteArgs| {
                handler!(
                    &shared,
                    WriteResult,
                    "write",
                    a.tool_call_id,
                    object([
                        ("path", a.path.into()),
                        ("content", write_content(&a.file_text, &a.file_bytes).into())
                    ])
                )
            }
        })),
        delete: None,
        shell: Some(Arc::new({
            let shared = shared.clone();
            move |a: ShellArgs| {
                handler!(
                    &shared,
                    ShellResult,
                    "bash",
                    a.tool_call_id,
                    object([
                        (
                            "command",
                            maho_ai::api::cursor_agent::pi_args::compose_shell_command(
                                &a.command,
                                Some(&a.working_directory)
                            )
                            .into()
                        ),
                        (
                            "timeout",
                            (a.timeout > 0)
                                .then_some(a.timeout)
                                .map(Value::from)
                                .unwrap_or(Value::Null)
                        )
                    ])
                )
            }
        })),
        shell_stream: None,
        diagnostics: None,
        mcp: Some(Arc::new({
            let shared = shared.clone();
            move |a: CursorMcpCall| {
                let name = if a.tool_name.is_empty() {
                    a.name
                } else {
                    a.tool_name
                };
                handler!(
                    &shared,
                    McpResult,
                    name,
                    a.tool_call_id,
                    Value::Object(a.args)
                )
            }
        })),
        mcp_approval_preflight: None,
        pi_read: Some(Arc::new({
            let shared = shared.clone();
            move |a: CursorPiCall<PiReadExecArgs>| {
                let id = a.tool_call_id;
                let parsed = maho_ai::api::cursor_agent::pi_args::pi_read_args(
                    &a.args.path,
                    a.args.offset.map(|value| value as f64),
                    a.args.limit.map(|value| value as f64),
                );
                if let Some(parsed) = parsed {
                    handler!(
                        &shared,
                        PiReadExecResult,
                        "read",
                        id,
                        serde_json::json!({"path":parsed.path,"offset":parsed.offset,"limit":parsed.limit})
                    )
                } else {
                    Box::pin(async move {
                        handler_result::<PiReadExecResult>(empty_result(id, "read".into()))
                    })
                }
            }
        })),
        pi_bash: Some(Arc::new({
            let shared = shared.clone();
            move |a: CursorPiCall<PiBashExecArgs>| {
                handler!(
                    &shared,
                    PiBashExecResult,
                    "bash",
                    a.tool_call_id,
                    object([
                        ("command", a.args.command.into()),
                        (
                            "timeout",
                            a.args
                                .timeout
                                .filter(|v| *v >= 0.0)
                                .map(Value::from)
                                .unwrap_or(Value::Null)
                        )
                    ])
                )
            }
        })),
        pi_edit: Some(Arc::new({
            let shared = shared.clone();
            move |a: CursorPiCall<PiEditExecArgs>| {
                handler!(&shared, PiEditExecResult, "edit", a.tool_call_id, object([("path", a.args.path.into()), ("edits", serde_json::Value::Array(a.args.edits.into_iter().map(|e| serde_json::json!({"oldText":e.old_text,"newText":e.new_text})).collect()))]))
            }
        })),
        pi_write: Some(Arc::new({
            let shared = shared.clone();
            move |a: CursorPiCall<PiWriteExecArgs>| {
                handler!(
                    &shared,
                    PiWriteExecResult,
                    "write",
                    a.tool_call_id,
                    object([
                        ("path", a.args.path.into()),
                        ("content", a.args.content.into())
                    ])
                )
            }
        })),
        pi_grep: Some(Arc::new({
            let shared = shared.clone();
            move |a: CursorPiCall<PiGrepExecArgs>| {
                handler!(
                    &shared,
                    PiGrepExecResult,
                    "grep",
                    a.tool_call_id,
                    map_cursor_pi_grep_args(&PiGrepExecArgs {
                        ignore_case: (a.args.ignore_case == Some(true)).then_some(true),
                        literal: (a.args.literal == Some(true)).then_some(true),
                        limit: pi_limit_i32(a.args.limit),
                        ..a.args.clone()
                    })
                )
            }
        })),
        pi_find: Some(Arc::new({
            let shared = shared.clone();
            move |a: CursorPiCall<PiFindExecArgs>| {
                handler!(
                    &shared,
                    PiFindExecResult,
                    "find",
                    a.tool_call_id,
                    object([
                        ("pattern", a.args.pattern.into()),
                        (
                            "path",
                            a.args
                                .path
                                .filter(|path| !path.is_empty())
                                .map(Value::from)
                                .unwrap_or(Value::Null)
                        ),
                        (
                            "limit",
                            pi_limit_i32(a.args.limit)
                                .map(Value::from)
                                .unwrap_or(Value::Null)
                        )
                    ])
                )
            }
        })),
        pi_ls: Some(Arc::new({
            let shared = shared.clone();
            move |a: CursorPiCall<PiLsExecArgs>| {
                handler!(
                    &shared,
                    PiLsExecResult,
                    "ls",
                    a.tool_call_id,
                    object([
                        (
                            "path",
                            maho_ai::api::cursor_agent::pi_args::pi_ls_path(a.args.path.as_deref())
                                .into()
                        ),
                        (
                            "limit",
                            pi_limit_i32(a.args.limit)
                                .map(Value::from)
                                .unwrap_or(Value::Null)
                        )
                    ])
                )
            }
        })),
        on_tool_result: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn error_result_is_error() {
        assert!(error_result("id".into(), "read".into(), "bad").is_error);
    }

    #[test]
    fn error_result_preserves_call_identity_and_message() {
        let result = error_result("call-7".into(), "grep".into(), "not available");
        assert_eq!(result.tool_call_id, "call-7");
        assert_eq!(result.tool_name, "grep");
        assert!(
            matches!(&result.content[0], ContentBlock::Text(text) if text.text == "not available")
        );
    }

    #[test]
    fn map_grep_args_uses_camel_case_and_omits_absent_values() {
        let args = PiGrepExecArgs {
            pattern: "needle".into(),
            path: Some("src".into()),
            glob: None,
            ignore_case: Some(true),
            literal: Some(false),
            context: Some(2),
            limit: Some(10),
        };
        assert_eq!(
            map_cursor_pi_grep_args(&args),
            json!({"pattern":"needle","path":"src","ignoreCase":true,"literal":false,"context":2,"limit":10})
        );
    }

    #[test]
    fn empty_result_has_success_shape() {
        let result = empty_result("call".into(), "read".into());
        assert_eq!(result.tool_call_id, "call");
        assert_eq!(result.tool_name, "read");
        assert!(!result.is_error, "unexpected error: {:?}", result.content);
        assert!(matches!(&result.content[0], ContentBlock::Text(text) if text.text.is_empty()));
    }

    #[test]
    fn zero_limit_pi_read_returns_empty_success_without_dispatch() {
        assert!(
            maho_ai::api::cursor_agent::pi_args::pi_read_args("file", None, Some(0.0)).is_none()
        );
        let result = empty_result("read-zero".into(), "read".into());
        assert!(!result.is_error, "unexpected error: {:?}", result.content);
        assert_eq!(result.tool_call_id, "read-zero");
        assert!(matches!(&result.content[0], ContentBlock::Text(text) if text.text.is_empty()));
    }

    #[tokio::test]
    async fn bridge_dispatches_tool_and_preserves_result_callback_identity() {
        let controller = maho_ai::utils::abort::AbortController::new();
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let results = observed.clone();
        let tool = AgentTool {
            label: "read".into(),
            prepare_arguments: None,
            replay: None,
            execution_mode: None,
            tool: maho_ai::types::Tool {
                name: "read".into(),
                description: String::new(),
                parameters: json!({"type":"object"}),
                output_schema: None,
                freeform: None,
                constrained_sampling: None,
            },
            execute: Arc::new(|id, args, signal, _| {
                Box::pin(async move {
                    assert_eq!(id, "call-identity");
                    assert_eq!(args, json!({"path":"sample"}));
                    assert!(!signal.expect("active signal").aborted());
                    let mut result = maho_agent::types::AgentToolResult::text("file contents");
                    result.nested_calls = Some(maho_ai::types::NestedToolCalls {
                        calls: vec![maho_ai::types::NestedToolCallRecord {
                            id: "call-identity/1".into(),
                            name: "read".into(),
                            arguments: Some(Default::default()),
                            arguments_bytes: None,
                            status: maho_ai::types::NestedToolCallStatus::Ok,
                            duration_ms: Some(0),
                            error: None,
                        }],
                        complete: true,
                    });
                    result
                })
            }),
        };
        let handlers = create_cursor_exec_bridge(CursorExecBridgeOptions {
            get_tool: Arc::new(move |name| (name == "read").then(|| tool.clone())),
            emit_event: Arc::new(|_, _| Box::pin(async {})),
            emit_tool_result: Some(Arc::new(move |id, name, args, result, error| {
                let results = results.clone();
                Box::pin(async move {
                    results
                        .lock()
                        .expect("results")
                        .push((id, name, args, result, error));
                })
            })),
            settle_tool_call: None,
            preflight_tool_call: None,
            get_abort_signal: Arc::new(move || Some(controller.signal())),
            open_batch_store: None,
            get_execution_scope: None,
            record_witness: None,
            after_tool_freeze: None,
        });
        let result = handlers.read.expect("read handler")(ReadArgs {
            tool_call_id: "call-identity".into(),
            path: "sample".into(),
            offset: None,
            limit: None,
            encoding_hint: None,
        })
        .await;
        let CursorExecHandlerResult::ToolResult(result) = result else {
            panic!("expected tool result")
        };
        assert!(!result.is_error, "unexpected error: {:?}", result.content);
        let results = observed.lock().expect("results");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "call-identity");
        assert_eq!(results[0].1, "read");
        assert_eq!(results[0].2, json!({"path":"sample"}));
        assert_eq!(results[0].3.content, result.content);
        assert!(result.nested_calls.is_some());
        assert_eq!(results[0].3.nested_calls, result.nested_calls);
        assert!(!results[0].4);
    }

    #[test]
    fn write_content_falls_back_to_file_bytes_when_text_is_empty() {
        assert_eq!(write_content("", b"from bytes"), "from bytes");
        assert_eq!(write_content("from text", b"ignored"), "from text");
    }

    #[test]
    fn pi_limit_floors_present_limits_and_keeps_absent_unset() {
        assert_eq!(pi_limit_i32(Some(0)), Some(1));
        assert_eq!(pi_limit_i32(Some(-4)), Some(1));
        assert_eq!(pi_limit_i32(Some(25)), Some(25));
        assert_eq!(pi_limit_i32(None), None);
    }

    #[tokio::test]
    async fn bridge_rejects_schema_invalid_arguments_before_execution() {
        let controller = maho_ai::utils::abort::AbortController::new();
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = executed.clone();
        let tool = AgentTool {
            label: "grep".into(),
            prepare_arguments: None,
            replay: None,
            execution_mode: None,
            tool: maho_ai::types::Tool {
                name: "grep".into(),
                description: String::new(),
                parameters: json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}),
                output_schema: None,
                freeform: None,
                constrained_sampling: None,
            },
            execute: Arc::new(move |_, _, _, _| {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { maho_agent::types::AgentToolResult::text("ran") })
            }),
        };
        let options = CursorExecBridgeOptions {
            get_tool: Arc::new(move |name| (name == "grep").then(|| tool.clone())),
            emit_event: Arc::new(|_, _| Box::pin(async {})),
            emit_tool_result: None,
            settle_tool_call: None,
            preflight_tool_call: None,
            get_abort_signal: Arc::new(move || Some(controller.signal())),
            open_batch_store: None,
            get_execution_scope: None,
            record_witness: None,
            after_tool_freeze: None,
        };
        let result = execute_tool(&options, "grep", "bad-call".into(), json!({"path":"src"})).await;
        assert!(result.is_error);
        assert!(
            matches!(&result.content[0], ContentBlock::Text(text) if text.text.starts_with("Validation failed for tool \"grep\"")),
            "unexpected content: {:?}",
            result.content
        );
        assert!(!executed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn bridge_grep_drops_empty_path_and_glob_and_reports_prepared_params() {
        let controller = maho_ai::utils::abort::AbortController::new();
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let results = observed.clone();
        let tool = AgentTool {
            label: "grep".into(),
            replay: None,
            execution_mode: None,
            prepare_arguments: Some(Arc::new(|mut args: Value| {
                args["prepared"] = json!(true);
                Ok(args)
            })),
            tool: maho_ai::types::Tool {
                name: "grep".into(),
                description: String::new(),
                parameters: json!({"type":"object"}),
                output_schema: None,
                freeform: None,
                constrained_sampling: None,
            },
            execute: Arc::new(|_, args, _, _| {
                Box::pin(async move {
                    assert_eq!(args, json!({"pattern":"needle","prepared":true}));
                    maho_agent::types::AgentToolResult::text("")
                })
            }),
        };
        let handlers = create_cursor_exec_bridge(CursorExecBridgeOptions {
            get_tool: Arc::new(move |name| (name == "grep").then(|| tool.clone())),
            emit_event: Arc::new(|_, _| Box::pin(async {})),
            emit_tool_result: Some(Arc::new(move |_, _, args, _, _| {
                let results = results.clone();
                Box::pin(async move {
                    results.lock().expect("results").push(args);
                })
            })),
            settle_tool_call: None,
            preflight_tool_call: None,
            get_abort_signal: Arc::new(move || Some(controller.signal())),
            open_batch_store: None,
            get_execution_scope: None,
            record_witness: None,
            after_tool_freeze: None,
        });
        let args = GrepArgs {
            pattern: "needle".into(),
            path: Some(String::new()),
            glob: Some(String::new()),
            case_insensitive: Some(false),
            tool_call_id: "grep-empty".into(),
            ..Default::default()
        };
        let CursorExecHandlerResult::ToolResult(result) =
            handlers.grep.expect("grep handler")(args).await
        else {
            panic!("expected tool result")
        };
        assert!(!result.is_error, "unexpected error: {:?}", result.content);
        assert_eq!(
            *observed.lock().expect("results"),
            vec![json!({"pattern":"needle","prepared":true})]
        );
    }

    #[test]
    fn bridge_registers_each_supported_handler_kind() {
        let controller = maho_ai::utils::abort::AbortController::new();
        let options = CursorExecBridgeOptions {
            get_tool: Arc::new(|_| None),
            emit_event: Arc::new(|_, _| Box::pin(async {})),
            emit_tool_result: None,
            settle_tool_call: None,
            preflight_tool_call: None,
            get_abort_signal: Arc::new(move || Some(controller.signal())),
            open_batch_store: None,
            get_execution_scope: None,
            record_witness: None,
            after_tool_freeze: None,
        };
        let handlers = create_cursor_exec_bridge(options);
        assert!(
            handlers.read.is_some()
                && handlers.ls.is_some()
                && handlers.grep.is_some()
                && handlers.write.is_some()
        );
        assert!(handlers.shell.is_some() && handlers.mcp.is_some());
        assert!(
            handlers.pi_read.is_some() && handlers.pi_bash.is_some() && handlers.pi_edit.is_some()
        );
        assert!(
            handlers.pi_write.is_some()
                && handlers.pi_grep.is_some()
                && handlers.pi_find.is_some()
                && handlers.pi_ls.is_some()
        );
        assert!(
            handlers.delete.is_none()
                && handlers.diagnostics.is_none()
                && handlers.mcp_approval_preflight.is_none()
        );
    }
}
