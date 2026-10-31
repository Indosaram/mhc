//! Port of senpi packages/agent/src/internal-events.ts.
//!
//! Internal to senpi packages; not a public API; may change without notice.
//!
//! PERF-DIVERGENCE(senpi assistant capture): upstream keys per-channel state in a module WeakMap
//! and freezes each record object. Here the channel owns its state behind an `Arc<Mutex<_>>` with
//! private fields (no getter, no attach path, no process-global registry), and a record is an
//! immutable value: listeners receive their own clone of the ordered record, whose capture is a
//! shared `Arc`.

use std::sync::{Arc, Mutex, MutexGuard};

use maho_ai::types::BoxFuture;
use maho_ai::utils::assistant_capture::{
    AssistantCaptureSubscription, Captured, FrozenProgress, subscribe_assistant_capture,
};

use crate::types::{AgentEvent, AgentLoopConfig};

/// `typeof internalProgressSink`: the loop hands every provider progress record to it, in order.
pub(crate) type ProgressSink = Arc<dyn Fn(FrozenProgress) -> BoxFuture<'static, ()> + Send + Sync>;

/// A loop event with the assistant capture its source carried (`carryAssistantCapture` target).
pub(crate) type CapturedEventSink =
    Arc<dyn Fn(Captured<AgentEvent>) -> BoxFuture<'static, ()> + Send + Sync>;

/// `InternalLoopConfig = AgentLoopConfig & { [internalProgressSink]?: ProgressSink }`. The sink
/// lives in the crate-private `AgentLoopConfig::internal_progress_sink` field.
pub(crate) type InternalLoopConfig = AgentLoopConfig;

/// `InternalEvent = { event } | { progress }`; the event keeps the assistant capture it carries.
#[derive(Debug, Clone)]
pub enum InternalEvent {
    Event(Captured<AgentEvent>),
    Progress(FrozenProgress),
}

/// For senpi's session layer only.
#[derive(Debug, Clone)]
pub struct OrderedAgentRecord {
    pub record: InternalEvent,
    pub sequence: u64,
}

type Listener = Arc<dyn Fn(OrderedAgentRecord) -> BoxFuture<'static, ()> + Send + Sync>;

/// The function `subscribe` returns; calling it again is a no-op.
pub type Unsubscribe = Box<dyn Fn() + Send + Sync>;

struct ChannelState {
    sequence: u64,
    next_id: u64,
    listeners: Vec<(u64, Listener)>,
    release_capture: Option<AssistantCaptureSubscription>,
}

/// Construction-time capability for senpi's session layer only.
#[derive(Clone)]
pub struct InternalAgentChannel {
    state: Arc<Mutex<ChannelState>>,
}

impl std::fmt::Debug for InternalAgentChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternalAgentChannel")
            .finish_non_exhaustive()
    }
}

impl InternalAgentChannel {
    fn state(&self) -> MutexGuard<'_, ChannelState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn subscribe(
        &self,
        listener: Arc<dyn Fn(OrderedAgentRecord) -> BoxFuture<'static, ()> + Send + Sync>,
    ) -> Unsubscribe {
        let id = {
            let mut state = self.state();
            let id = state.next_id;
            state.next_id += 1;
            state.listeners.push((id, listener));
            if state.release_capture.is_none() {
                state.release_capture = Some(subscribe_assistant_capture());
            }
            id
        };
        let channel = self.clone();
        Box::new(move || {
            let mut state = channel.state();
            state.listeners.retain(|(entry, _)| *entry != id);
            if state.listeners.is_empty()
                && let Some(mut release) = state.release_capture.take()
            {
                release.release();
            }
        })
    }

    fn has_listeners(&self) -> bool {
        !self.state().listeners.is_empty()
    }
}

/// For senpi's session layer only. Pass at Agent construction; an existing Agent cannot be
/// attached. Capture is held from the first listener until the last release, never by an idle
/// channel.
pub fn create_internal_agent_channel() -> InternalAgentChannel {
    InternalAgentChannel {
        state: Arc::new(Mutex::new(ChannelState {
            sequence: 0,
            next_id: 0,
            listeners: Vec::new(),
            release_capture: None,
        })),
    }
}

/// A run started without an internal listener gets no sink, and its loop reads the public stream
/// exactly as before.
pub(crate) fn internal_loop_sink(channel: Option<&InternalAgentChannel>) -> Option<ProgressSink> {
    let channel = channel.filter(|channel| channel.has_listeners())?.clone();
    Some(Arc::new(move |progress| {
        match dispatch_internal_agent(Some(&channel), InternalEvent::Progress(progress)) {
            Some(delivery) => delivery,
            None => Box::pin(async {}),
        }
    }))
}

/// Returns nothing, so it costs the public pipeline no await, while the owner has no internal
/// listener. The sequence is taken when the record is dispatched, not when it is delivered.
pub(crate) fn dispatch_internal_agent(
    channel: Option<&InternalAgentChannel>,
    event: InternalEvent,
) -> Option<BoxFuture<'static, ()>> {
    let channel = channel?;
    let (record, listeners) = {
        let mut state = channel.state();
        if state.listeners.is_empty() {
            return None;
        }
        state.sequence += 1;
        let record = OrderedAgentRecord {
            record: event,
            sequence: state.sequence,
        };
        let listeners: Vec<Listener> = state
            .listeners
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect();
        (record, listeners)
    };
    Some(Box::pin(async move {
        for listener in listeners {
            listener(record.clone()).await;
        }
    }))
}

#[cfg(test)]
#[path = "internal_events_tests.rs"]
mod tests;
