//! Port of senpi `packages/agent/src/execution/prepared/executor-freeze.ts`.

use maho_ai::types::ContentBlock;
use serde_json::{Value, json};

use crate::durable::store::StoreValue;
use crate::types::AgentToolResult;

use super::super::canonical_digest::canonical_digest;
use super::super::ledger_types::FrozenCall;
use super::executor_body::{BodyOutcome, error_result};
use super::executor_types::PrepareReply;

/// Tool arguments and results are JSON on every model and wire path; a round trip drops what JSON cannot carry.
pub fn to_store_value<T: serde::Serialize>(value: &T) -> Result<StoreValue, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

/// The call after the whole `tool_call` chain, detached from every object a handler or caller still holds.
pub fn freeze_call(name: &str, args: &Value) -> FrozenCall {
    let frozen_args = args.clone();
    let digest = canonical_digest(&json!({ "name": name, "args": frozen_args }));
    FrozenCall {
        name: name.to_owned(),
        args: frozen_args,
        digest,
    }
}

fn result_value(
    result: &AgentToolResult,
    is_error: bool,
    requested_name: Option<&str>,
) -> Result<StoreValue, String> {
    to_store_value(&json!({
        "content": result.content,
        "isError": is_error,
        "requestedName": requested_name,
    }))
}

pub struct RecordedOutcome {
    pub outcome: BodyOutcome,
    pub value: StoreValue,
}

/// A settled body and its durable record. Content JSON cannot carry settles the call as a tool error.
pub fn recorded_result(outcome: BodyOutcome, requested_name: Option<&str>) -> RecordedOutcome {
    match result_value(&outcome.result, outcome.is_error, requested_name) {
        Ok(value) => RecordedOutcome { outcome, value },
        Err(err) => {
            let result = error_result(format!("Tool result content is not JSON: {err}"));
            let fallback_val = json!({
                "content": result.content,
                "isError": true,
                "requestedName": requested_name,
            });
            RecordedOutcome {
                outcome: BodyOutcome {
                    result,
                    is_error: true,
                },
                value: fallback_val,
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum StoppedReply {
    Blocked {
        result: AgentToolResult,
        requested_name: Option<String>,
    },
    BatchAbort {
        result: AgentToolResult,
    },
}

impl From<StoppedReply> for PrepareReply {
    fn from(value: StoppedReply) -> Self {
        match value {
            StoppedReply::Blocked {
                result,
                requested_name,
            } => PrepareReply::Blocked {
                result,
                requested_name,
            },
            StoppedReply::BatchAbort { result } => PrepareReply::BatchAbort { result },
        }
    }
}

/// A preparation that ended without a frozen call, as the ledger's durable `blocked` outcome value.
pub fn stopped_value(reply: &StoppedReply) -> StoreValue {
    let (kind, result, requested_name) = match reply {
        StoppedReply::Blocked {
            result,
            requested_name,
        } => ("blocked", result, requested_name.as_deref()),
        StoppedReply::BatchAbort { result } => ("batch_abort", result, None),
    };
    let text: String = result
        .content
        .iter()
        .filter_map(|part| match part {
            ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    json!({
        "kind": kind,
        "text": text,
        "terminate": result.terminate == Some(true),
        "requestedName": requested_name,
    })
}

/// The reply a re-sent prepare gets back from a durable `blocked` outcome; `None` for a value this module never wrote.
pub fn stopped_reply(value: &StoreValue) -> Option<StoppedReply> {
    let obj = value.as_object()?;
    let kind = obj.get("kind")?.as_str()?;
    let text = obj.get("text")?.as_str()?;
    let terminate = obj.get("terminate")?.as_bool()?;
    let mut result = error_result(text);
    if terminate {
        result.terminate = Some(true);
    }
    match kind {
        "batch_abort" => Some(StoppedReply::BatchAbort { result }),
        "blocked" => {
            let requested_name = obj
                .get("requestedName")
                .and_then(Value::as_str)
                .map(str::to_owned);
            Some(StoppedReply::Blocked {
                result,
                requested_name,
            })
        }
        _ => None,
    }
}
