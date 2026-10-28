//! Native senpi `computer` tool implementation matching upstream `src/tool.ts`.

use maho_tools::definition::AbortSignal;
use maho_tools::definition::{ToolCall, ToolContent, ToolDefinition, ToolError, ToolResult};
use senpi_desktop_protocol::{
    AuditRecord, ComputerCallStep, ComputerDisplay, ComputerScreenshot, is_read_only_computer_call,
};
use senpi_desktop_run::context::DesktopRunError;
use senpi_desktop_run::runtime::{
    ComputerRunHost, ComputerRunRequest, ExecuteTool, run_computer_code,
};
use senpi_desktop_service::rpc_client::DesktopError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::activation::ComputerHandle;
use crate::cua_errors::{computer_failure, launch_context_failure};
use crate::params::{CallStep, ComputerToolParams, DEFAULT_TIMEOUT_SECONDS, parse_computer_params};
use crate::session::{ComputerHostContext, run_snapshot};
use crate::tool_definition::computer_tool_definition;

#[derive(Clone)]
pub struct ComputerToolDeps {
    pub handle: Arc<ComputerHandle>,
    pub execute_tool: Option<ExecuteTool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshots: Option<Vec<ComputerScreenshot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit: Option<Vec<AuditRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComputerToolResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    pub content: Vec<ComputerDisplay>,
    pub details: ComputerToolDetails,
}

impl ComputerToolResult {
    pub fn to_tool_result(self) -> ToolResult {
        let details = serde_json::to_value(&self.details).ok();
        let content = self
            .content
            .into_iter()
            .map(|disp| match disp {
                ComputerDisplay::Text { text } => ToolContent::Text {
                    text,
                    audience: None,
                },
                ComputerDisplay::Image { data, mime_type } => {
                    ToolContent::Image { data, mime_type }
                }
            })
            .collect();
        ToolResult {
            content,
            details,
            structured_content: None,
            is_error: None,
        }
    }
}

pub fn render_call_step(step: &CallStep) -> String {
    let args_str = match &step.args {
        Some(a) => serde_json::to_string(a).unwrap_or_else(|_| "[]".to_string()),
        None => "[]".to_string(),
    };
    format!("{}(...{})", step.method, args_str)
}

pub fn render_call_chain(chain: &[CallStep]) -> Result<String, String> {
    if chain.is_empty() {
        return Err("renderCallChain: empty chain".to_string());
    }
    let root = &chain[0];
    if chain.len() == 1 {
        Ok(format!("return await desktop.{};", render_call_step(root)))
    } else {
        let hop = &chain[1];
        Ok(format!(
            "return await (await desktop.{}).{};",
            render_call_step(root),
            render_call_step(hop)
        ))
    }
}

fn value_result(value: Option<Value>, fallback: &str) -> ComputerToolResult {
    let text = match &value {
        None => fallback.to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(v) => serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()),
    };
    ComputerToolResult {
        is_error: None,
        content: vec![ComputerDisplay::Text { text }],
        details: ComputerToolDetails {
            value,
            read_only: None,
            screenshots: None,
            audit: None,
            is_error: None,
        },
    }
}

pub async fn run_computer(
    deps: &ComputerToolDeps,
    context: &ComputerHostContext,
    code: String,
    read_only: bool,
    timeout_seconds: f64,
    signal: Option<AbortSignal>,
) -> Result<ComputerToolResult, DesktopRunError> {
    deps.handle.activate(context).await.map_err(DesktopRunError::Engine)?;

    let snapshot = run_snapshot(&deps.handle.settings(), context, read_only);
    let timeout_ms = std::time::Duration::from_secs_f64(timeout_seconds).as_millis() as u64;

    let controller = maho_ai::utils::abort::AbortController::new();
    if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
        controller.abort(None);
    }
    let req = ComputerRunRequest {
        code,
        snapshot,
        timeout_ms,
        signal: Some(controller.signal()),
    };
    let host = ComputerRunHost {
        service: deps.handle.service(),
        execute_tool: deps.execute_tool.clone(),
        kernel: None,
    };

    let operation = run_computer_code(req, host);
    tokio::pin!(operation);
    let outcome = if let Some(signal) = signal {
        tokio::select! {
            result=&mut operation=>result?,
            ()=signal.cancelled()=>{controller.abort(None);operation.await?}
        }
    } else {
        operation.await?
    };

    let mut content = outcome.displays;
    if let Some(value) = &outcome.return_value {
        let text = match value {
            Value::String(text) => text.clone(),
            value => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        };
        content.push(ComputerDisplay::Text { text });
    }
    if content.is_empty() {
        content.push(ComputerDisplay::Text {
            text: "Done.".to_string(),
        });
    }

    Ok(ComputerToolResult {
        is_error: None,
        content,
        details: ComputerToolDetails {
            value: outcome.return_value,
            read_only: Some(read_only),
            screenshots: Some(outcome.screenshots),
            audit: Some(outcome.audit),
            is_error: None,
        },
    })
}

