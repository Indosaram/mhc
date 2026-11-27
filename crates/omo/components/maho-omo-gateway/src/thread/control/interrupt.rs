//! Interrupt control helper (`thread/control/interrupt.ts`).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::super::contracts::results::{
    ThreadInterruptBackground, ThreadInterruptBackgroundFailedJob, ThreadInterruptBackgroundJob,
};
use super::super::tools::ports::{BoxFuture, InterruptRequest};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterruptOutcome {
    pub interrupted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub background: ThreadInterruptBackground,
}

fn job_of(entry: &Value) -> Option<ThreadInterruptBackgroundJob> {
    let obj = entry.as_object()?;
    let kind = obj.get("kind").and_then(Value::as_str)?.to_owned();
    let id = obj.get("id").and_then(Value::as_str)?.to_owned();
    let description = obj
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(ThreadInterruptBackgroundJob {
        kind,
        id,
        description,
    })
}

fn background_of(value: Option<&Value>) -> ThreadInterruptBackground {
    let Some(obj) = value.and_then(Value::as_object) else {
        return ThreadInterruptBackground::NotSupported;
    };
    let stopped = obj
        .get("stopped")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(job_of).collect())
        .unwrap_or_default();
    let failed = obj
        .get("failed")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    let job = job_of(entry)?;
                    let reason = entry
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned();
                    Some(ThreadInterruptBackgroundFailedJob {
                        kind: job.kind,
                        id: job.id,
                        description: job.description,
                        reason,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    ThreadInterruptBackground::Stopped { stopped, failed }
}

pub async fn interrupt_over<F>(send: F, turn_id: Option<&str>) -> Result<InterruptOutcome, String>
where
    F: FnOnce(InterruptRequest) -> BoxFuture<Result<Value, String>>,
{
    let request = InterruptRequest {
        turn_id: turn_id.map(str::to_owned),
        background: "stop".to_owned(),
    };
    let reply = send(request).await?;
    let interrupted = reply.get("interrupted").and_then(Value::as_bool) == Some(true);
    let reported_turn = reply
        .get("turnId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let background = background_of(reply.get("background"));
    Ok(InterruptOutcome {
        interrupted,
        turn_id: reported_turn,
        background,
    })
}

pub fn interrupt_outcome_json(
    outcome: &InterruptOutcome,
    thread_id: &str,
    device: Option<&str>,
) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("kind".into(), Value::String("ok".into()));
    map.insert("thread".into(), Value::String(thread_id.to_owned()));
    if let Some(d) = device {
        map.insert("device".into(), Value::String(d.to_owned()));
    }
    if let Some(t) = &outcome.turn_id {
        map.insert("turn_id".into(), Value::String(t.clone()));
    }
    map.insert("interrupted".into(), Value::Bool(outcome.interrupted));
    map.insert(
        "background".into(),
        serde_json::to_value(&outcome.background)
            .unwrap_or_else(|_| json!({ "status": "not_supported" })),
    );
    Value::Object(map)
}
