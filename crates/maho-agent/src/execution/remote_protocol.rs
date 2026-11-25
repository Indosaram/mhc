//! Port of senpi `packages/agent/src/execution/remote-protocol.ts`.

use maho_ai::types::ContentBlock;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::AgentToolResult;

use super::device_files::DeviceFileOp;
use super::tool_host::ExecutionScope;

pub const HAND_CONTROL_LANE: &str = "hand.control";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpochRefusal {
    PeerUnauthorized,
    EpochFenced,
    EpochStepExceeded,
    EpochUnsupported,
}

impl EpochRefusal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PeerUnauthorized => "peer_unauthorized",
            Self::EpochFenced => "epoch_fenced",
            Self::EpochStepExceeded => "epoch_step_exceeded",
            Self::EpochUnsupported => "epoch_unsupported",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "peer_unauthorized" => Some(Self::PeerUnauthorized),
            "epoch_fenced" => Some(Self::EpochFenced),
            "epoch_step_exceeded" => Some(Self::EpochStepExceeded),
            "epoch_unsupported" => Some(Self::EpochUnsupported),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteTerminalReason {
    Cancelled,
    LeaseExpired,
    HandRestarted,
    Uncertain,
}

impl RemoteTerminalReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::LeaseExpired => "lease_expired",
            Self::HandRestarted => "hand_restarted",
            Self::Uncertain => "uncertain",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cancelled" => Some(Self::Cancelled),
            "lease_expired" => Some(Self::LeaseExpired),
            "hand_restarted" => Some(Self::HandRestarted),
            "uncertain" => Some(Self::Uncertain),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteToolResult {
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub details: Option<Value>,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHello {
    pub instance: String,
    pub durable: bool,
    pub ops: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteInvoke {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
    pub execution_id: String,
    pub batch_id: String,
    pub index: usize,
    pub scope: ExecutionScope,
    pub tool_call_id: String,
    pub name: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_surface: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFile {
    pub execution_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
    pub scope: ExecutionScope,
    pub op: DeviceFileOp,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminal {
    pub execution_id: String,
    pub reason: RemoteTerminalReason,
    pub uncertain: bool,
    pub hooks_ran: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteHandMessage {
    Invoke(RemoteInvoke),
    File(RemoteFile),
    #[serde(rename_all = "camelCase")]
    Abort {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        epoch: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    Ack {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        epoch: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    Settled {
        execution_id: String,
        result: RemoteToolResult,
    },
    Terminal(RemoteTerminal),
    #[serde(rename_all = "camelCase")]
    Refused {
        execution_id: String,
        reason: String,
    },
    Hello {
        instance: String,
        durable: bool,
        ops: Vec<String>,
    },
    Floor {
        epoch: u64,
    },
    FloorAck {
        floor: u64,
    },
    FloorRefused {
        reason: EpochRefusal,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("remote_protocol: {0}")]
pub struct RemoteProtocolError(pub String);

pub fn encode_remote_message(message: &RemoteHandMessage) -> Vec<u8> {
    serde_json::to_vec(message).unwrap_or_default()
}

fn is_non_empty_str(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn is_epoch(v: Option<&Value>) -> Option<u64> {
    v.and_then(Value::as_u64).filter(|&e| e >= 1)
}

pub fn decode_remote_message(payload: &[u8]) -> Result<RemoteHandMessage, RemoteProtocolError> {
    let val: Value = serde_json::from_slice(payload)
        .map_err(|_| RemoteProtocolError("payload is not JSON".to_owned()))?;
    let obj = val
        .as_object()
        .ok_or_else(|| RemoteProtocolError("no execution ID".to_owned()))?;

    let kind = obj.get("kind").and_then(Value::as_str).unwrap_or("");
    match kind {
        "hello" => {
            let instance = is_non_empty_str(obj.get("instance"))
                .ok_or_else(|| RemoteProtocolError("malformed hello".to_owned()))?
                .to_owned();
            let durable = match obj.get("durable") {
                None => true,
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| RemoteProtocolError("malformed hello".to_owned()))?,
            };
            let ops = match obj.get("ops") {
                None => Vec::new(),
                Some(v) => {
                    let arr = v
                        .as_array()
                        .ok_or_else(|| RemoteProtocolError("malformed hello".to_owned()))?;
                    let mut out = Vec::with_capacity(arr.len());
                    for item in arr {
                        let s = is_non_empty_str(Some(item))
                            .ok_or_else(|| RemoteProtocolError("malformed hello".to_owned()))?;
                        out.push(s.to_owned());
                    }
                    out
                }
            };
            return Ok(RemoteHandMessage::Hello {
                instance,
                durable,
                ops,
            });
        }
        "floor" => {
            let epoch = is_epoch(obj.get("epoch"))
                .ok_or_else(|| RemoteProtocolError("malformed floor".to_owned()))?;
            return Ok(RemoteHandMessage::Floor { epoch });
        }
        "floor_ack" => {
            let floor = is_epoch(obj.get("floor"))
                .ok_or_else(|| RemoteProtocolError("malformed floor ack".to_owned()))?;
            return Ok(RemoteHandMessage::FloorAck { floor });
        }
        "floor_refused" => {
            let reason = obj
                .get("reason")
                .and_then(Value::as_str)
                .and_then(EpochRefusal::parse)
                .ok_or_else(|| RemoteProtocolError("malformed floor refusal".to_owned()))?;
            return Ok(RemoteHandMessage::FloorRefused { reason });
        }
        _ => {}
    }

    let execution_id = is_non_empty_str(obj.get("executionId"))
        .ok_or_else(|| RemoteProtocolError("no execution ID".to_owned()))?
        .to_owned();
    let epoch = match obj.get("epoch") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            is_epoch(Some(v)).ok_or_else(|| RemoteProtocolError("malformed epoch".to_owned()))?,
        ),
    };

    match kind {
        "invoke" => {
            let batch_id = is_non_empty_str(obj.get("batchId"))
                .ok_or_else(|| RemoteProtocolError("malformed invoke".to_owned()))?
                .to_owned();
            let index = obj
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| RemoteProtocolError("malformed invoke".to_owned()))?
                as usize;
            let scope_obj = obj
                .get("scope")
                .and_then(Value::as_object)
                .ok_or_else(|| RemoteProtocolError("malformed invoke".to_owned()))?;
            let scope_id = is_non_empty_str(scope_obj.get("id"))
                .ok_or_else(|| RemoteProtocolError("malformed invoke".to_owned()))?
                .to_owned();
            let scope_root = scope_obj
                .get("root")
                .and_then(Value::as_str)
                .ok_or_else(|| RemoteProtocolError("malformed invoke".to_owned()))?
                .to_owned();
            let tool_call_id = is_non_empty_str(obj.get("toolCallId"))
                .ok_or_else(|| RemoteProtocolError("malformed invoke".to_owned()))?
                .to_owned();
            let name = is_non_empty_str(obj.get("name"))
                .ok_or_else(|| RemoteProtocolError("malformed invoke".to_owned()))?
                .to_owned();
            let arguments = obj.get("arguments").cloned().unwrap_or(Value::Null);
            let session_ref = match obj.get("sessionRef") {
                None => None,
                Some(v) => Some(
                    v.as_str()
                        .ok_or_else(|| RemoteProtocolError("malformed invoke".to_owned()))?
                        .to_owned(),
                ),
            };
            let prompt_surface = match obj.get("promptSurface") {
                None => None,
                Some(v) => Some(
                    v.as_str()
                        .ok_or_else(|| RemoteProtocolError("malformed invoke".to_owned()))?
                        .to_owned(),
                ),
            };
            Ok(RemoteHandMessage::Invoke(RemoteInvoke {
                epoch,
                execution_id,
                batch_id,
                index,
                scope: ExecutionScope {
                    id: scope_id,
                    root: scope_root,
                },
                tool_call_id,
                name,
                arguments,
                session_ref,
                prompt_surface,
            }))
        }
        "file" => {
            let scope_obj = obj
                .get("scope")
                .and_then(Value::as_object)
                .ok_or_else(|| RemoteProtocolError("malformed file operation".to_owned()))?;
            let scope_id = is_non_empty_str(scope_obj.get("id"))
                .ok_or_else(|| RemoteProtocolError("malformed file operation".to_owned()))?
                .to_owned();
            let scope_root = scope_obj
                .get("root")
                .and_then(Value::as_str)
                .ok_or_else(|| RemoteProtocolError("malformed file operation".to_owned()))?
                .to_owned();
            let op_val = obj
                .get("op")
                .cloned()
                .ok_or_else(|| RemoteProtocolError("malformed file operation".to_owned()))?;
            let op: DeviceFileOp = serde_json::from_value(op_val)
                .map_err(|_| RemoteProtocolError("malformed file operation".to_owned()))?;
            let args = obj
                .get("args")
                .filter(|v| v.is_object())
                .cloned()
                .ok_or_else(|| RemoteProtocolError("malformed file operation".to_owned()))?;
            Ok(RemoteHandMessage::File(RemoteFile {
                execution_id,
                epoch,
                scope: ExecutionScope {
                    id: scope_id,
                    root: scope_root,
                },
                op,
                args,
            }))
        }
        "abort" => Ok(RemoteHandMessage::Abort {
            execution_id,
            epoch,
        }),
        "ack" => Ok(RemoteHandMessage::Ack {
            execution_id,
            epoch,
        }),
        "terminal" => {
            let reason = obj
                .get("reason")
                .and_then(Value::as_str)
                .and_then(RemoteTerminalReason::parse)
                .ok_or_else(|| RemoteProtocolError("malformed terminal outcome".to_owned()))?;
            let uncertain = obj
                .get("uncertain")
                .and_then(Value::as_bool)
                .ok_or_else(|| RemoteProtocolError("malformed terminal outcome".to_owned()))?;
            let hooks_ran = obj
                .get("hooksRan")
                .and_then(Value::as_bool)
                .ok_or_else(|| RemoteProtocolError("malformed terminal outcome".to_owned()))?;
            Ok(RemoteHandMessage::Terminal(RemoteTerminal {
                execution_id,
                reason,
                uncertain,
                hooks_ran,
            }))
        }
        "settled" => {
            let res_val = obj
                .get("result")
                .ok_or_else(|| RemoteProtocolError("malformed result".to_owned()))?;
            let res_obj = res_val
                .as_object()
                .ok_or_else(|| RemoteProtocolError("malformed result".to_owned()))?;
            if !res_obj.get("content").is_some_and(Value::is_array)
                || res_obj.get("isError").and_then(Value::as_bool).is_none()
            {
                return Err(RemoteProtocolError("malformed result".to_owned()));
            }
            let result: RemoteToolResult = serde_json::from_value(res_val.clone())
                .map_err(|_| RemoteProtocolError("malformed result".to_owned()))?;
            Ok(RemoteHandMessage::Settled {
                execution_id,
                result,
            })
        }
        "refused" => {
            let reason = obj
                .get("reason")
                .and_then(Value::as_str)
                .ok_or_else(|| RemoteProtocolError("malformed refusal".to_owned()))?
                .to_owned();
            Ok(RemoteHandMessage::Refused {
                execution_id,
                reason,
            })
        }
        _ => Err(RemoteProtocolError("unknown message kind".to_owned())),
    }
}

pub fn to_remote_tool_result(result: &AgentToolResult) -> RemoteToolResult {
    let added = result
        .added_tool_names
        .as_ref()
        .filter(|n| !n.is_empty())
        .cloned();
    RemoteToolResult {
        content: result.content.clone(),
        details: Some(result.details.clone()),
        is_error: result.is_error == Some(true),
        structured_content: result.structured_content.clone(),
        terminate: result.terminate,
        added_tool_names: added,
    }
}
