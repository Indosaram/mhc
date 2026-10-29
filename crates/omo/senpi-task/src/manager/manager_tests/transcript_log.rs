//! `transcript-log.test.ts`

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::host::HostError;
use crate::manager::child_handle::{
    ManagedChildEvent, ManagedChildHandle, ManagedChildListener, Unsubscribe,
};
use crate::manager::runtime_fallback_event::RuntimeFallbackStore;
use crate::manager::transcript_log::{
    TRANSCRIPT_ASSISTANT_EVENT, TRANSCRIPT_TOOL_EVENT, TranscriptLogStore, log_transcript_event,
    subscribe_transcript_log,
};
use crate::runners::RunnerOutcome;
use crate::store::PersistedTaskEvent;

#[derive(Default)]
struct RecordingStore {
    appended: Mutex<Vec<(String, PersistedTaskEvent)>>,
}

impl RuntimeFallbackStore for RecordingStore {}

impl TranscriptLogStore for RecordingStore {
    fn append_transcript_event(&self, task_id: &str, event: &PersistedTaskEvent) {
        self.appended
            .lock()
            .expect("appended")
            .push((task_id.to_string(), event.clone()));
    }
}

impl RecordingStore {
    fn appended(&self) -> Vec<(String, PersistedTaskEvent)> {
        self.appended.lock().expect("appended").clone()
    }
}

fn event(event_type: &str) -> ManagedChildEvent {
    ManagedChildEvent::of(event_type)
}

fn message_end(message: Value) -> ManagedChildEvent {
    ManagedChildEvent {
        message: Some(message),
        ..event("message_end")
    }
}

fn assistant_end() -> ManagedChildEvent {
    message_end(
        json!({ "role": "assistant", "content": [{ "type": "text", "text": "done reasoning" }] }),
    )
}

#[test]
fn given_an_assistant_message_end_when_logged_then_an_assistant_transcript_event_is_appended_with_the_text()
 {
    let store = RecordingStore::default();
    log_transcript_event(&store, "st_1", &assistant_end());
    let appended = store.appended();
    assert_eq!(appended.len(), 1);
    assert_eq!(appended[0].1.event_type, TRANSCRIPT_ASSISTANT_EVENT);
    assert_eq!(appended[0].1.payload, json!({ "text": "done reasoning" }));
}

#[test]
fn given_a_tool_execution_end_when_logged_then_a_tool_transcript_event_is_appended_with_the_tool_name_and_error_flag()
 {
    let store = RecordingStore::default();
    let tool_end = ManagedChildEvent {
        tool_name: Some("bash".to_string()),
        result: Some(json!("ok")),
        is_error: Some(false),
        ..event("tool_execution_end")
    };
    log_transcript_event(&store, "st_1", &tool_end);
    let appended = store.appended();
    assert_eq!(appended[0].1.event_type, TRANSCRIPT_TOOL_EVENT);
    assert_eq!(
        appended[0].1.payload,
        json!({ "tool": "bash", "is_error": false })
    );
}

#[test]
fn given_a_non_assistant_message_end_when_logged_then_nothing_is_appended() {
    let store = RecordingStore::default();
    log_transcript_event(
        &store,
        "st_1",
        &message_end(json!({ "role": "user", "content": [{ "type": "text", "text": "hi" }] })),
    );
    assert!(store.appended().is_empty());
}

#[test]
fn given_a_retry_fallback_exhausted_event_when_logged_then_it_is_appended_with_the_chain_key_and_last_error()
 {
    let store = RecordingStore::default();
    let exhausted = ManagedChildEvent {
        chain_key: Some("kimi-coding/kimi-for-coding-highspeed".to_string()),
        last_error: Some("403 quota".to_string()),
        ..event("retry_fallback_exhausted")
    };
    log_transcript_event(&store, "st_1", &exhausted);
    let appended = store.appended();
    assert_eq!(appended.len(), 1);
    assert_eq!(appended[0].1.event_type, "retry_fallback_exhausted");
    assert_eq!(
        appended[0].1.payload,
        json!({ "chain_key": "kimi-coding/kimi-for-coding-highspeed", "last_error": "403 quota" })
    );
}

#[test]
fn given_an_unrelated_event_type_when_logged_then_nothing_is_appended() {
    let store = RecordingStore::default();
    log_transcript_event(&store, "st_1", &event("queue_update"));
    log_transcript_event(
        &store,
        "st_1",
        &ManagedChildEvent {
            tool_name: Some("bash".to_string()),
            ..event("tool_execution_start")
        },
    );
    assert!(store.appended().is_empty());
}

#[derive(Default)]
struct ListenerHandle {
    listener: Mutex<Option<ManagedChildListener>>,
    unsubscribed: Arc<Mutex<bool>>,
}

impl ManagedChildHandle for ListenerHandle {
    fn task_id(&self) -> &str {
        "st_2"
    }
    fn session_id(&self) -> Option<String> {
        None
    }
    fn pid(&self) -> Option<i64> {
        None
    }
    fn steer(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }
    fn follow_up(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }
    fn abort(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn subscribe(&self, listener: ManagedChildListener) -> Unsubscribe {
        *self.listener.lock().expect("listener") = Some(listener);
        let unsubscribed = Arc::clone(&self.unsubscribed);
        Box::new(move || *unsubscribed.lock().expect("flag") = true)
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        RunnerOutcome::Cancelled
    }
    fn last_assistant_text(&self) -> Option<String> {
        None
    }
    fn dispose(&self) -> Result<(), HostError> {
        Ok(())
    }
}

#[test]
fn given_a_handle_when_subscribed_then_transcript_events_flow_into_the_store_and_the_unsubscribe_is_returned()
 {
    let store = Arc::new(RecordingStore::default());
    let handle = ListenerHandle::default();
    let unsubscribe = subscribe_transcript_log(
        &handle,
        Arc::clone(&store) as Arc<dyn TranscriptLogStore>,
        "st_2",
    );
    let listener = handle
        .listener
        .lock()
        .expect("listener")
        .clone()
        .expect("subscribed");
    listener(&assistant_end());
    unsubscribe();
    let appended = store.appended();
    assert_eq!(appended.len(), 1);
    assert_eq!(appended[0].0, "st_2");
    assert!(*handle.unsubscribed.lock().expect("flag"));
}

#[test]
fn given_an_assistant_message_end_carrying_a_stop_reason_error_when_logged_then_a_child_error_transcript_event_is_appended_with_the_diagnostic()
 {
    let store = RecordingStore::default();
    log_transcript_event(
        &store,
        "st_3",
        &message_end(
            json!({ "role": "assistant", "content": [], "stopReason": "error", "errorMessage": "upstream gateway timeout" }),
        ),
    );
    let appended = store.appended();
    assert_eq!(appended.len(), 1);
    assert_eq!(appended[0].1.event_type, "child_error");
    assert_eq!(
        appended[0].1.payload,
        json!({ "message": "upstream gateway timeout", "stop_reason": "error" })
    );
}
