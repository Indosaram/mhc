//! Port of senpi packages/coding-agent/src/core/structured-session-lane.ts.
//!
//! The AgentSession end of the internal-only agent channel (senpi#4272, R3.C6.2). While a structured listener is
//! attached, it holds the channel's capture subscription; block controls reach structured listeners at their position
//! among the ordinary session events. Extension listeners, `session.subscribe` and every legacy destination never see a
//! control.
//!
//! PERF-DIVERGENCE(senpi structured-session-lane.ts): upstream enqueues each control on the session's agent event queue
//! to keep its position. Here the agent awaits every internal record before it hands the next event to the session, so
//! a control delivered from the channel listener already lands between the session events around it. Upstream reads an
//! event's capture through a WeakMap on the source event object; here the channel's latest event record is held until
//! the session emits the matching event.

use std::sync::{Arc, Mutex, MutexGuard};

use maho_agent::InternalAgentChannel;
use maho_agent::internal_events::{InternalEvent, OrderedAgentRecord, Unsubscribe};
use maho_agent::types::{AgentEvent, AgentMessage};
use maho_ai::utils::assistant_capture::{Captured, FrozenProgress, carry_assistant_capture};
use maho_ext_api::AgentSessionEvent;

/// One record of a session's internal structured lane: a session event with its provider capture, or a block control,
/// in session order.
#[derive(Debug, Clone)]
pub enum AgentSessionStructuredRecord {
    Event(Captured<AgentSessionEvent>),
    Progress(FrozenProgress),
}

pub type StructuredListener = Arc<dyn Fn(&AgentSessionStructuredRecord) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredLaneUnavailableError;

impl std::fmt::Display for StructuredLaneUnavailableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Structured lane unavailable: Agent was constructed outside the session layer")
    }
}

impl std::error::Error for StructuredLaneUnavailableError {}

#[derive(Default)]
struct LaneState {
    next_id: u64,
    listeners: Vec<(u64, StructuredListener)>,
    release_channel: Option<Unsubscribe>,
    /// The channel's latest event record, waiting for the session to emit that event.
    source: Option<Captured<AgentEvent>>,
}

#[derive(Clone, Default)]
pub struct StructuredSessionLane {
    channel: Option<InternalAgentChannel>,
    state: Arc<Mutex<LaneState>>,
}

/// Unsubscribes its structured listener when dropped; the last one releases the channel's capture subscription.
pub struct StructuredSubscription {
    lane: StructuredSessionLane,
    id: u64,
}

impl Drop for StructuredSubscription {
    fn drop(&mut self) {
        let release = {
            let mut state = self.lane.state();
            state.listeners.retain(|(id, _)| *id != self.id);
            if state.listeners.is_empty() {
                state.source = None;
                state.release_channel.take()
            } else {
                None
            }
        };
        if let Some(release) = release {
            release();
        }
    }
}

fn assistant_message(event: &AgentEvent) -> Option<&maho_ai::types::AssistantMessage> {
    match event {
        AgentEvent::MessageStart { message, .. } | AgentEvent::MessageEnd { message } => {
            match message {
                AgentMessage::Llm(maho_ai::types::Message::Assistant(message)) => {
                    Some(message.as_ref())
                }
                _ => None,
            }
        }
        _ => None,
    }
}

impl StructuredSessionLane {
    pub fn new(channel: Option<InternalAgentChannel>) -> Self {
        Self {
            channel,
            state: Arc::default(),
        }
    }

    fn state(&self) -> MutexGuard<'_, LaneState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn active(&self) -> bool {
        !self.state().listeners.is_empty()
    }

    pub fn subscribe(
        &self,
        listener: StructuredListener,
    ) -> Result<StructuredSubscription, StructuredLaneUnavailableError> {
        let channel = self
            .channel
            .as_ref()
            .ok_or(StructuredLaneUnavailableError)?;
        let mut state = self.state();
        let id = state.next_id;
        state.next_id += 1;
        state.listeners.push((id, listener));
        if state.release_channel.is_none() {
            let lane = Arc::downgrade(&self.state);
            state.release_channel = Some(channel.subscribe(Arc::new(
                move |record: OrderedAgentRecord| -> maho_ai::types::BoxFuture<'static, ()> {
                    if let Some(state) = lane.upgrade() {
                        let lane = StructuredSessionLane {
                            channel: None,
                            state,
                        };
                        match record.record {
                            InternalEvent::Progress(progress) => {
                                lane.emit(&AgentSessionStructuredRecord::Progress(progress));
                            }
                            InternalEvent::Event(event) => lane.state().source = Some(event),
                        }
                    }
                    Box::pin(async {})
                },
            )));
        }
        Ok(StructuredSubscription {
            lane: self.clone(),
            id,
        })
    }

    /// Publishes a session event with the provider capture of its source agent event.
    pub fn emit_event(&self, event: &AgentSessionEvent) {
        let source = match event {
            AgentSessionEvent::Agent(emitted) => {
                let mut state = self.state();
                state.source.take_if(|source| {
                    std::mem::discriminant(&source.value) == std::mem::discriminant(emitted)
                })
            }
            _ => None,
        };
        let record = match (source, event) {
            (Some(source), AgentSessionEvent::Agent(emitted)) => {
                // A user-facing description replaced the engine message, so the capture's frozen message follows it.
                let described = assistant_message(emitted)
                    .filter(|message| assistant_message(&source.value) != Some(*message));
                carry_assistant_capture(&source, event.clone(), described)
            }
            _ => Captured::bare(event.clone()),
        };
        self.emit(&AgentSessionStructuredRecord::Event(record));
    }

    pub fn dispose(&self) {
        let release = {
            let mut state = self.state();
            state.listeners.clear();
            state.source = None;
            state.release_channel.take()
        };
        if let Some(release) = release {
            release();
        }
    }

    fn emit(&self, record: &AgentSessionStructuredRecord) {
        let listeners: Vec<_> = self
            .state()
            .listeners
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect();
        for listener in listeners {
            listener(record);
        }
    }
}
