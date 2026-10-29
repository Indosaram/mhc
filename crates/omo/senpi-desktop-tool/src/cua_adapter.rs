//! OpenAI CUA adapter tool implementation matching upstream `src/cua-adapter.ts`.

use std::sync::Arc;
use tokio::sync::Mutex;

use maho_tools::definition::{ToolCall, ToolContent, ToolDefinition, ToolError, ToolResult};
use senpi_desktop_protocol::{ComputerDisplay, PermissionDeniedData};
use senpi_desktop_run::context::DesktopRunError;
use senpi_desktop_service::rpc_client::DesktopError;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::cua_actions::{ScreenshotBounds, actions_of, actions_script, parse_computer_actions};
use crate::cua_definition::computer_actions_tool_definition;
use crate::cua_errors::{ComputerFailure, computer_failure, launch_context_failure};
use crate::params::{DEFAULT_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS};
use crate::session::ComputerHostContext;
use crate::tool::{ComputerToolDeps, run_computer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepOutcome {
    pub index: usize,
    pub action: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<PermissionDeniedData>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionsOutcome {
    pub steps: Vec<StepOutcome>,
    pub bounds: Option<ScreenshotBounds>,
    pub failed_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<ComputerFailure>,
}

pub fn create_computer_actions_tool(deps: ComputerToolDeps) -> ToolDefinition {
    let mut def = computer_actions_tool_definition();
    let bounds_holder: Arc<Mutex<Option<ScreenshotBounds>>> = Arc::new(Mutex::new(None));
    let deps_for_exec = deps.clone();

    def.execute = Arc::new(move |call: ToolCall| {
        let deps = deps_for_exec.clone();
        let bounds_mutex = bounds_holder.clone();

        Box::pin(async move {
            let input = &call.params;
            let signal = Some(call.signal.clone());
            let stop_hotkey = deps.handle.settings().stop_hotkey;

            let context = match call.context {
                Some(ctx) => ComputerHostContext::from_tool_context(ctx),
                None => {
                    return Err(ToolError::Message(
                        "Missing required ToolContext for computer actions".into(),
                    ));
                }
            };

            let params = match parse_computer_actions(input) {
                Ok(p) => p,
                Err(e) => {
                    let failure = computer_failure(&e.code, &e.reason, &stop_hotkey, None);
                    return Ok(ToolResult {
                        content: vec![ToolContent::text(&failure.message)],
                        details: Some(json!({ "value": failure, "isError": true })),
                        structured_content: None,
                        is_error: None,
                    });
                }
            };

            let timeout_seconds = params
                .timeout()
                .map(|t| t.clamp(1.0, MAX_TIMEOUT_SECONDS as f64))
                .unwrap_or(DEFAULT_TIMEOUT_SECONDS as f64);
            let read_only = crate::cua_actions::is_read_only_actions(input);

            let current_bounds = bounds_mutex.lock().await.clone();
            let code = actions_script(&actions_of(&params), current_bounds.as_ref());

            let result =
                match run_computer(&deps, &context, code, read_only, timeout_seconds, signal).await
                {
                    Ok(r) => r,
                    Err(err) => {
                        if let DesktopRunError::Engine(DesktopError::LaunchContext(launch)) = &err {
                            let failure = launch_context_failure(launch);
                            return Ok(ToolResult {
                                content: vec![ToolContent::text(&failure.message)],
                                details: Some(json!({ "value": failure, "isError": true })),
                                structured_content: None,
                                is_error: None,
                            });
                        }
                        let (reason, message, perm) = match &err {
                            DesktopRunError::Engine(DesktopError::Rpc(rpc)) => match &rpc.data {
                                Some(senpi_desktop_protocol::JsonRpcErrorData::Engine(data)) => (
                                    data.code.as_str().to_string(),
                                    rpc.message.clone(),
                                    data.permission.clone(),
                                ),
                                _ => ("Error".to_string(), rpc.message.clone(), None),
                            },
                            DesktopRunError::Engine(other) => {
                                ("Error".to_string(), other.to_string(), None)
                            }
                            DesktopRunError::ComputerRun(c) => {
                                (c.reason.as_str().to_string(), c.message.clone(), None)
                            }
                        };
                        let failure =
                            computer_failure(&reason, &message, &stop_hotkey, perm.as_ref());
                        return Ok(ToolResult {
                            content: vec![ToolContent::text(&failure.message)],
                            details: Some(json!({ "value": failure, "isError": true })),
                            structured_content: None,
                            is_error: None,
                        });
                    }
                };

            let Some(raw_value) = result.details.value.clone() else {
                return Ok(result.to_tool_result());
            };

            let outcome: ActionsOutcome = match serde_json::from_value(raw_value.clone()) {
                Ok(o) => o,
                Err(_) => return Ok(result.to_tool_result()),
            };

            *bounds_mutex.lock().await = outcome.bounds.clone();

            let images: Vec<ToolContent> = result
                .content
                .into_iter()
                .filter_map(|d| match d {
                    ComputerDisplay::Image { data, mime_type } => {
                        Some(ToolContent::Image { data, mime_type })
                    }
                    _ => None,
                })
                .collect();

            match outcome.failed_index {
                None => {
                    let actions_str = outcome
                        .steps
                        .iter()
                        .map(|s| s.action.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let text = format!("{} action(s) done: {}.", outcome.steps.len(), actions_str);
                    let mut content = images;
                    content.push(ToolContent::text(text));
                    Ok(ToolResult {
                        content,
                        details: serde_json::to_value(&result.details).ok(),
                        structured_content: None,
                        is_error: None,
                    })
                }
                Some(idx) => {
                    let failed = &outcome.steps[idx];
                    let failure = computer_failure(
                        failed.code.as_deref().unwrap_or("Error"),
                        failed.message.as_deref().unwrap_or("action failed"),
                        &stop_hotkey,
                        failed.permission.as_ref(),
                    );

                    let fail_text = if failure.permission.is_none() {
                        failure.message.clone()
                    } else {
                        serde_json::to_string(&failure).unwrap_or_else(|_| failure.message.clone())
                    };

                    let text = format!(
                        "Action {} ({}) failed. {}",
                        failed.index + 1,
                        failed.action,
                        fail_text
                    );

                    let mut content = images;
                    content.push(ToolContent::text(text));

                    let mut outcome_with_failure = outcome.clone();
                    outcome_with_failure.failure = Some(failure);

                    let mut details = json!({
                        "value": outcome_with_failure,
                        "isError": true
                    });
                    if let Some(ro) = result.details.read_only {
                        details["readOnly"] = json!(ro);
                    }
                    if let Some(sc) = &result.details.screenshots {
                        details["screenshots"] = json!(sc);
                    }
                    if let Some(au) = &result.details.audit {
                        details["audit"] = json!(au);
                    }

                    Ok(ToolResult {
                        content,
                        details: Some(details),
                        structured_content: None,
                        is_error: None,
                    })
                }
            }
        })
    });

    def
}
