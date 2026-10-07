//! Kibitzer delivery lifecycle (latest `kibitzer/delivery.ts`).
//!
//! Concurrency-safe native port of `createKibitzerDelivery`. Per-session state sits behind its OWN
//! `Mutex`; the global map is held only to get/insert/remove the session `Arc`. Coordinator
//! callbacks (`enqueue`/`remove`) run OUTSIDE every lock, so a coordinator that re-enters
//! `mark_delivered` cannot deadlock. The pending snapshot is written while holding the session lock,
//! so a later accept can never be overwritten by a stale write. A session is removed only after a
//! re-check that it is still empty AND still the same `Arc`, so a concurrent accept is never lost.
//! The coordinator key is `kibitzer:{path}` - the upstream member scope (one coordinator per
//! wiring); it is deliberately NOT session-prefixed, matching `delivery.ts`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use memory_core::recall::{RecallLedger, RecallNudge, render_nudge_block};

use crate::context::MemoryIdentityContext;
use crate::kibitzer_notice::NUDGED_ENTRY_TYPE;
use crate::recall_consumer::GATE_SURFACE_HASH;
use crate::recall_session_read::RECALL_CUSTOM_TYPE;

/// The coordinator `source` label for a passive kibitzer entry.
pub const KIBITZER_COORDINATOR_SOURCE: &str = "kibitzer";

/// `Pick<PendingNudges, "write" | "delete">`: the pending handoff the delivery owns.
pub trait KibitzerPendingPort: Send + Sync {
    fn write(&self, session_id: &str, nudges: &[RecallNudge]) -> std::io::Result<()>;
    fn delete(&self, session_id: &str);
}

/// The pending-nudge port for one identity (`options.pendingFor`).
pub type KibitzerPendingFor = Arc<dyn Fn(&MemoryIdentityContext) -> Arc<dyn KibitzerPendingPort> + Send + Sync>;
/// Append one visible session entry (`options.appendEntry`).
pub type AppendEntry = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// A passive entry on the `IdleInjectionCoordinator`.
pub struct KibitzerCoordinatorEntry {
    pub key: String,
    pub source: String,
    pub passive: bool,
    pub custom_type: String,
    pub content: String,
    pub details: serde_json::Value,
    pub on_flushed: Arc<dyn Fn() + Send + Sync>,
}

/// `IdleInjectionCoordinator`: keyed passive entries. `enqueue` returns the typed refusal: `false`
/// when the coordinator did NOT take ownership (retired queue, or a runtime not retained yet),
/// matching the upstream boolean contract whose `IdleInjectionRetiredError` throw path the producer
/// must handle itself - the caller keeps the nudge and its pending handoff and must never read a
/// refusal as delivered.
pub trait KibitzerIdleCoordinator: Send + Sync {
    fn enqueue(&self, entry: KibitzerCoordinatorEntry) -> bool;
    fn remove(&self, key: &str);
}

/// The hidden steer message handed to `sendMessage({ deliverAs: "steer" })`.
pub struct KibitzerSteerMessage {
    pub custom_type: String,
    pub content: String,
    pub display: bool,
}

/// Delivery provenance recorded on the `nudged` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerDeliveryVia { Steer, Wake, Prompt }

impl KibitzerDeliveryVia {
    fn as_str(self) -> &'static str {
        match self { KibitzerDeliveryVia::Steer => "steer", KibitzerDeliveryVia::Wake => "wake", KibitzerDeliveryVia::Prompt => "prompt" }
    }
}

/// The `tool_result` gate: `hasPendingMessages() === false && isIdle() !== true`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KibitzerToolResultGate {
    pub has_pending_messages: bool,
    pub is_idle: bool,
}

pub struct KibitzerDeliveryOptions {
    pub ledger_for: Arc<dyn Fn(&MemoryIdentityContext) -> RecallLedger + Send + Sync>,
    pub pending_for: KibitzerPendingFor,
    pub coordinator: Option<Arc<dyn KibitzerIdleCoordinator>>,
    pub send_message: Arc<dyn Fn(KibitzerSteerMessage) -> Result<(), String> + Send + Sync>,
    pub append_entry: AppendEntry,
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
}

