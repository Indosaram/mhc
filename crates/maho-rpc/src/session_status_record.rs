//! Port of senpi `packages/coding-agent/src/modes/rpc/session-status-record.ts`.
//!
//! Shared projection for inline legacy snapshots and the compact snapshot's streaming passes.

use crate::session_status::{
    SessionStatusEntryView, SessionStatusExtras, SessionStatusRuntimeView,
    derive_session_status_with_extras,
};
use crate::session_status_subscriptions::SubscriptionOptions;
use serde_json::{Map, Value, json};

pub struct StatusRecordSubscription<'a> {
    pub id: &'a str,
    pub seq: u64,
    pub options: &'a SubscriptionOptions,
    pub admission_id: Option<&'a str>,
}

/// Builds one wire `session_status` JSON record from a derived session status.
pub fn status_record(
    subscription: &StatusRecordSubscription<'_>,
    session_id: &str,
    entry: &SessionStatusEntryView,
    runtime: Option<&SessionStatusRuntimeView>,
    fanout_questions: usize,
    last_change: Option<Value>,
    last_effect: Option<Value>,
) -> Value {
    status_record_with_extras(
        subscription,
        session_id,
        entry,
        runtime,
        fanout_questions,
        last_change,
        last_effect,
        &SessionStatusExtras::default(),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "matches senpi statusRecord inputs cleanly"
)]
pub fn status_record_with_extras(
    subscription: &StatusRecordSubscription<'_>,
    session_id: &str,
    entry: &SessionStatusEntryView,
    runtime: Option<&SessionStatusRuntimeView>,
    fanout_questions: usize,
    last_change: Option<Value>,
    last_effect: Option<Value>,
    extras: &SessionStatusExtras,
) -> Value {
    let mut derived =
        derive_session_status_with_extras(entry, runtime, fanout_questions, None, None, extras);
    let context = derived.remove("context");
    let mut obj = Map::new();
    obj.insert("type".to_owned(), json!("session_status"));
    obj.insert("subscriptionId".to_owned(), json!(subscription.id));
    obj.insert("seq".to_owned(), json!(subscription.seq));
    obj.insert("sessionId".to_owned(), json!(session_id));
    obj.extend(derived);
    if subscription.options.include_context
        && let Some(ctx) = context
    {
        obj.insert("context".to_owned(), ctx);
    }
    if let Some(effect) = last_effect {
        obj.insert("lastEffect".to_owned(), effect);
    }
    obj.insert("lastChange".to_owned(), last_change.unwrap_or(Value::Null));
    if subscription.seq > 0
        && let Some(admission_id) = subscription.admission_id
    {
        obj.insert("admissionId".to_owned(), json!(admission_id));
        obj.insert("generation".to_owned(), json!(0));
    }
    Value::Object(obj)
}

/// Wraps an already-constructed status JSON value with optional compact `admissionId`/`generation`.
pub fn attach_admission_to_status_record(
    mut record: Value,
    seq: u64,
    admission_id: Option<&str>,
) -> Value {
    if let Some(obj) = record.as_object_mut()
        && seq > 0
        && let Some(id) = admission_id
    {
        obj.insert("admissionId".to_owned(), json!(id));
        obj.insert("generation".to_owned(), json!(0));
    }
    record
}
