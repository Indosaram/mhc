//! Port of senpi `packages/agent/src/execution/remote-tool-host.ts`.

use std::collections::HashMap;
use std::sync::Arc;

use maho_ai::types::{BoxFuture, ContentBlock, Message, ToolResultMessage};
use maho_ai::utils::tool_pair_repair::TOOL_RESULT_PLACEHOLDER;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::assistant_terminal_state::now_ms;
use crate::transport::transport::Transport;
use crate::types::{AgentEvent, AgentToolCall, ToolExecutionMode};

use super::device_files::DeviceFiles;
use super::plane::PlaneHandler;
use super::remote_device_files::create_remote_device_files;
use super::remote_link::{HandLink, HandReply, create_hand_link};
use super::remote_protocol::{
    RemoteInvoke, RemoteTerminal, RemoteTerminalReason, RemoteToolResult,
};
use super::tool_host::{ExecutionScope, ToolHost, ToolHostCall, ToolHostOptions, ToolHostResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandPlacement {
    SplitLocal,
    CrossMachine,
}

pub type PromptSurfaceFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;

pub struct RemoteToolHostOptions {
    pub transport: Arc<dyn Transport>,
    pub placement: HandPlacement,
    pub session_ref: Option<String>,
    pub mint_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    pub planes: Option<HashMap<String, PlaneHandler>>,
    pub file_planes: Option<Vec<String>>,
    pub epoch: Option<u64>,
    pub prompt_surface: Option<PromptSurfaceFn>,
}

fn error_result(text: impl Into<String>) -> RemoteToolResult {
    RemoteToolResult {
        content: vec![ContentBlock::text(text)],
        details: Some(Value::Object(Default::default())),
        is_error: true,
        structured_content: None,
        terminate: None,
        added_tool_names: None,
    }
}

fn terminal_result(
    placement: HandPlacement,
    reason: RemoteTerminalReason,
    uncertain: bool,
    hooks_ran: bool,
) -> RemoteToolResult {
    if placement == HandPlacement::SplitLocal {
        return RemoteToolResult {
            content: vec![ContentBlock::text(TOOL_RESULT_PLACEHOLDER)],
            details: Some(Value::Object(Default::default())),
            is_error: false,
            structured_content: None,
            terminate: None,
            added_tool_names: None,
        };
    }
    let effects = if uncertain {
        "Its effects are uncertain."
    } else {
        "It had no effect."
    };
    RemoteToolResult {
        content: vec![ContentBlock::text(format!(
            "The tool call ended without a result ({}) and was not run again. {effects}",
            reason.as_str()
        ))],
        details: Some(json!({
            "outcome": reason.as_str(),
            "uncertain": uncertain,
            "hooksRan": hooks_ran,
        })),
        is_error: true,
        structured_content: None,
        terminate: None,
        added_tool_names: None,
    }
}

fn result_of(placement: HandPlacement, reply: HandReply) -> RemoteToolResult {
    match reply {
        HandReply::Settled { result } => result,
        HandReply::Terminal {
            terminal:
                RemoteTerminal {
                    reason,
                    uncertain,
                    hooks_ran,
                    ..
                },
        } => terminal_result(placement, reason, uncertain, hooks_ran),
        HandReply::Refused { reason } => {
            error_result(format!("The hand refused this tool call: {reason}"))
        }
        HandReply::Aborted { sent } => error_result(if sent {
            "Tool execution aborted"
        } else {
            "Operation aborted"
        }),
        HandReply::Unreachable { reason } => {
            error_result(format!("The hand is unreachable: {reason}"))
        }
        HandReply::Closed { sent } => {
            if sent {
                terminal_result(placement, RemoteTerminalReason::HandRestarted, true, true)
            } else {
                terminal_result(placement, RemoteTerminalReason::Cancelled, false, false)
            }
        }
        HandReply::EpochRefused { reason } => {
            error_result(format!("The hand refused this session's epoch: {reason}"))
        }
    }
}

pub struct RemoteToolHost {
    link: Arc<HandLink>,
    placement: HandPlacement,
    session_ref: Option<String>,
    mint_id: Arc<dyn Fn() -> String + Send + Sync>,
    prompt_surface: Option<PromptSurfaceFn>,
}

impl RemoteToolHost {
    pub fn close(&self) {
        self.link.close();
    }
}

impl ToolHost for RemoteToolHost {
    fn execute(
        &self,
        call: ToolHostCall,
        host_options: ToolHostOptions,
    ) -> BoxFuture<'_, Result<ToolHostResult, String>> {
        let link = Arc::clone(&self.link);
        let placement = self.placement;
        let session_ref = self.session_ref.clone();
        let mint_id = Arc::clone(&self.mint_id);
        let prompt_surface_fn = self.prompt_surface.clone();

        Box::pin(async move {
            let ToolHostOptions { signal, emit, .. } = host_options;
            let batch_id = mint_id();
            let prompt_surface = prompt_surface_fn.as_ref().and_then(|f| f());
            let mut futures: Vec<BoxFuture<'static, RemoteToolResult>> =
                Vec::with_capacity(call.calls.len());

            let invoke_of = |raw: &AgentToolCall, index: usize| RemoteInvoke {
                epoch: None,
                execution_id: mint_id(),
                batch_id: batch_id.clone(),
                index,
                scope: call.scope.clone(),
                tool_call_id: raw.id.clone(),
                name: raw.name.clone(),
                arguments: Value::Object(raw.arguments.clone()),
                session_ref: session_ref.clone(),
                prompt_surface: prompt_surface.clone(),
            };

            let finish = |raw: AgentToolCall, result: RemoteToolResult| {
                let emit = Arc::clone(&emit);
                async move {
                    let result_val = serde_json::to_value(&result).unwrap_or(Value::Null);
                    emit(AgentEvent::ToolExecutionEnd {
                        parent_tool_call_id: None,
                        tool_call_id: raw.id,
                        tool_name: raw.name,
                        result: result_val,
                        is_error: result.is_error,
                    })
                    .await;
                    result
                }
            };

            let mut results: Vec<RemoteToolResult> = Vec::with_capacity(call.calls.len());
            if call.mode == ToolExecutionMode::Sequential {
                for (index, raw) in call.calls.iter().enumerate() {
                    emit(AgentEvent::ToolExecutionStart {
                        parent_tool_call_id: None,
                        tool_call_id: raw.id.clone(),
                        tool_name: raw.name.clone(),
                        args: Value::Object(raw.arguments.clone()),
                    })
                    .await;
                    let reply = link
                        .dispatch_invoke(invoke_of(raw, index), signal.clone())
                        .await;
                    let res = finish(raw.clone(), result_of(placement, reply)).await;
                    results.push(res);
                    if signal.as_ref().is_some_and(AbortSignal::aborted) {
                        break;
                    }
                }
            } else {
                for (index, raw) in call.calls.iter().enumerate() {
                    emit(AgentEvent::ToolExecutionStart {
                        parent_tool_call_id: None,
                        tool_call_id: raw.id.clone(),
                        tool_name: raw.name.clone(),
                        args: Value::Object(raw.arguments.clone()),
                    })
                    .await;
                    let dispatch_fut = link.dispatch_invoke(invoke_of(raw, index), signal.clone());
                    let raw_owned = raw.clone();
                    let emit_clone = Arc::clone(&emit);
                    futures.push(Box::pin(async move {
                        let reply = dispatch_fut.await;
                        let result = result_of(placement, reply);
                        let result_val = serde_json::to_value(&result).unwrap_or(Value::Null);
                        emit_clone(AgentEvent::ToolExecutionEnd {
                            parent_tool_call_id: None,
                            tool_call_id: raw_owned.id,
                            tool_name: raw_owned.name,
                            result: result_val,
                            is_error: result.is_error,
                        })
                        .await;
                        result
                    }));
                }
                results = futures::future::join_all(futures).await;
            }

            let mut messages = Vec::with_capacity(results.len());
            for (index, result) in results.iter().enumerate() {
                let Some(raw) = call.calls.get(index) else {
                    continue;
                };
                let message = ToolResultMessage {
                    tool_call_id: raw.id.clone(),
                    tool_name: raw.name.clone(),
                    content: result.content.clone(),
                    details: result.details.clone(),
                    usage: None,
                    nested_calls: None,
                    added_tool_names: result
                        .added_tool_names
                        .as_ref()
                        .filter(|n| !n.is_empty())
                        .cloned(),
                    is_error: result.is_error,
                    timestamp: now_ms(),
                };
                let agent_msg = AgentMessage::Llm(Message::ToolResult(message.clone()));
                emit(AgentEvent::MessageStart {
                    message: agent_msg.clone(),
                    streaming: None,
                })
                .await;
                emit(AgentEvent::MessageEnd { message: agent_msg }).await;
                messages.push(message);
            }

            let terminate =
                !results.is_empty() && results.iter().all(|r| r.terminate == Some(true));
            Ok(ToolHostResult {
                messages,
                terminate,
            })
        })
    }

    fn device_files(&self, scope: &ExecutionScope) -> Option<Arc<dyn DeviceFiles>> {
        Some(create_remote_device_files(
            Arc::clone(&self.link),
            scope.clone(),
            Arc::clone(&self.mint_id),
        ))
    }
}

use crate::types::AgentMessage;
use maho_ai::utils::abort::AbortSignal;

pub fn create_remote_tool_host(options: RemoteToolHostOptions) -> Arc<RemoteToolHost> {
    let mint_id = options
        .mint_id
        .unwrap_or_else(|| Arc::new(|| Uuid::new_v4().to_string()));
    let link = create_hand_link(
        options.transport,
        options.planes.unwrap_or_default(),
        options.file_planes.unwrap_or_default(),
        options.epoch,
    );
    Arc::new(RemoteToolHost {
        link,
        placement: options.placement,
        session_ref: options.session_ref,
        mint_id,
        prompt_surface: options.prompt_surface,
    })
}