#[derive(Default)]
struct DeliveryState {
    nudges: BTreeMap<String, RecallNudge>,
    coordinator_keys: BTreeSet<String>,
    steering: bool,
}

pub struct KibitzerDelivery {
    options: KibitzerDeliveryOptions,
    sessions: Mutex<BTreeMap<String, Arc<Mutex<DeliveryState>>>>,
    running_sessions: Mutex<BTreeSet<String>>,
    tools_in_flight: Mutex<BTreeMap<String, BTreeSet<String>>>,
}

impl KibitzerDelivery {
    pub fn new(options: KibitzerDeliveryOptions) -> Arc<Self> {
        Arc::new(Self {
            options,
            sessions: Mutex::new(BTreeMap::new()),
            running_sessions: Mutex::new(BTreeSet::new()),
            tools_in_flight: Mutex::new(BTreeMap::new()),
        })
    }

    fn coordinator_key(path: &str) -> String { format!("kibitzer:{path}") }

    /// The per-session state, created on first use. The global map is held only for this lookup.
    fn state_for(&self, session_id: &str) -> Arc<Mutex<DeliveryState>> {
        let mut sessions = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(sessions.entry(session_id.to_string()).or_insert_with(|| Arc::new(Mutex::new(DeliveryState::default()))))
    }

    fn existing_state(&self, session_id: &str) -> Option<Arc<Mutex<DeliveryState>>> {
        self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(session_id).map(Arc::clone)
    }

