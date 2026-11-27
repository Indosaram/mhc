//! Port of senpi `packages/agent/src/execution/loop-hook-host-protocol.ts`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum LoopHookMessage {
    #[serde(rename = "hook.call", rename_all = "camelCase")]
    Call {
        call_id: String,
        event: String,
        payload: Value,
    },
    #[serde(rename = "hook.hello")]
    Hello { instance: String },
    #[serde(rename = "hook.started", rename_all = "camelCase")]
    Started { call_id: String, instance: String },
    #[serde(rename = "hook.reply", rename_all = "camelCase")]
    Reply { call_id: String, result: Value },
    #[serde(rename = "hook.failed", rename_all = "camelCase")]
    Failed { call_id: String, error: String },
}

pub fn encode_loop_hook_message(message: &LoopHookMessage) -> Vec<u8> {
    serde_json::to_vec(message).unwrap_or_default()
}

fn is_id(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty())
}

pub fn decode_loop_hook_message(payload: &[u8]) -> Option<LoopHookMessage> {
    let val: Value = serde_json::from_slice(payload).ok()?;
    let obj = val.as_object()?;
    match obj.get("kind")?.as_str()? {
        "hook.call" => {
            let call_id = is_id(obj.get("callId"))?.to_owned();
            let event = is_id(obj.get("event"))?.to_owned();
            let payload = obj.get("payload")?.clone();
            Some(LoopHookMessage::Call {
                call_id,
                event,
                payload,
            })
        }
        "hook.hello" => {
            let instance = is_id(obj.get("instance"))?.to_owned();
            Some(LoopHookMessage::Hello { instance })
        }
        "hook.started" => {
            let call_id = is_id(obj.get("callId"))?.to_owned();
            let instance = is_id(obj.get("instance"))?.to_owned();
            Some(LoopHookMessage::Started { call_id, instance })
        }
        "hook.reply" => {
            let call_id = is_id(obj.get("callId"))?.to_owned();
            let result = obj.get("result")?.clone();
            Some(LoopHookMessage::Reply { call_id, result })
        }
        "hook.failed" => {
            let call_id = is_id(obj.get("callId"))?.to_owned();
            let error = obj.get("error")?.as_str()?.to_owned();
            Some(LoopHookMessage::Failed { call_id, error })
        }
        _ => None,
    }
}
