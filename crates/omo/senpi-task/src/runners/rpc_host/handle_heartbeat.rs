//! `runners/rpc-host/handle-heartbeat.ts`: a hosted session's liveness state machine.
//!
//! Probe failure is uncertainty about ONE session, never evidence that its host or turn died.
//! Three consecutive failed probes publish `unresponsive`; after [`HOST_UNRESPONSIVE_DEADLINE_MS`]
//! the session goes `terminal-pending` and a confirmed close is retried every [`MAX_BACKOFF_MS`].
//! Any good probe or session event returns it to `running`. An `unknown_session` answer means the
//! session is gone.
//!
//! The timing decisions are kept in [`HostHeartbeatMachine`] against an explicit `now`, so the
//! driver (the handle's heartbeat step) owns the clock and tests step it deterministically.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::json;

use crate::runners::rpc::heartbeat::HEARTBEAT_DEADLINE_MS;
pub use crate::state::host_liveness::HostLivenessStatus;

pub const HOST_UNRESPONSIVE_DEADLINE_MS: i64 = 120_000;
pub const MAX_BACKOFF_MS: i64 = 30_000;
const UNRESPONSIVE_AFTER: u32 = 3;

/// What the driver must do after a machine step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatStep {
    Idle,
    /// Start one probe now (single-flight).
    Probe,
    /// The session is terminal-pending: attempt the confirmed close now.
    Close,
    /// `onGone(false)`: the host answered that the session is absent.
    Gone,
}

#[derive(Debug)]
pub struct HostHeartbeatMachine {
    interval_ms: i64,
    status: HostLivenessStatus,
    stopped: bool,
    failures: u32,
    probe_pending: bool,
    probe_missed: bool,
    probe_deadline: i64,
    next_probe_at: i64,
    unresponsive_deadline: Option<i64>,
    next_close_at: Option<i64>,
}

impl HostHeartbeatMachine {
    pub fn new(interval_ms: i64, now: i64) -> Self {
        Self {
            interval_ms,
            status: HostLivenessStatus::Running,
            stopped: false,
            failures: 0,
            probe_pending: false,
            probe_missed: false,
            probe_deadline: 0,
            next_probe_at: now + interval_ms,
            unresponsive_deadline: None,
            next_close_at: None,
        }
    }

    pub fn status(&self) -> HostLivenessStatus {
        self.status
    }

    pub fn stopped(&self) -> bool {
        self.stopped
    }

    fn terminal_pending(&self) -> bool {
        matches!(self.status, HostLivenessStatus::TerminalPending { .. })
    }

    fn retry_delay(&self) -> i64 {
        let doubled = self
            .interval_ms
            .saturating_mul(1_i64 << self.failures.min(5));
        doubled.min(MAX_BACKOFF_MS)
    }

    fn publish(&mut self, next: HostLivenessStatus, changes: &mut Vec<HostLivenessStatus>) {
        self.status = next;
        changes.push(next);
    }

    fn failed(&mut self, now: i64, changes: &mut Vec<HostLivenessStatus>) {
        if self.stopped || self.terminal_pending() {
            return;
        }
        self.failures += 1;
        if self.failures == UNRESPONSIVE_AFTER && self.status.is_running() {
            self.publish(HostLivenessStatus::Unresponsive { since: now }, changes);
            self.unresponsive_deadline = Some(now + HOST_UNRESPONSIVE_DEADLINE_MS);
        }
        self.next_probe_at = now
            + if self.status.is_running() {
                HEARTBEAT_DEADLINE_MS
            } else {
                self.retry_delay()
            };
    }

    /// Advance the clock: count an overdue probe, enter terminal-pending, and say what to do next.
    pub fn tick(&mut self, now: i64, changes: &mut Vec<HostLivenessStatus>) -> HeartbeatStep {
        if self.stopped {
            return HeartbeatStep::Idle;
        }
        if let (HostLivenessStatus::Unresponsive { since }, Some(deadline)) =
            (self.status, self.unresponsive_deadline)
            && now >= deadline
        {
            self.publish(HostLivenessStatus::TerminalPending { since }, changes);
            self.unresponsive_deadline = None;
            self.next_close_at = Some(now);
        }
        if self.terminal_pending() {
            // The close port resolves only after authoritative host absence; it is retried.
            return match self.next_close_at {
                Some(at) if now >= at => {
                    self.next_close_at = Some(now + MAX_BACKOFF_MS);
                    HeartbeatStep::Close
                }
                _ => HeartbeatStep::Idle,
            };
        }
        if self.probe_pending {
            if now >= self.probe_deadline {
                // The request stays single-flight; each overdue window counts one failure.
                self.probe_missed = true;
                self.failed(now, changes);
                self.probe_deadline = self.next_probe_at;
                return self.tick(now, changes);
            }
            return HeartbeatStep::Idle;
        }
        if now >= self.next_probe_at {
            self.probe_pending = true;
            self.probe_missed = false;
            self.probe_deadline = now + HEARTBEAT_DEADLINE_MS;
            return HeartbeatStep::Probe;
        }
        HeartbeatStep::Idle
    }