    /// Remove the session only when it is STILL the same `Arc` and STILL empty. Holding the global
    /// map while locking the per-session state follows the map-then-state order used by
    /// `state_for`; no path holds state-then-map, so this cannot deadlock.
    fn remove_if_empty(&self, session_id: &str, state: &Arc<Mutex<DeliveryState>>) {
        let mut sessions = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = sessions.get(session_id)
            && Arc::ptr_eq(existing, state)
            && state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).nudges.is_empty()
        {
            sessions.remove(session_id);
        }
    }

    /// `writePending(sessionId, context, state)`: empty deletes, otherwise writes the full map. Must
    /// be called while the caller holds the session state lock so per-session ordering is preserved.
    fn write_pending(&self, session_id: &str, context: &MemoryIdentityContext, nudges: &[RecallNudge]) {
        let pending = (self.options.pending_for)(context);
        if nudges.is_empty() { pending.delete(session_id); return; }
        if let Err(error) = pending.write(session_id, nudges) {
            (self.options.warn)(&format!("omo-senpi kibitzer pending write skipped: {error}"));
        }
    }

    pub fn accept(self: &Arc<Self>, session_id: &str, context: &MemoryIdentityContext, nudges: &[RecallNudge]) {
        // The ledger mark is per-batch and outside every lock.
        let ledger = (self.options.ledger_for)(context);
        let entries: Vec<memory_core::recall::RecallSurfacedEntry> =
            nudges.iter().map(|nudge| memory_core::recall::RecallSurfacedEntry { path: nudge.path.clone(), hash: GATE_SURFACE_HASH.to_string() }).collect();
        if let Err(error) = ledger.mark_surfaced(session_id, &entries) {
            (self.options.warn)(&format!("omo-senpi kibitzer delivery ledger mark skipped: {error}"));
        }

        let state = self.state_for(session_id);
        // Merge + persist the FULL map under the session lock; collect coordinator work for later.
        // A coordinator key is NOT tracked here: it is tracked only after a SUCCESSFUL enqueue.
        let (snapshot_len, steering, to_enqueue) = {
            let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut to_enqueue: Vec<(String, RecallNudge)> = Vec::new();
            for nudge in nudges {
                state.nudges.insert(nudge.path.clone(), nudge.clone());
                if self.options.coordinator.is_some() {
                    to_enqueue.push((Self::coordinator_key(&nudge.path), nudge.clone()));
                }
            }
            let snapshot: Vec<RecallNudge> = state.nudges.values().cloned().collect();
            self.write_pending(session_id, context, &snapshot);
            (snapshot.len(), state.steering, to_enqueue)
        };

        // Coordinator enqueue OUTSIDE the state lock (its onFlushed may re-enter mark_delivered). A
        // REFUSED enqueue (`false`) is the upstream throw path: the nudge and its pending handoff stay
        // in place and NO key is tracked, so the caller keeps ownership. A key is tracked only after a
        // SUCCESSFUL enqueue AND only while the nudge is still held, so a synchronous `on_flushed`
        // reentry that already delivered it cannot leave a phantom key behind.
        if let Some(coordinator) = &self.options.coordinator {
            for (key, nudge) in to_enqueue {
                let path = nudge.path.clone();
                let check_path = path.clone();
                let flush_path = path.clone();
                let me = Arc::clone(self);
                let session = session_id.to_string();
                let context = context.clone();
                let accepted = coordinator.enqueue(KibitzerCoordinatorEntry {
                    key: key.clone(),
                    source: KIBITZER_COORDINATOR_SOURCE.to_string(),
                    passive: true,
                    custom_type: NUDGED_ENTRY_TYPE.to_string(),
                    content: render_nudge_block(&nudge),
                    details: serde_json::json!({ "path": path }),
                    on_flushed: Arc::new(move || { me.mark_delivered(&session, &context, std::slice::from_ref(&flush_path), KibitzerDeliveryVia::Wake); }),
                });
                if !accepted {
                    (self.options.warn)(&format!("omo-senpi kibitzer coordinator enqueue skipped: {key}"));
                    continue;
                }
                let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.nudges.contains_key(&check_path) { state.coordinator_keys.insert(key); }
            }
        }

        // Immediate steer only while running AND a tool call is in flight.
        let running = self.running_sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(session_id);
        let in_flight = self.tools_in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(session_id).map(|calls| calls.len()).unwrap_or(0);
        if running && in_flight > 0 && !steering && snapshot_len > 0 {
            self.steer(session_id, context);
        }
    }

    pub fn on_tool_result(self: &Arc<Self>, session_id: &str, context: &MemoryIdentityContext, gate: &KibitzerToolResultGate) {
        let (held, steering) = match self.existing_state(session_id) {
            Some(state) => {
                let state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                (state.nudges.len(), state.steering)
            }
            None => (0, false),
        };
        if held == 0 || steering || gate.has_pending_messages || gate.is_idle { return; }
        self.steer(session_id, context);
    }

    fn steer(self: &Arc<Self>, session_id: &str, context: &MemoryIdentityContext) {
        let Some(state) = self.existing_state(session_id) else { return; };
        let nudges = {
            let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.nudges.is_empty() || state.steering { return; }
            state.steering = true;
            state.nudges.values().cloned().collect::<Vec<_>>()
        };
        let content = nudges.iter().map(render_nudge_block).collect::<Vec<_>>().join("\n");
        match (self.options.send_message)(KibitzerSteerMessage { custom_type: RECALL_CUSTOM_TYPE.to_string(), content, display: false }) {
            Ok(()) => {
                let paths: Vec<String> = nudges.iter().map(|nudge| nudge.path.clone()).collect();
                self.mark_delivered(session_id, context, &paths, KibitzerDeliveryVia::Steer);
            }
            Err(error) => (self.options.warn)(&format!("omo-senpi kibitzer steer delivery failed: {error}")),
        }
        // Clear steering under the lock (the state may have been emptied by mark_delivered).
        if let Some(state) = self.existing_state(session_id) {
            state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).steering = false;
        }
    }

    pub fn drain_for_prompt(&self, session_id: &str) -> Vec<RecallNudge> {
        let Some(state) = self.existing_state(session_id) else { return Vec::new(); };
        let (nudges, to_remove) = {
            let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let nudges: Vec<RecallNudge> = state.nudges.values().cloned().collect();
            state.nudges.clear();
            let to_remove: Vec<String> = state.coordinator_keys.iter().cloned().collect();
            state.coordinator_keys.clear();
            (nudges, to_remove)
        };
        // Coordinator removal OUTSIDE the state lock.
        if let Some(coordinator) = &self.options.coordinator {
            for key in &to_remove { coordinator.remove(key); }
        }
        self.remove_if_empty(session_id, &state);
        nudges
    }

    pub fn on_compaction_accepted(&self, session_id: &str, context: &MemoryIdentityContext) {
        self.running_sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
        self.tools_in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
        let to_remove = self.clear_session(session_id);
        if let Some(coordinator) = &self.options.coordinator {
            for key in &to_remove { coordinator.remove(key); }
        }
        (self.options.pending_for)(context).delete(session_id);
    }

    pub fn on_session_shutdown(&self, session_id: &str) {
        self.running_sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
        self.tools_in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
        let to_remove = self.clear_session(session_id);
        if let Some(coordinator) = &self.options.coordinator {
            for key in &to_remove { coordinator.remove(key); }
        }
    }

    /// Drop the session from the map and return its coordinator keys (called under no state lock).
    fn clear_session(&self, session_id: &str) -> Vec<String> {
        let removed = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
        match removed {
            Some(state) => {
                let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                state.nudges.clear();
                state.coordinator_keys.iter().cloned().collect::<Vec<_>>()
            }
            None => Vec::new(),
        }
    }

    pub fn mark_delivered(self: &Arc<Self>, session_id: &str, context: &MemoryIdentityContext, paths: &[String], via: KibitzerDeliveryVia) {
        let Some(state) = self.existing_state(session_id) else { return; };
        let (delivered, emptied, to_remove) = {
            let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut delivered: Vec<RecallNudge> = Vec::new();
            let mut to_remove: Vec<String> = Vec::new();
            for path in paths {
                let Some(nudge) = state.nudges.remove(path) else { continue; };
                delivered.push(nudge);
                let key = Self::coordinator_key(path);
                state.coordinator_keys.remove(&key);
                to_remove.push(key);
            }
            if delivered.is_empty() { return; }
            let remaining: Vec<RecallNudge> = state.nudges.values().cloned().collect();
            // Rewrite pending under the SAME lock so a later accept cannot be overwritten by this.
            self.write_pending(session_id, context, &remaining);
            (delivered, state.nudges.is_empty(), to_remove)
        };
        // Coordinator removal + trace OUTSIDE the state lock.
        if let Some(coordinator) = &self.options.coordinator {
            for key in &to_remove { coordinator.remove(key); }
        }
        (self.options.append_entry)(NUDGED_ENTRY_TYPE, serde_json::json!({ "version": 1, "nudges": delivered, "via": via.as_str() }));
        if emptied {
            self.remove_if_empty(session_id, &state);
        }
    }

    pub fn mark_running(&self, session_id: &str) {
        self.running_sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(session_id.to_string());
        self.tools_in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
    }

    pub fn mark_settled(&self, session_id: &str) {
        self.running_sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
        self.tools_in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
    }

    pub fn mark_tool_started(&self, session_id: &str, tool_call_id: &str) {
        self.tools_in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(session_id.to_string()).or_default().insert(tool_call_id.to_string());
    }

    pub fn mark_tool_finished(&self, session_id: &str, tool_call_id: &str) {
        let mut tools = self.tools_in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(calls) = tools.get_mut(session_id) {
            calls.remove(tool_call_id);
            if calls.is_empty() { tools.remove(session_id); }
        }
    }

    pub fn mark_turn_ended(&self, session_id: &str) {
        self.tools_in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
    }
}

