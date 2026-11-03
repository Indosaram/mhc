//! Per-session arming state.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

/// What the classifier needs to know about a session.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionArming {
    pub armed: bool,
    pub compaction_pending: bool,
    /// Contract revision injected into the session, when one was.
    pub revision: Option<u64>,
}

/// A directive waiting for the next agent turn to carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingDirective {
    pub full: bool,
    pub mass: bool,
}

#[derive(Default)]
struct Entry {
    arming: SessionArming,
    todo_reminded: bool,
    pending: Option<PendingDirective>,
}

/// Arming state of every session this process has seen, plus the active one.
#[derive(Default)]
pub struct Sessions {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    active: Option<String>,
}

impl Sessions {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub fn snapshot(&self, session: &str) -> SessionArming {
        self.lock()
            .entries
            .get(session)
            .map(|entry| entry.arming)
            .unwrap_or_default()
    }

    /// Marks the session armed at the current contract revision and clears a pending re-arm.
    pub fn mark_armed(&self, session: &str) {
        self.mark_armed_at(session, crate::contract_revision());
    }

    pub fn mark_armed_at(&self, session: &str, revision: u64) {
        let mut inner = self.lock();
        inner.active = Some(session.to_owned());
        let entry = inner.entries.entry(session.to_owned()).or_default();
        entry.arming = SessionArming {
            armed: true,
            compaction_pending: false,
            revision: Some(revision),
        };
    }

    /// Compaction dropped the contract from the conversation: disarm and reset the todo latch.
    pub fn mark_compact_rearm(&self, session: &str) {
        let mut inner = self.lock();
        inner.active = Some(session.to_owned());
        let entry = inner.entries.entry(session.to_owned()).or_default();
        entry.arming.armed = false;
        entry.arming.compaction_pending = true;
        entry.todo_reminded = false;
    }

    pub fn active_session(&self) -> Option<String> {
        self.lock().active.clone()
    }

    pub(crate) fn set_pending(&self, session: &str, next: PendingDirective) {
        let mut inner = self.lock();
        let entry = inner.entries.entry(session.to_owned()).or_default();
        entry.pending = Some(match entry.pending {
            Some(previous) => PendingDirective {
                full: previous.full || next.full,
                mass: previous.mass || next.mass,
            },
            None => next,
        });
    }

    pub(crate) fn pending(&self, session: &str) -> Option<PendingDirective> {
        self.lock()
            .entries
            .get(session)
            .and_then(|entry| entry.pending)
    }

    pub(crate) fn take_pending(&self, session: &str) -> Option<PendingDirective> {
        self.lock()
            .entries
            .get_mut(session)
            .and_then(|entry| entry.pending.take())
    }

    /// Latches the todo reminder; true only for the call that set the latch.
    pub(crate) fn latch_todo_reminder(&self, session: &str) -> bool {
        let mut inner = self.lock();
        let entry = inner.entries.entry(session.to_owned()).or_default();
        !std::mem::replace(&mut entry.todo_reminded, true)
    }
}
