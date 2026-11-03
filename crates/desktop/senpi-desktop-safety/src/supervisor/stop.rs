//! Immediate refusal is separate from durable commit and publication.
//!
//! mhc base: upstream also releases registered hold signals and runs the
//! emergency release worker inside `commit_stop`; neither exists here yet, so
//! a commit is the authority callback plus publication.
use std::sync::atomic::Ordering;

use super::{StopSource, Supervisor, SupervisorStatus};

impl Supervisor {
    /// Callback-safe refusal: no lock, allocation, persistence or notification.
    /// The owner must schedule `complete_stop` outside its event callback.
    pub fn suspend_input(&self) {
        match &self.authority {
            Some(authority) => authority.request_stop(),
            None => self.suspended.store(true, Ordering::SeqCst),
        }
    }

    /// A new stop has refused input but cannot yet be reported as committed.
    #[must_use]
    pub fn stop_pending(&self) -> bool {
        match &self.authority {
            Some(authority) => {
                authority.generation() > authority.committed_through.load(Ordering::SeqCst)
            }
            None => self.is_suspended() && !self.published.load(Ordering::SeqCst),
        }
    }

    /// Wire-facing state. Input gates must use `status` or `is_suspended`.
    #[must_use]
    pub fn published_status(&self) -> SupervisorStatus {
        let mut status = self.status();
        status.suspended = self.published.load(Ordering::SeqCst);
        status
    }

    /// Latch suspension and commit the stop. The first source is kept until
    /// [`Self::reset`]. Never acknowledge an uncommitted stop.
    pub fn trigger_stop(&self, source: StopSource) {
        self.finish_stop(source);
    }

    fn finish_stop(&self, source: StopSource) {
        self.suspend_input();
        self.drain_stop(source);
    }

    /// Drain a callback's already-recorded stop without counting it twice.
    /// Legacy owners retain their original callback/trigger behavior.
    pub fn complete_stop(&self, source: StopSource) {
        if self.authority.is_some() {
            self.drain_stop(source);
        } else {
            self.finish_stop(source);
        }
    }

    fn drain_stop(&self, source: StopSource) {
        loop {
            let _commit = self.stop_commit_lock.lock();
            let requested = self
                .authority
                .as_ref()
                .map(|authority| authority.generation());
            if let (Some(authority), Some(requested)) = (&self.authority, requested) {
                if requested <= authority.committed_through.load(Ordering::SeqCst) {
                    return;
                }
            }
            if !self.commit_stop(source, requested) || self.authority.is_none() {
                return;
            }
        }
    }

    fn commit_stop(&self, source: StopSource, requested: Option<u64>) -> bool {
        if let (Some(authority), Some(requested)) = (&self.authority, requested) {
            (authority.commit)(requested);
        }
        let mut stopped_by = self.stopped_by.lock();
        self.stop_epoch.fetch_add(1, Ordering::SeqCst);
        stopped_by.get_or_insert(source);
        if let (Some(authority), Some(requested)) = (&self.authority, requested) {
            authority
                .committed_through
                .store(requested, Ordering::SeqCst);
        } else {
            self.suspended.store(true, Ordering::SeqCst);
        }
        self.published.store(true, Ordering::SeqCst);
        drop(stopped_by);
        true
    }
}