/// The host-supplied inputs of [`mount_recall_ports`]. `env` is the mount's resolved environment
/// (`BTreeMap`), matching the actual `omo_mount.rs` call site.
pub struct MountRecallPortsInput {
    pub actions: Arc<dyn maho_ext_api::ExtensionActions>,
    pub executor: tokio::runtime::Handle,
    pub resolve_context: crate::prompt::PromptContextResolver,
    pub env: BTreeMap<String, String>,
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
}

/// The memory-owned ports one recall wiring needs, built over the real host actions.
pub struct MountRecallPorts {
    pub caps: crate::kibitzer_events::KibitzerEventCaps,
    pub send_message: Arc<dyn Fn(KibitzerSteerMessage) -> Result<(), String> + Send + Sync>,
    pub pending_for: KibitzerPendingFor,
    pub coordinator: Option<Arc<dyn KibitzerIdleCoordinator>>,
    pub drain_pending_for: crate::recall_drain::DrainPendingFor,
    pub drain_queued: Option<crate::recall_drain::DrainQueued>,
}

struct RecallPendingWrite(memory_core::recall::PendingNudges);

impl KibitzerPendingPort for RecallPendingWrite {
    fn write(&self, session_id: &str, nudges: &[RecallNudge]) -> std::io::Result<()> {
        self.0.write(session_id, nudges)
    }
    fn delete(&self, session_id: &str) {
        self.0.delete(session_id);
    }
}

