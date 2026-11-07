//! Fail-closed kill-switch supervisor, ported from gajae-code
//! `pi-natives/src/computer/supervisor.rs` and generalized to a registry of
//! stop paths.
//!
//! Side-effecting input may fire only while [`Supervisor::input_allowed`]
//! holds: not suspended, and a live stop path with a fresh heartbeat - the
//! `Global` listener (OS hotkey), or the `HostRelay` alone when the policy
//! opts in (`computer.allowHostRelayOnlyStop`). A stop latches suspension
//! until a user-only [`Supervisor::reset`], which demands a [`UserReset`].
//! A stale heartbeat or a dead listener disables input without suspending.
//!
//! Time comes only from the injected [`Clock`].

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

use parking_lot::Mutex;

use crate::{clock::Clock, reset::UserReset};

pub(crate) mod authority;
mod status;
mod stop;

/// Max age of a stop path's heartbeat before it stops counting (ms).
pub const HEARTBEAT_FRESH_MS: u64 = 2_000;
/// Interval at which a live stop path beats (ms).
pub const HEARTBEAT_INTERVAL_MS: u64 = 500;

/// A registered stop path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StopPathId {
    /// The OS-level hotkey listener.
    Global,
    /// Stop requests relayed by the host process over JSON-RPC.
    HostRelay,
}

/// Who latched suspension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSource {
    Hotkey,
    HostRelay,
    Api,
}

/// The stop path input currently relies on, as reported in `stopPath`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveStopPath {
    Global,
    HostRelay,
    None,
}

impl ActiveStopPath {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::HostRelay => "host-relay",
            Self::None => "none",
        }
    }
}

/// Which stop paths may authorize input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StopPolicy {
    /// `computer.allowHostRelayOnlyStop`: accept the host relay without a
    /// live global listener.
    pub allow_host_relay_only: bool,
}

/// Why the global stop-chord listener failed to start; the gate turns
/// `AccessibilityDenied` into a permission error instead of a missing stop path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopPathFailure {
    Unavailable,
    AccessibilityDenied,
}

/// Snapshot of supervisor state used for gating and status reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorStatus {
    /// Input is latched off until a user-only reset.
    pub suspended: bool,
    pub global_live: bool,
    pub host_relay_live: bool,
    /// The heartbeat of [`Self::stop_path`] is within [`HEARTBEAT_FRESH_MS`].
    pub heartbeat_fresh: bool,
    /// The first live and fresh path (`Global` before `HostRelay`), else the
    /// first live one, else `None`.
    pub stop_path: ActiveStopPath,
    /// The source of the stop that latched suspension.
    pub stopped_by: Option<StopSource>,
    pub global_failure: Option<StopPathFailure>,
}

impl SupervisorStatus {
    /// Whether side-effecting input may fire under `policy`.
    #[must_use]
    pub const fn input_allowed(self, policy: &StopPolicy) -> bool {
        if self.suspended || !self.heartbeat_fresh {
            return false;
        }
        match self.stop_path {
            ActiveStopPath::Global => true,
            ActiveStopPath::HostRelay => policy.allow_host_relay_only,
            ActiveStopPath::None => false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct StopPathState {
    live: bool,
    last_heartbeat_ms: u64,
}

impl StopPathState {
    const fn fresh_at(self, now_ms: u64) -> bool {
        self.live && now_ms.saturating_sub(self.last_heartbeat_ms) <= HEARTBEAT_FRESH_MS
    }
}

/// Kill-switch state shared by the session loop and every stop path.
pub struct Supervisor {
    #[cfg(test)]
    activation_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    suspended: AtomicBool,
    authority: Option<Arc<authority::AuthorityAdmission>>,
    /// Whether the latest stop is committed and may be reported.
    published: AtomicBool,
    stop_commit_lock: Mutex<()>,
    /// Bumped by every stop, so a control grant granted before one is dead.
    stop_epoch: AtomicU64,
    /// Guards every write to `suspended` so the flag and its source agree.
    stopped_by: Mutex<Option<StopSource>>,
    stop_paths: Mutex<HashMap<StopPathId, StopPathState>>,
    global_failure: Mutex<Option<StopPathFailure>>,
    clock: Arc<dyn Clock>,
}

impl Supervisor {
    /// Not suspended, no stop path live: input is refused until one registers.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            #[cfg(test)]
            activation_hook: Mutex::new(None),
            suspended: AtomicBool::new(false),
            authority: None,
            published: AtomicBool::new(false),
            stop_commit_lock: Mutex::new(()),
            stop_epoch: AtomicU64::new(0),
            stopped_by: Mutex::new(None),
            stop_paths: Mutex::new(HashMap::new()),
            global_failure: Mutex::new(None),
            clock,
        }
    }

    /// The latest global listener start result; success clears a previous failure.
    pub fn note_global_failure(&self, failure: Option<StopPathFailure>) {
        *self.global_failure.lock() = failure;
    }

    /// Record that a stop path is live (or not); going live counts as a beat.
    pub fn set_live(&self, id: StopPathId, live: bool) {
        let now_ms = self.clock.now_ms();
        let mut paths = self.stop_paths.lock();
        let state = paths.entry(id).or_default();
        state.live = live;
        if live {
            state.last_heartbeat_ms = now_ms;
        }
    }

    /// Heartbeat from a stop path, due every [`HEARTBEAT_INTERVAL_MS`].
    pub fn heartbeat(&self, id: StopPathId) {
        let now_ms = self.clock.now_ms();
        self.stop_paths
            .lock()
            .entry(id)
            .or_default()
            .last_heartbeat_ms = now_ms;
    }

    /// Invalidation epoch: stops and authority activation invalidate old holds
    /// and grants. This is NOT the immediate authority request generation.
    #[must_use]
    pub fn stop_epoch(&self) -> u64 {
        self.stop_epoch.load(Ordering::SeqCst)
    }

    /// Lift suspension. User-only: the proof comes from the host's resume token.
    pub fn reset(&self, _proof: UserReset) {
        if self.authority.is_some() {
            return;
        }
        let _commit = self.stop_commit_lock.lock();
        let mut stopped_by = self.stopped_by.lock();
        *stopped_by = None;
        self.published.store(false, Ordering::SeqCst);
        self.suspended.store(false, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_suspended(&self) -> bool {
        match &self.authority {
            Some(authority) => authority.is_suspended(),
            None => self.suspended.load(Ordering::SeqCst),
        }
    }

    /// Lifts suspension without a [`UserReset`], for tests only. Authority
    /// mode ignores it: only [`Self::activate_authority`] can lift that.
    #[cfg(test)]
    pub(crate) fn reset_for_test(&self) {
        if self.authority.is_some() {
            return;
        }
        let _commit = self.stop_commit_lock.lock();
        let mut stopped_by = self.stopped_by.lock();
        *stopped_by = None;
        self.published.store(false, Ordering::SeqCst);
        self.suspended.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "supervisor/authority_race_tests.rs"]
mod authority_race_tests;

#[cfg(test)]
#[path = "supervisor/authority_tests.rs"]
mod authority_tests;

#[cfg(test)]
#[path = "supervisor/authority_test_support.rs"]
mod authority_test_support;

#[cfg(test)]
#[path = "supervisor/authority_schedule_tests.rs"]
mod authority_schedule_tests;
