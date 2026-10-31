//! Port of senpi packages/coding-agent/src/modes/interactive/session-control-lifecycle.ts (v2026.10.27-2).
//!
//! A bound control endpoint's live wiring and its clean exit.
//!
//! Edges: `agent_idle` -> idle; a delivery's entry written (its `message_end`) -> emitted;
//! SIGCONT -> continue; the last input hold of the session ended (`on_inputs_settled`) -> submission. The host adds submission (its
//! last open submission ticket released) and draft_cleared; the socket adds command; the watcher adds
//! inbox.
//!
//! Clean exit, in order: stop every edge source, close the socket, apply the header-only rule (OPEN
//! until mhc's `SessionManager` can discard a header-only file; the file is kept meanwhile) (a
//! session file holding nothing but its header is removed only when the registrant says nothing
//! references the session - an unanswerable question keeps the file), then remove the endpoint.
//! A process that exits without disposing still removes its endpoint on exit.
use crate::session_control_commands::TuiControlSurface;
use crate::session_control_feed::{ControlFeed, ControlFeedKind};
use crate::session_control_registry::{
    TuiRegistryEntry, unregister_tui_endpoint, unregister_tui_endpoint_sync,
};
use crate::session_control_server::ControlServer;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use maho_core::agent_session::AgentSession;
use maho_core::extensions::session_control_types::RegisterControlEndpointOptions;
use maho_ext_api::{AgentEvent, AgentMessage, AgentSessionEvent};
use maho_rpc::session_control_wake::{WakeScheduler, on_process_continue};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};

pub struct TuiControlContext {
    pub session: Arc<AgentSession>,
    pub agent_dir: String,
    pub surface: Arc<dyn TuiControlSurface>,
    pub editor_revision: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Clone for TuiControlContext {
    fn clone(&self) -> Self {
        Self {
            session: self.session.clone(),
            agent_dir: self.agent_dir.clone(),
            surface: self.surface.clone(),
            editor_revision: self.editor_revision.clone(),
        }
    }
}

pub type WakeFn = Arc<dyn Fn(&str) -> BoxFuture<'static, Value> + Send + Sync>;

#[derive(Clone)]
pub struct ActiveControlEndpoint {
    pub socket: String,
    wake: WakeFn,
    feed: Arc<Mutex<ControlFeed>>,
    surface: Arc<dyn TuiControlSurface>,
    disposing: Arc<Mutex<Option<Shared<BoxFuture<'static, ()>>>>>,
    teardown: Arc<Mutex<Option<BoxFuture<'static, ()>>>>,
}

impl ActiveControlEndpoint {
    pub fn wake(&self, reason: &str) {
        tokio::spawn((self.wake)(reason));
    }

    pub fn note_state(&self) {
        lock(&self.feed).publish(ControlFeedKind::State, Map::new());
    }

    pub fn note_questions(&self) {
        let mut data = Map::new();
        data.insert("pending".into(), json!(self.surface.pending_question_ids()));
        lock(&self.feed).publish(ControlFeedKind::Question, data);
    }

    /// Idempotent: a second call awaits the first teardown.
    pub fn dispose(&self) -> BoxFuture<'static, ()> {
        let shared = {
            let mut disposing = lock(&self.disposing);
            disposing
                .get_or_insert_with(|| {
                    let teardown = lock(&self.teardown)
                        .take()
                        .unwrap_or_else(|| Box::pin(async {}));
                    teardown.shared()
                })
                .clone()
        };
        Box::pin(shared)
    }
}

pub struct ActivationParts {
    pub context: TuiControlContext,
    pub options: RegisterControlEndpointOptions,
    pub entry: TuiRegistryEntry,
    pub server: Arc<ControlServer>,
    pub scheduler: WakeScheduler,
    pub feed: Arc<Mutex<ControlFeed>>,
    pub stop_inbox: Arc<dyn Fn() + Send + Sync>,
    pub wake: WakeFn,
}

const REPORT_TEXT_LIMIT: usize = 4_000;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Endpoints a process exit must still remove (senpi `process.once("exit", ...)`); the interactive
/// entry calls [`unregister_endpoints_at_exit`] on every exit path.
static EXIT_ENTRIES: Mutex<Vec<(u64, TuiRegistryEntry)>> = Mutex::new(Vec::new());
static NEXT_EXIT_ENTRY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Removes every endpoint this process still has registered, synchronously.
pub fn unregister_endpoints_at_exit() {
    let entries = std::mem::take(&mut *lock(&EXIT_ENTRIES));
    for (_, entry) in entries {
        unregister_tui_endpoint_sync(&entry);
    }
}