    pub fn answered(&mut self, now: i64, changes: &mut Vec<HostLivenessStatus>) {
        self.probe_pending = false;
        if self.stopped || self.terminal_pending() {
            return;
        }
        self.failures = 0;
        self.unresponsive_deadline = None;
        if !self.status.is_running() {
            self.publish(HostLivenessStatus::Running, changes);
        }
        self.next_probe_at = now + self.interval_ms;
    }

    /// A probe refusal; `missing` is `isHostSessionMissing`. A refusal after the probe was already
    /// counted as missed is the same silence, unless it says the session is gone.
    pub fn refused(
        &mut self,
        now: i64,
        missing: bool,
        changes: &mut Vec<HostLivenessStatus>,
    ) -> HeartbeatStep {
        self.probe_pending = false;
        if self.stopped || self.terminal_pending() {
            return HeartbeatStep::Idle;
        }
        if missing {
            self.stopped = true;
            return HeartbeatStep::Gone;
        }
        if !self.probe_missed {
            self.failed(now, changes);
        }
        HeartbeatStep::Idle
    }

    /// `activity()`: a session event is evidence of life.
    pub fn activity(&mut self, now: i64, changes: &mut Vec<HostLivenessStatus>) {
        if self.stopped || self.terminal_pending() {
            return;
        }
        self.failures = 0;
        self.unresponsive_deadline = None;
        if !self.status.is_running() {
            self.publish(HostLivenessStatus::Running, changes);
            self.next_probe_at = now
                + if self.probe_pending {
                    HEARTBEAT_DEADLINE_MS
                } else {
                    self.interval_ms
                };
        }
    }

    pub fn stop(&mut self) {
        self.stopped = true;
    }

    /// When the driver must step next.
    pub fn next_wake(&self) -> Option<i64> {
        if self.stopped {
            return None;
        }
        if self.terminal_pending() {
            return self.next_close_at;
        }
        let pending = if self.probe_pending {
            self.probe_deadline
        } else {
            self.next_probe_at
        };
        Some(
            self.unresponsive_deadline
                .map_or(pending, |deadline| deadline.min(pending)),
        )
    }
}

pub type LivenessListener = Arc<dyn Fn(HostLivenessStatus) + Send + Sync>;

/// `startHostHeartbeat`'s observable surface: the machine plus its subscribers.
pub struct HostHeartbeat {
    task_id: String,
    machine: Mutex<HostHeartbeatMachine>,
    listeners: Mutex<Vec<(u64, LivenessListener)>>,
    next_listener: std::sync::atomic::AtomicU64,
}

impl HostHeartbeat {
    pub fn new(task_id: String, interval_ms: i64, now: i64) -> Self {
        Self {
            task_id,
            machine: Mutex::new(HostHeartbeatMachine::new(interval_ms, now)),
            listeners: Mutex::new(Vec::new()),
            next_listener: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn status(&self) -> HostLivenessStatus {
        self.lock().status()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HostHeartbeatMachine> {
        self.machine.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs one machine step and publishes every status change it made, outside the lock.
    pub fn step<T>(
        &self,
        apply: impl FnOnce(&mut HostHeartbeatMachine, &mut Vec<HostLivenessStatus>) -> T,
    ) -> T {
        let mut changes = Vec::new();
        let result = apply(&mut self.lock(), &mut changes);
        for status in changes {
            self.notify(status);
        }
        result
    }

    fn notify(&self, status: HostLivenessStatus) {
        let listeners: Vec<LivenessListener> = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(status))).is_err()
            {
                utils::logger::log(
                    "senpi-task liveness observer failed",
                    Some(&json!({ "taskId": self.task_id })),
                );
            }
        }
    }

    /// `subscribe(listener)`: a non-running status is replayed to the new listener at once.
    pub fn subscribe(
        self: &Arc<Self>,
        listener: LivenessListener,
    ) -> crate::manager::child_handle::Unsubscribe {
        let id = self
            .next_listener
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((id, Arc::clone(&listener)));
        let status = self.status();
        if !status.is_running() {
            listener(status);
        }
        let weak = Arc::downgrade(self);
        Box::new(move || {
            if let Some(heartbeat) = weak.upgrade() {
                heartbeat
                    .listeners
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .retain(|(existing, _)| *existing != id);
            }
        })
    }

    pub fn stop(&self) {
        self.lock().stop();
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

#[cfg(test)]
#[path = "handle_heartbeat_tests.rs"]
mod tests;