struct RecallPendingTake(memory_core::recall::PendingNudges);

impl crate::recall_drain::PendingNudgesPort for RecallPendingTake {
    fn take(&self, session_id: &str) -> Vec<RecallNudge> {
        self.0.take(session_id)
    }
}

/// Builds the memory-owned ports over the real host actions: the steer closure appends through
/// `ExtensionActions::send_message` with `deliver_as: steer`, and both pending ports read the
/// identity's `recall_pending` directory.
///
/// Three returned values are empty because this input cannot supply them, and they are returned
/// empty rather than invented:
///   * `caps` - `memory.recall.event_caps` resolves from settings, which this input does not carry,
///     so the pinned defaults are used;
///   * `coordinator` - the idle-injection coordinator is a host object the mount does not pass;
///   * `drain_queued` - the in-memory queue half is fed by the sidecar's accept path; the durable
///     half is `drain_pending_for`.
///
/// `executor`, `resolve_context`, `env` and `warn` are accepted for the mount's call shape; the
/// closures below read the identity off each `MemoryIdentityContext` instead.
pub fn mount_recall_ports(input: MountRecallPortsInput) -> MountRecallPorts {
    let MountRecallPortsInput { actions, executor: _, resolve_context: _, env: _, warn: _ } = input;
    let send_message: Arc<dyn Fn(KibitzerSteerMessage) -> Result<(), String> + Send + Sync> = {
        let actions = Arc::clone(&actions);
        Arc::new(move |message| {
            actions
                .send_message(
                    maho_ext_api::CustomMessage {
                        custom_type: message.custom_type,
                        content: vec![maho_ext_api::ToolContent::text(message.content)],
                        display: message.display,
                        details: None,
                    },
                    maho_ext_api::SendMessageOptions { trigger_turn: false, deliver_as: Some(maho_ext_api::DeliverAs::Steer) },
                )
                .map_err(|error| error.to_string())
        })
    };
    let pending_for: KibitzerPendingFor =
        Arc::new(|identity: &MemoryIdentityContext| {
            Arc::new(RecallPendingWrite(memory_core::recall::PendingNudges::new(identity.identity_paths.recall_pending.clone()))) as Arc<dyn KibitzerPendingPort>
        });
    let drain_pending_for: crate::recall_drain::DrainPendingFor =
        Arc::new(|identity: &MemoryIdentityContext| {
            Arc::new(RecallPendingTake(memory_core::recall::PendingNudges::new(identity.identity_paths.recall_pending.clone()))) as Arc<dyn crate::recall_drain::PendingNudgesPort>
        });
    MountRecallPorts {
        caps: crate::kibitzer_events::KibitzerEventCaps::default(),
        send_message,
        pending_for,
        coordinator: None,
        drain_pending_for,
        drain_queued: None,
    }
}

#[cfg(test)]
#[path = "kibitzer_delivery_tests.rs"]
mod tests;