pub fn activate_control_endpoint(parts: ActivationParts) -> ActiveControlEndpoint {
    let ActivationParts {
        context,
        options: _,
        entry,
        server,
        scheduler,
        feed,
        stop_inbox,
        wake,
    } = parts;
    let session = context.session.clone();
    let surface = context.surface.clone();
    let events_feed = feed.clone();
    let events_wake = wake.clone();
    let observed = Arc::downgrade(&session);
    let subscription = session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
        let publish = |kind, data| lock(&events_feed).publish(kind, data);
        match event {
            AgentSessionEvent::Agent(AgentEvent::AgentStart) => {
                publish(ControlFeedKind::State, Map::new());
            }
            AgentSessionEvent::AgentIdle => {
                let mut data = Map::new();
                if let Some(session) = observed.upgrade() {
                    data.insert(
                        "turn_epoch".into(),
                        session.external_admission().turn_epoch().into(),
                    );
                }
                publish(ControlFeedKind::Completion, data);
                publish(ControlFeedKind::State, Map::new());
                tokio::spawn(events_wake("idle"));
            }
            AgentSessionEvent::AgentEnd { .. }
            | AgentSessionEvent::QueueUpdate { .. }
            | AgentSessionEvent::CompactionStart { .. }
            | AgentSessionEvent::CompactionEnd { .. } => {
                publish(ControlFeedKind::State, Map::new());
            }
            // senpi `externalAdmission.onEmitted`: an admitted delivery reached the transcript. mhc's
            // admission has no emitted listener; its delivery's `message_end` is the same edge.
            AgentSessionEvent::Agent(AgentEvent::MessageEnd { message })
                if message.role() == "custom"
                    && serde_json::to_value(message).is_ok_and(|value| {
                        maho_core::external_admission::delivery_id_of(&value).is_some()
                    }) =>
            {
                tokio::spawn(events_wake("emitted"));
            }
            AgentSessionEvent::Agent(AgentEvent::MessageEnd { message })
                if message.role() == "assistant" =>
            {
                let text = assistant_text(message);
                let mut data = Map::new();
                data.insert("text".into(), utf16_prefix(&text, REPORT_TEXT_LIMIT).into());
                publish(ControlFeedKind::Report, data);
            }
            _ => {}
        }
    }));
    // Deferred like the host's release: an accepted prompt marks its run active right after.
    let inputs_wake = wake.clone();
    let inputs_live = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let inputs_flag = inputs_live.clone();
    session
        .external_admission()
        .on_inputs_settled(Arc::new(move || {
            if !inputs_flag.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let wake = inputs_wake.clone();
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                wake("submission").await;
            });
        }));
    let continue_wake = wake.clone();
    let stop_continue = on_process_continue(Arc::new(move || {
        tokio::spawn(continue_wake("continue"));
    }));
    let exit_id = NEXT_EXIT_ENTRY.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    lock(&EXIT_ENTRIES).push((exit_id, entry.clone()));
    let teardown_feed = feed.clone();
    let teardown_entry = entry.clone();
    let teardown: BoxFuture<'static, ()> = Box::pin(async move {
        lock(&EXIT_ENTRIES).retain(|(id, _)| *id != exit_id);
        drop(subscription);
        inputs_live.store(false, std::sync::atomic::Ordering::SeqCst);
        stop_continue();
        stop_inbox();
        scheduler.dispose();
        lock(&teardown_feed).clear();
        server.close().await;
        // senpi's header-only rule removes a file this endpoint's header write created and nothing
        // else filled, unless the registrant says the session is referenced. OPEN: mhc's
        // `SessionManager` has no `discard_header_only_file` (nor the owned-header record it reads),
        // so the file is kept (what the rule does whenever it cannot answer), and the registrant's
        // `is_session_referenced` is not asked.
        unregister_tui_endpoint(&teardown_entry).await;
    });
    ActiveControlEndpoint {
        socket: entry.socket.clone(),
        wake,
        feed,
        surface,
        disposing: Arc::default(),
        teardown: Arc::new(Mutex::new(Some(teardown))),
    }
}

/// The composer's admission hold (senpi `editorHold`): `{revision}` or `{hold_reason, revision}`.
pub fn editor_hold(context: &TuiControlContext) -> Value {
    let revision = (context.editor_revision)();
    match context.surface.draft_hold() {
        Some(hold) => json!({"hold_reason": hold, "revision": revision}),
        None => json!({"revision": revision}),
    }
}

fn assistant_text(message: &AgentMessage) -> String {
    let AgentMessage::Llm(maho_ai::types::Message::Assistant(message)) = message else {
        return String::new();
    };
    message
        .content
        .iter()
        .filter_map(|part| match part {
            maho_ai::types::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// JavaScript `slice(0, limit)`: the first `limit` UTF-16 code units, never splitting a scalar.
fn utf16_prefix(text: &str, limit: usize) -> String {
    let mut units = 0;
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        if units + ch.len_utf16() > limit {
            break;
        }
        units += ch.len_utf16();
        end = index + ch.len_utf8();
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_are_cut_at_the_utf16_limit() {
        assert_eq!(utf16_prefix("abcdef", 4), "abcd");
        assert_eq!(utf16_prefix("a\u{1F600}b", 2), "a");
        assert_eq!(utf16_prefix("a\u{1F600}b", 3), "a\u{1F600}");
        assert_eq!(utf16_prefix("", 4), "");
    }
}
