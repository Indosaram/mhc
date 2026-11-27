//! Port of senpi `packages/coding-agent/src/modes/rpc/session-event-writer-lifecycle.ts`.
//!
//! Session publication, sealing, forgetting, and status-lifecycle helpers.

use crate::session_event_writer_state::SessionEventWriterState;

impl SessionEventWriterState {
    pub fn mark_session_sealed(&mut self, session_id: &str) {
        self.sealed_sessions.insert(session_id.to_owned());
    }

    pub fn is_session_sealed(&self, session_id: &str) -> bool {
        self.sealed_sessions.contains(session_id)
    }

    pub fn set_worker_session(&mut self, session_id: &str, is_worker: bool) {
        if is_worker {
            self.worker_sessions.insert(session_id.to_owned());
        } else {
            self.worker_sessions.remove(session_id);
        }
    }

    pub fn forget_session(&mut self, session_id: &str) {
        self.sealed_sessions.remove(session_id);
        self.worker_sessions.remove(session_id);
        self.open_turns.clear_session(session_id);
        self.surface_dedupe.forget_session(session_id);
    }
}
