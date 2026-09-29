//! Child event transcript projection (`manager/transcript-log.ts`).

use serde_json::{Map, Value, json};

use crate::manager::child_handle::{ManagedChildEvent, ManagedChildHandle, Unsubscribe};
use crate::manager::runtime_fallback_event::{RuntimeFallbackStore, apply_runtime_fallback_event};
use crate::state::TaskRecord;
use crate::store::{PersistedTaskEvent, TaskRecordStore};

pub const TRANSCRIPT_ASSISTANT_EVENT: &str = "assistant_message";
pub const TRANSCRIPT_TOOL_EVENT: &str = "tool_execution";
pub const TRANSCRIPT_ERROR_EVENT: &str = "child_error";

pub trait TranscriptLogStore: RuntimeFallbackStore + Send + Sync {
    fn append_transcript_event(&self, task_id: &str, event: &PersistedTaskEvent);
}

impl RuntimeFallbackStore for TaskRecordStore {
    fn load_record(&self, task_id: &str) -> Option<TaskRecord> {
        self.load(task_id).ok().flatten()
    }

    fn supports_replace(&self) -> bool {
        true
    }

    fn replace_record(&self, record: &TaskRecord) {
        if let Err(error) = self.replace(record) {
            utils::logger::log(
                "senpi-task runtime fallback record replace failed",
                Some(&json!({ "taskId": record.task_id, "error": error.to_string() })),
            );
        }
    }
}

impl TranscriptLogStore for TaskRecordStore {
    fn append_transcript_event(&self, task_id: &str, event: &PersistedTaskEvent) {
        if let Err(error) = self.append_event(task_id, event) {
            utils::logger::log(
                "senpi-task transcript event append failed",
                Some(&json!({ "taskId": task_id, "error": error.to_string() })),
            );
        }
    }
}

pub fn log_transcript_event(
    store: &dyn TranscriptLogStore,
    task_id: &str,
    event: &ManagedChildEvent,
) {
    apply_runtime_fallback_event(store, task_id, event);
    if let Some(persisted) = to_persisted_event(event) {
        store.append_transcript_event(task_id, &persisted);
    }
}

pub fn subscribe_transcript_log(
    handle: &dyn ManagedChildHandle,
    store: std::sync::Arc<dyn TranscriptLogStore>,
    task_id: &str,
) -> Unsubscribe {
    let task_id = task_id.to_string();
    handle.subscribe(std::sync::Arc::new(move |event| {
        log_transcript_event(store.as_ref(), &task_id, event);
    }))
}

fn persisted(event_type: &str, payload: Value) -> Option<PersistedTaskEvent> {
    Some(PersistedTaskEvent {
        event_type: event_type.to_string(),
        payload,
    })
}

/// Absent strings are dropped, mirroring `JSON.stringify` of an `undefined` field.
fn string_fields(fields: &[(&str, &Option<String>)]) -> Value {
    let map: Map<String, Value> = fields
        .iter()
        .filter_map(|(key, value)| {
            value
                .as_ref()
                .map(|value| ((*key).to_string(), json!(value)))
        })
        .collect();
    Value::Object(map)
}

fn to_persisted_event(event: &ManagedChildEvent) -> Option<PersistedTaskEvent> {
    match event.event_type.as_str() {
        "message_end" => {
            let message = event.message.as_ref()?;
            if let Some(failure) = assistant_failure(message) {
                return persisted(TRANSCRIPT_ERROR_EVENT, failure);
            }
            assistant_text(message)
                .and_then(|text| persisted(TRANSCRIPT_ASSISTANT_EVENT, json!({ "text": text })))
        }
        "tool_execution_end" => {
            let tool = event.tool_name.as_ref()?;
            persisted(
                TRANSCRIPT_TOOL_EVENT,
                json!({ "tool": tool, "is_error": event.is_error == Some(true) }),
            )
        }
        "retry_fallback_applied" => persisted(
            &event.event_type,
            string_fields(&[
                ("from", &event.from),
                ("to", &event.to),
                ("chain_key", &event.chain_key),
                ("reason", &event.reason),
            ]),
        ),
        "retry_fallback_exhausted" => persisted(
            &event.event_type,
            string_fields(&[
                ("chain_key", &event.chain_key),
                ("last_error", &event.last_error),
            ]),
        ),
        _ => None,
    }
}

fn is_assistant(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("assistant")
}

fn assistant_failure(message: &Value) -> Option<Value> {
    if !is_assistant(message) || message.get("stopReason").and_then(Value::as_str) != Some("error")
    {
        return None;
    }
    let diagnostic = message
        .get("errorMessage")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .unwrap_or("child turn failed without a diagnostic message");
    Some(json!({ "message": diagnostic, "stop_reason": "error" }))
}

fn assistant_text(message: &Value) -> Option<String> {
    if !is_assistant(message) {
        return None;
    }
    let text: String = message
        .get("content")?
        .as_array()?
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    (!text.is_empty()).then_some(text)
}
