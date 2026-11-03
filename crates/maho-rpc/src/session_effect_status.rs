//! The `effect_status` record and the pending-transition bus behind it (senpi `rpc/session-effect-status.ts`).
//!
//! Ledgers call [`note_session_effect`] the moment they persist a transition. Each multi-session host
//! installs its own sink and owns the sessions it holds; a transition of a session no installed sink owns at
//! that moment is dropped. A held transition reaches `effects: true` subscriptions only AFTER the session
//! record that caused it is on the connection's FIFO: the writer drains a session's notes right after each
//! record it publishes ([`drain_session_effects`]), and a coalesced flush covers transitions recorded from
//! inside a session event. Sessions are keyed by their routing id (senpi keys the `AgentSession` object).
use crate::effect_record::{EffectActor, EffectKind, EffectOutcome, EffectState, EffectStatus};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// One `effect_status` record as it travels on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEffectStatusRecord {
    pub r#type: &'static str,
    pub subscription_id: String,
    pub seq: u64,
    pub session_id: String,
    pub effect_id: String,
    pub kind: EffectKind,
    pub state: EffectState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<EffectOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<EffectActor>,
    pub at: String,
}

pub fn effect_status_record(
    subscription_id: &str,
    seq: u64,
    session_id: &str,
    status: &EffectStatus,
    at: &str,
) -> SessionEffectStatusRecord {
    SessionEffectStatusRecord {
        r#type: "effect_status",
        subscription_id: subscription_id.to_owned(),
        seq,
        session_id: session_id.to_owned(),
        effect_id: status.effect_id.clone(),
        kind: status.kind,
        state: status.state,
        disposition: status.disposition.clone(),
        outcome: status.outcome.clone(),
        by: status.by.clone(),
        at: at.to_owned(),
    }
}

/// A host that publishes `effect_status` for the sessions it owns.
pub trait SessionEffectSink: Send + Sync {
    fn owns(&self, session_id: &str) -> bool;
    fn flush(&self, session_id: &str, statuses: Vec<EffectStatus>);
}

#[derive(Default)]
struct Bus {
    next_sink: u64,
    sinks: Vec<(u64, Arc<dyn SessionEffectSink>)>,
    pending: HashMap<String, Vec<EffectStatus>>,
    dirty: Vec<(String, u64)>,
    scheduled: bool,
}

fn bus() -> std::sync::MutexGuard<'static, Bus> {
    static BUS: OnceLock<Mutex<Bus>> = OnceLock::new();
    BUS.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A ledger persisted a transition: hold it until the causing record is published, then deliver. Without a
/// tokio runtime the coalesced flush cannot be scheduled and the writer's drain is the only delivery.
pub fn note_session_effect(session_id: &str, status: EffectStatus) {
    let mut bus = bus();
    let Some(owner) = bus
        .sinks
        .iter()
        .find(|(_, sink)| sink.owns(session_id))
        .map(|(id, _)| *id)
    else {
        return;
    };
    bus.pending
        .entry(session_id.to_owned())
        .or_default()
        .push(status);
    if !bus.dirty.iter().any(|(session, _)| session == session_id) {
        bus.dirty.push((session_id.to_owned(), owner));
    }
    if !bus.scheduled
        && let Ok(runtime) = tokio::runtime::Handle::try_current()
    {
        bus.scheduled = true;
        runtime.spawn(async { flush_dirty() });
    }
}

/// Take a session's held transitions; the writer calls this right after publishing its causing record.
pub fn drain_session_effects(session_id: &str) -> Vec<EffectStatus> {
    bus().pending.remove(session_id).unwrap_or_default()
}

fn flush_dirty() {
    let deliveries: Vec<(String, Arc<dyn SessionEffectSink>, Vec<EffectStatus>)> = {
        let mut bus = bus();
        bus.scheduled = false;
        let dirty = std::mem::take(&mut bus.dirty);
        dirty
            .into_iter()
            .filter_map(|(session, owner)| {
                let drained = bus.pending.remove(&session).unwrap_or_default();
                let sink = bus
                    .sinks
                    .iter()
                    .find(|(id, _)| *id == owner)
                    .map(|(_, sink)| sink.clone())?;
                (!drained.is_empty()).then_some((session, sink, drained))
            })
            .collect()
    };
    // Delivered outside the bus lock: a sink may note further transitions.
    for (session, sink, drained) in deliveries {
        sink.flush(&session, drained);
    }
}

/// Each host installs its own sink; the returned closure removes it and stops holding for its sessions.
pub fn install_session_effect_sink(sink: Arc<dyn SessionEffectSink>) -> impl FnOnce() {
    let id = {
        let mut bus = bus();
        bus.next_sink += 1;
        let id = bus.next_sink;
        bus.sinks.push((id, sink));
        id
    };
    move || {
        let mut bus = bus();
        bus.sinks.retain(|(existing, _)| *existing != id);
        let owned: Vec<String> = bus
            .dirty
            .iter()
            .filter(|(_, owner)| *owner == id)
            .map(|(session, _)| session.clone())
            .collect();
        bus.dirty.retain(|(_, owner)| *owner != id);
        for session in owned {
            bus.pending.remove(&session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(effect_id: &str) -> EffectStatus {
        EffectStatus {
            effect_id: effect_id.into(),
            kind: EffectKind::Abort,
            state: EffectState::Done,
            disposition: None,
            outcome: None,
            answer: None,
            by: None,
            accepted_at: None,
            updated_at: None,
            session_id: None,
            durable_session_id: "d".into(),
        }
    }

    struct Sink {
        prefix: &'static str,
        flushed: Mutex<Vec<(String, Vec<String>)>>,
        notify: tokio::sync::Notify,
    }
    impl SessionEffectSink for Sink {
        fn owns(&self, session_id: &str) -> bool {
            session_id.starts_with(self.prefix)
        }
        fn flush(&self, session_id: &str, statuses: Vec<EffectStatus>) {
            self.flushed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((
                    session_id.to_owned(),
                    statuses
                        .into_iter()
                        .map(|status| status.effect_id)
                        .collect(),
                ));
            self.notify.notify_one();
        }
    }

    #[test]
    fn record_carries_only_present_fields() {
        let record = effect_status_record("sub", 3, "s", &status("e"), "t");
        assert_eq!(
            serde_json::to_value(record).expect("wire"),
            serde_json::json!({"type":"effect_status","subscriptionId":"sub","seq":3,"sessionId":"s","effectId":"e","kind":"abort","state":"done","at":"t"})
        );
    }

    #[tokio::test]
    async fn unowned_sessions_drop_and_owned_ones_drain_or_flush() {
        let sink = Arc::new(Sink {
            prefix: "effect-bus-test-",
            flushed: Mutex::default(),
            notify: tokio::sync::Notify::new(),
        });
        let remove = install_session_effect_sink(sink.clone());
        note_session_effect("unowned-effect-bus-session", status("x"));
        assert!(drain_session_effects("unowned-effect-bus-session").is_empty());
        note_session_effect("effect-bus-test-a", status("e1"));
        let drained = drain_session_effects("effect-bus-test-a");
        assert_eq!(drained.len(), 1);
        note_session_effect("effect-bus-test-b", status("e2"));
        tokio::time::timeout(std::time::Duration::from_secs(5), sink.notify.notified())
            .await
            .expect("flushed");
        assert_eq!(
            sink.flushed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            vec![("effect-bus-test-b".to_owned(), vec!["e2".to_owned()])]
        );
        remove();
        note_session_effect("effect-bus-test-c", status("e3"));
        assert!(drain_session_effects("effect-bus-test-c").is_empty());
    }
}