pub fn create_computer_tool(deps: ComputerToolDeps) -> ToolDefinition {
    let mut def = computer_tool_definition();
    let deps_for_exec = deps.clone();

    def.execute = Arc::new(move |call: ToolCall| {
        let deps = deps_for_exec.clone();
        Box::pin(async move {
            let input = &call.params;
            let signal = Some(call.signal.clone());

            let context = match call.context {
                Some(ctx) => ComputerHostContext::from_tool_context(ctx),
                None => {
                    return Err(ToolError::Message(
                        "Missing required ToolContext for computer tool".into(),
                    ));
                }
            };

            let params = match parse_computer_params(input) {
                Ok(p) => p,
                Err(e) => {
                    return Err(ToolError::Message(e.to_string()));
                }
            };

            let handle = deps.handle.clone();
            let res = match params {
                ComputerToolParams::Call { chain } => {
                    let steps: Vec<ComputerCallStep> = chain
                        .iter()
                        .map(|s| ComputerCallStep {
                            method: s.method.clone(),
                            args: s.args.clone(),
                        })
                        .collect();
                    let read_only = match is_read_only_computer_call(&steps) {
                        Ok(ro) => ro,
                        Err(e) => return Err(ToolError::Message(e.to_string())),
                    };
                    let code = match render_call_chain(&chain) {
                        Ok(c) => c,
                        Err(e) => return Err(ToolError::Message(e)),
                    };
                    run_with_permission_catch(
                        run_computer(
                            &deps,
                            &context,
                            code,
                            read_only,
                            DEFAULT_TIMEOUT_SECONDS as f64,
                            signal,
                        ),
                        &handle,
                    )
                    .await
                }
                ComputerToolParams::Run {
                    code,
                    read_only,
                    timeout,
                } => {
                    let timeout_seconds = timeout.unwrap_or(DEFAULT_TIMEOUT_SECONDS as f64);
                    let is_ro = read_only.unwrap_or(false);
                    run_with_permission_catch(
                        run_computer(&deps, &context, code, is_ro, timeout_seconds, signal),
                        &handle,
                    )
                    .await
                }
                ComputerToolParams::Capabilities => {
                    // Like call/run, activation and capability-query permission failures are
                    // structured tool results rather than thrown tool errors.
                    let capabilities = async {
                        handle
                            .activate(&context)
                            .await
                            .map_err(DesktopRunError::Engine)?;
                        match handle.capabilities().await {
                            Ok(Some(caps)) => Ok(value_result(
                                serde_json::to_value(caps).ok(),
                                "capabilities unavailable",
                            )),
                            Ok(None) => Ok(value_result(None, "capabilities unavailable")),
                            Err(error) => Err(DesktopRunError::Engine(error)),
                        }
                    };
                    run_with_permission_catch(capabilities, &handle).await
                }
                ComputerToolParams::Close => {
                    handle
                        .close()
                        .await
                        .map_err(|error| ToolError::Message(error.to_string()))?;
                    Ok(value_result(None, "Closed the desktop session."))
                }
            };

            match res {
                Ok(ctr) => Ok(ctr.to_tool_result()),
                Err(err) => Err(ToolError::Message(err.to_string())),
            }
        })
    });

    def
}

async fn run_with_permission_catch(
    fut: impl std::future::Future<Output = Result<ComputerToolResult, DesktopRunError>>,
    handle: &ComputerHandle,
) -> Result<ComputerToolResult, DesktopRunError> {
    match fut.await {
        Ok(res) => Ok(res),
        Err(err) => {
            if let DesktopRunError::Engine(DesktopError::LaunchContext(launch)) = &err {
                let failure = launch_context_failure(launch);
                let json_val = serde_json::to_value(&failure).unwrap_or(Value::Null);
                return Ok(ComputerToolResult {
                    is_error: Some(true),
                    content: vec![ComputerDisplay::Text {
                        text: launch.message.clone(),
                    }],
                    details: ComputerToolDetails {
                        value: Some(json_val),
                        read_only: None,
                        screenshots: None,
                        audit: None,
                        is_error: Some(true),
                    },
                });
            }
            if let DesktopRunError::Engine(DesktopError::Rpc(rpc)) = &err
                && let Some(senpi_desktop_protocol::JsonRpcErrorData::Engine(data)) = &rpc.data
                && data.code == senpi_desktop_protocol::ErrorCode::PermissionDenied
            {
                let failure = computer_failure(
                    data.code.as_str(),
                    &rpc.message,
                    &handle.settings().stop_hotkey,
                    data.permission.as_ref(),
                );
                let json_val = serde_json::to_value(&failure).unwrap_or(Value::Null);
                let text = serde_json::to_string_pretty(&failure).unwrap_or_default();
                return Ok(ComputerToolResult {
                    is_error: Some(true),
                    content: vec![ComputerDisplay::Text { text }],
                    details: ComputerToolDetails {
                        value: Some(json_val),
                        read_only: None,
                        screenshots: None,
                        audit: None,
                        is_error: Some(true),
                    },
                });
            }
            Err(err)
        }
    }
}
