//! Port of senpi `packages/agent/src/execution/remote-file-call.ts`.

use serde_json::json;

use crate::types::AgentToolResult;

use super::device_files::{DeviceFileErrorCode, DeviceFileOutcome};
use super::remote_protocol::{RemoteFile, RemoteInvoke};

pub fn file_call(message: &RemoteFile) -> RemoteInvoke {
    RemoteInvoke {
        epoch: message.epoch,
        execution_id: message.execution_id.clone(),
        batch_id: message.execution_id.clone(),
        index: 0,
        scope: message.scope.clone(),
        tool_call_id: message.execution_id.clone(),
        name: format!("file.{}", message.op.as_str()),
        arguments: message.args.clone(),
        session_ref: None,
        prompt_surface: None,
    }
}

pub fn file_result(outcome: DeviceFileOutcome) -> AgentToolResult {
    let is_ok = matches!(outcome, DeviceFileOutcome::Ok { .. });
    AgentToolResult {
        content: Vec::new(),
        details: json!({ "file": outcome }),
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: Some(!is_ok),
    }
}

pub fn file_refusal(code: DeviceFileErrorCode, message: impl Into<String>) -> AgentToolResult {
    file_result(DeviceFileOutcome::err(code, message))
}

pub fn file_stopped(granted: bool) -> AgentToolResult {
    if granted {
        file_refusal(DeviceFileErrorCode::Aborted, "the operation was aborted")
    } else {
        file_refusal(DeviceFileErrorCode::RefusedByGrant, "the grant was revoked")
    }
}
