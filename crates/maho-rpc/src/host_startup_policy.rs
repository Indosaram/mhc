//! The startup readiness policy of a spawned RPC host (senpi#4602, design A-amended): one controller
//! per start attempt decides, on its own monotonic clock, whether the attempt is READY or has failed.
//!
//! Each phase gets a finite allowance from the moment it is entered; every deadline is clipped to
//! `STARTUP_DECISION_BUDGET_MS` after spawn. Only the completion of the current phase grants the next
//! phase its allowance: a replayed milestone, a heartbeat or a same-phase event adds no time. The
//! clock is sampled at every ingress, so an event that arrives at or after the deadline loses to it.

use crate::host_ensure_stop::SIGKILL_GRACE_MS;
use crate::host_stalled_evidence::DEFAULT_CHILD_STALLED_STOP_MAX_MS;
use crate::host_stop_intent::{CHILD_KILL_EXIT_TIMEOUT_MS, CHILD_STOP_TIMEOUT_MS};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::watch;

/// G: spawn to ready-or-failed decision.
pub const STARTUP_DECISION_BUDGET_MS: u64 = 180_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupPhase {
    SupervisorBootstrap,
    SupervisorSetup,
    HostBootstrap,
    HostRuntime,
    OpaqueHost,
    InternalReady,
    ObserverReady,
    PublicPublished,
    PresenterResolved,
    PublicVerified,
}

impl StartupPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SupervisorBootstrap => "supervisor_bootstrap",
            Self::SupervisorSetup => "supervisor_setup",
            Self::HostBootstrap => "host_bootstrap",
            Self::HostRuntime => "host_runtime",
            Self::OpaqueHost => "opaque_host",
            Self::InternalReady => "internal_ready",
            Self::ObserverReady => "observer_ready",
            Self::PublicPublished => "public_published",
            Self::PresenterResolved => "presenter_resolved",
            Self::PublicVerified => "public_verified",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "supervisor_bootstrap" => Some(Self::SupervisorBootstrap),
            "supervisor_setup" => Some(Self::SupervisorSetup),
            "host_bootstrap" => Some(Self::HostBootstrap),
            "host_runtime" => Some(Self::HostRuntime),
            "opaque_host" => Some(Self::OpaqueHost),
            "internal_ready" => Some(Self::InternalReady),
            "observer_ready" => Some(Self::ObserverReady),
            "public_published" => Some(Self::PublicPublished),
            "presenter_resolved" => Some(Self::PresenterResolved),
            "public_verified" => Some(Self::PublicVerified),
            _ => None,
        }
    }

    pub const fn allowance_ms(self) -> u64 {
        match self {
            Self::SupervisorBootstrap => 30_000,
            Self::SupervisorSetup => 15_000,
            Self::HostBootstrap => 30_000,
            Self::HostRuntime => 60_000,
            // A custom host-child command that cannot report progress: one phase from its spawn,
            // completed only by the authenticated internal reply, replacing host_bootstrap + host_runtime.
            Self::OpaqueHost => 90_000,
            Self::InternalReady => 15_000,
            Self::ObserverReady => 15_000,
            Self::PublicPublished => 15_000,
            Self::PresenterResolved => 30_000,
            Self::PublicVerified => 10_000,
        }
    }
}

/// `Legacy`: a supervisor command that is not known to report progress. It gets one public-ready
/// phase of the whole decision budget from spawn, with no renewals, and keeps every inner limit of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupLaunchKind {
    Initial,
    Successor,
    Legacy,
}

const SHARED_PREFIX: [StartupPhase; 6] = [
    StartupPhase::SupervisorBootstrap,
    StartupPhase::SupervisorSetup,
    StartupPhase::HostBootstrap,
    StartupPhase::HostRuntime,
    StartupPhase::InternalReady,
    StartupPhase::ObserverReady,
];

fn initial_path(launch_kind: StartupLaunchKind) -> Vec<StartupPhase> {
    match launch_kind {
        StartupLaunchKind::Initial => {
            let mut p = SHARED_PREFIX.to_vec();
            p.extend([
                StartupPhase::PublicPublished,
                StartupPhase::PresenterResolved,
                StartupPhase::PublicVerified,
            ]);
            p
        }
        StartupLaunchKind::Successor => {
            let mut p = SHARED_PREFIX.to_vec();
            p.extend([
                StartupPhase::PresenterResolved,
                StartupPhase::PublicPublished,
                StartupPhase::PublicVerified,
            ]);
            p
        }
        StartupLaunchKind::Legacy => vec![StartupPhase::PublicVerified],
    }
}

const OPAQUE_REPLACES: [StartupPhase; 3] = [
    StartupPhase::HostBootstrap,
    StartupPhase::HostRuntime,
    StartupPhase::InternalReady,
];

/// Opaque cancellation handle returned by [`StartupClock::schedule_at`].
pub struct StartupTimerHandle {
    cancel_fn: Option<Box<dyn FnOnce() + Send>>,
}

impl StartupTimerHandle {
    pub fn new(cancel: impl FnOnce() + Send + 'static) -> Self {
        Self {
            cancel_fn: Some(Box::new(cancel)),
        }
    }

    pub fn noop() -> Self {
        Self { cancel_fn: None }
    }

    pub fn cancel(mut self) {
        if let Some(cancel) = self.cancel_fn.take() {
            cancel();
        }
    }
}

pub trait StartupClock: Send + Sync {
    fn now_ms(&self) -> u64;
    fn schedule_at(&self, at_ms: u64, fire: Box<dyn FnOnce() + Send>) -> StartupTimerHandle;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutLimit {
    Phase,
    Overall,
}

impl TimeoutLimit {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Phase => "phase",
            Self::Overall => "overall",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupTimeout {
    pub phase: StartupPhase,
    pub limit: TimeoutLimit,
    pub phase_budget_ms: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupOutcome {
    Ready {
        elapsed_ms: u64,
    },
    Timeout(StartupTimeout),
    Failed {
        phase: StartupPhase,
        cause: String,
        elapsed_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompleteResult {
    Accepted,
    Ignored,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpaqueHostResult {
    Accepted,
    Rejected,
}

struct ControllerState {
    launch_kind: StartupLaunchKind,
    budget_ms: u64,
    path: Vec<StartupPhase>,
    t0: u64,
    ceiling: u64,
    index: usize,
    phase_limit: u64,
    deadline: u64,
    timer: Option<StartupTimerHandle>,
    terminal: Option<StartupOutcome>,
}

impl ControllerState {
    fn current(&self) -> StartupPhase {
        let idx = self.index.min(self.path.len().saturating_sub(1));
        self.path
            .get(idx)
            .copied()
            .unwrap_or(StartupPhase::SupervisorBootstrap)
    }

    fn allowance(&self, phase: StartupPhase) -> u64 {
        if self.launch_kind == StartupLaunchKind::Legacy {
            self.budget_ms
        } else {
            phase.allowance_ms()
        }
    }
}

pub struct StartupController {
    clock: Arc<dyn StartupClock>,
    state: Mutex<ControllerState>,
    rev_tx: watch::Sender<u64>,
}

impl StartupController {
    /// Create the controller immediately BEFORE spawning: its creation instant is the spawn origin.
    pub fn start(
        launch_kind: StartupLaunchKind,
        clock: Arc<dyn StartupClock>,
        decision_budget_ms: Option<u64>,
    ) -> Arc<Self> {
        let budget_ms = decision_budget_ms.unwrap_or(STARTUP_DECISION_BUDGET_MS);
        let t0 = clock.now_ms();
        let ceiling = t0.saturating_add(budget_ms);
        let (rev_tx, _) = watch::channel(0u64);
        let controller = Arc::new(Self {
            clock,
            state: Mutex::new(ControllerState {
                launch_kind,
                budget_ms,
                path: initial_path(launch_kind),
                t0,
                ceiling,
                index: 0,
                phase_limit: 0,
                deadline: 0,
                timer: None,
                terminal: None,
            }),
            rev_tx,
        });
        Self::enter_phase(&controller, t0);
        controller
    }

    fn notify(&self) {
        self.rev_tx.send_modify(|rev| *rev = rev.wrapping_add(1));
    }

    fn enter_phase(controller: &Arc<Self>, at: u64) {
        let (deadline, timer) = {
            let guard = controller
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if guard.terminal.is_some() {
                return;
            }
            let current = guard.current();
            let phase_limit = at.saturating_add(guard.allowance(current));
            let deadline = phase_limit.min(guard.ceiling);
            drop(guard);

            let weak = Arc::downgrade(controller);
            let timer = controller.clock.schedule_at(
                deadline,
                Box::new(move || {
                    if let Some(ctrl) = weak.upgrade() {
                        ctrl.expire();
                    }
                }),
            );
            let mut guard = controller
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if guard.terminal.is_some() {
                drop(guard);
                timer.cancel();
                return;
            }
            guard.phase_limit = phase_limit;
            guard.deadline = deadline;
            let prev = guard.timer.replace(timer);
            if let Some(prev) = prev {
                prev.cancel();
            }
            (deadline, ())
        };
        let _ = (deadline, timer);
        controller.notify();
    }

    fn expire(&self) {
        let changed = {
            let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.terminal.is_some() {
                false
            } else {
                let phase = guard.current();
                let limit = if guard.phase_limit < guard.ceiling {
                    TimeoutLimit::Phase
                } else {
                    TimeoutLimit::Overall
                };
                let phase_budget_ms = guard.allowance(phase);
                let elapsed_ms = guard.deadline.saturating_sub(guard.t0);
                guard.terminal = Some(StartupOutcome::Timeout(StartupTimeout {
                    phase,
                    limit,
                    phase_budget_ms,
                    elapsed_ms,
                }));
                if let Some(timer) = guard.timer.take() {
                    timer.cancel();
                }
                true
            }
        };
        if changed {
            self.notify();
        }
    }

    fn expired_at(guard: &mut ControllerState, now: u64) -> bool {
        if guard.terminal.is_none() && now >= guard.deadline {
            let phase = guard.current();
            let limit = if guard.phase_limit < guard.ceiling {
                TimeoutLimit::Phase
            } else {
                TimeoutLimit::Overall
            };
            let phase_budget_ms = guard.allowance(phase);
            let elapsed_ms = guard.deadline.saturating_sub(guard.t0);
            guard.terminal = Some(StartupOutcome::Timeout(StartupTimeout {
                phase,
                limit,
                phase_budget_ms,
                elapsed_ms,
            }));
            if let Some(timer) = guard.timer.take() {
                timer.cancel();
            }
            return true;
        }
        matches!(
            guard.terminal,
            Some(StartupOutcome::Timeout(_) | StartupOutcome::Failed { .. })
        )
    }

    /// `Ignored`: a replay of a completed phase; `Rejected`: terminal, expired, or not the current phase.
    pub fn complete(self: &Arc<Self>, requested: StartupPhase) -> CompleteResult {
        let now = self.clock.now_ms();
        let outcome = {
            let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let was_terminal = guard.terminal.is_some();
            if Self::expired_at(&mut guard, now) {
                drop(guard);
                if !was_terminal {
                    self.notify();
                }
                return CompleteResult::Rejected;
            }
            let phase = if requested == StartupPhase::InternalReady
                && guard.current() == StartupPhase::OpaqueHost
            {
                StartupPhase::OpaqueHost
            } else {
                requested
            };
            let position = guard.path.iter().position(|p| *p == phase);
            match position {
                Some(pos) if pos < guard.index => return CompleteResult::Ignored,
                Some(pos) if pos == guard.index && guard.terminal.is_none() => {}
                _ => return CompleteResult::Rejected,
            }
            if let Some(timer) = guard.timer.take() {
                timer.cancel();
            }
            guard.index += 1;
            if guard.index == guard.path.len() {
                guard.terminal = Some(StartupOutcome::Ready {
                    elapsed_ms: now.saturating_sub(guard.t0),
                });
                Some(true)
            } else {
                Some(false)
            }
        };
        match outcome {
            Some(true) => self.notify(),
            Some(false) => Self::enter_phase(self, now),
            None => {}
        }
        CompleteResult::Accepted
    }

    /// The supervisor declares, before it spawns its host child, that the child cannot report progress.
    /// Only an initial launch in `SupervisorSetup` may switch; `OpaqueHost` then replaces the child's phases.
    pub fn declare_opaque_host(&self) -> OpaqueHostResult {
        let now = self.clock.now_ms();
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let was_terminal = guard.terminal.is_some();
        if Self::expired_at(&mut guard, now) {
            drop(guard);
            if !was_terminal {
                self.notify();
            }
            return OpaqueHostResult::Rejected;
        }
        if guard.launch_kind != StartupLaunchKind::Initial
            || guard.current() != StartupPhase::SupervisorSetup
        {
            return OpaqueHostResult::Rejected;
        }
        let split = guard.index + 1;
        let before = guard.path[..split].to_vec();
        let after: Vec<StartupPhase> = guard.path[split..]
            .iter()
            .copied()
            .filter(|phase| !OPAQUE_REPLACES.contains(phase))
            .collect();
        let mut new_path = before;
        new_path.push(StartupPhase::OpaqueHost);
        new_path.extend(after);
        guard.path = new_path;
        OpaqueHostResult::Accepted
    }

    pub fn fail(&self, cause: impl Into<String>) {
        let now = self.clock.now_ms();
        let cause = cause.into();
        let changed = {
            let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let was_terminal = guard.terminal.is_some();
            if Self::expired_at(&mut guard, now) {
                !was_terminal
            } else {
                let phase = guard.current();
                guard.terminal = Some(StartupOutcome::Failed {
                    phase,
                    cause,
                    elapsed_ms: now.saturating_sub(guard.t0),
                });
                if let Some(timer) = guard.timer.take() {
                    timer.cancel();
                }
                true
            }
        };
        if changed {
            self.notify();
        }
    }

    pub fn terminal(&self) -> Option<StartupOutcome> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .terminal
            .clone()
    }

    pub fn phase(&self) -> StartupPhase {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .current()
    }

    /// Milliseconds left before the current phase's deadline; 0 once terminal.
    pub fn remaining_ms(&self) -> u64 {
        let guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.terminal.is_some() {
            0
        } else {
            guard.deadline.saturating_sub(self.clock.now_ms())
        }
    }

    /// Resolves `true` once `phase` is the current phase, `false` once the attempt is terminal first.
    pub async fn when_phase(&self, phase: StartupPhase) -> bool {
        let mut rx = self.rev_tx.subscribe();
        loop {
            rx.mark_unchanged();
            {
                let guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                if guard.terminal.is_some() {
                    return false;
                }
                if guard.current() == phase {
                    return true;
                }
            }
            if rx.changed().await.is_err() {
                return false;
            }
        }
    }

    /// Resolves with the terminal outcome.
    pub async fn settled(&self) -> StartupOutcome {
        let mut rx = self.rev_tx.subscribe();
        loop {
            rx.mark_unchanged();
            {
                let guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(outcome) = &guard.terminal {
                    return outcome.clone();
                }
            }
            if rx.changed().await.is_err() {
                return StartupOutcome::Failed {
                    phase: self.phase(),
                    cause: "startup controller channel closed".to_owned(),
                    elapsed_ms: 0,
                };
            }
        }
    }
}

pub fn start_startup_controller(
    launch_kind: StartupLaunchKind,
    clock: Arc<dyn StartupClock>,
    decision_budget_ms: Option<u64>,
) -> Arc<StartupController> {
    StartupController::start(launch_kind, clock, decision_budget_ms)
}

/// The fixed reap/record margin of the existing stop path, beside its SIGTERM, kill-exit and SIGKILL waits.
const STOP_REAP_MARGIN_MS: u64 = 5_000;
/// Probe disposal and guarded record bookkeeping after a failed start.
const FAILED_START_FINALIZE_MS: u64 = 2_000 + 5_000;
/// D: one pooled allocation for discovery, ownership observations, legacy reads and pre-spawn writes.
const DISCOVERY_BUDGET_MS: u64 = 90_000;
const LOCK_WAIT_HEADROOM_MS: u64 = 10_000;
const HANDOVER_REPLY_MARGIN_MS: u64 = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupEnvelope {
    pub decision: u64,
    pub stop: u64,
    pub cleanup: u64,
    pub settled: u64,
    pub discovery: u64,
    pub holder: u64,
    pub lock_wait: u64,
    pub handover_reply: u64,
}

/// The numerical envelope every startup-adjacent wait derives from, for one resolved child-stall setting
/// (`SENPI_RPC_CHILD_STALLED_STOP_MAX_MS`, default 60 s).
pub const fn startup_envelope(stall_ms: Option<u64>) -> StartupEnvelope {
    let stall = match stall_ms {
        Some(ms) => ms,
        None => DEFAULT_CHILD_STALLED_STOP_MAX_MS,
    };
    let decision = STARTUP_DECISION_BUDGET_MS;
    let stop_term = if CHILD_STOP_TIMEOUT_MS > stall {
        CHILD_STOP_TIMEOUT_MS
    } else {
        stall
    };
    let stop = stop_term + CHILD_KILL_EXIT_TIMEOUT_MS + STOP_REAP_MARGIN_MS + SIGKILL_GRACE_MS;
    let cleanup = stop + FAILED_START_FINALIZE_MS;
    let settled = decision + cleanup;
    let holder = DISCOVERY_BUDGET_MS + stop + settled;
    StartupEnvelope {
        decision,
        stop,
        cleanup,
        settled,
        discovery: DISCOVERY_BUDGET_MS,
        holder,
        lock_wait: holder + LOCK_WAIT_HEADROOM_MS,
        handover_reply: 2 * holder + HANDOVER_REPLY_MARGIN_MS,
    }
}

/// The ensure lock was not acquired within W: no startup was attempted by this caller, and nothing was signalled.
pub fn ensure_lock_wait_detail(lock_wait_ms: u64) -> String {
    format!(
        "RPC host ensure lock wait exceeded {lock_wait_ms}ms; startup not started by this caller"
    )
}

pub fn startup_timeout_detail(outcome: &StartupTimeout, cleanup: &str) -> String {
    format!(
        "RPC host startup timed out: phase={}; limit={}; phase_budget_ms={}; startup_budget_ms={}; elapsed_ms={}; cleanup={cleanup}",
        outcome.phase.as_str(),
        outcome.limit.as_str(),
        outcome.phase_budget_ms,
        STARTUP_DECISION_BUDGET_MS,
        outcome.elapsed_ms,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    type VirtualTimerEntry = (u64, u64, bool, Box<dyn FnOnce() + Send>);
    type CancellableTimerEntry = (u64, u64, Box<dyn FnOnce() + Send>);

    pub(crate) struct VirtualClock {
        now: AtomicU64,
        timers: Mutex<Vec<VirtualTimerEntry>>,
        next_id: AtomicU64,
    }

    impl VirtualClock {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {
                now: AtomicU64::new(0),
                timers: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
            })
        }

        #[allow(dead_code)]
        pub(crate) fn advance_to(&self, target: u64) {
            self.now.store(target, Ordering::SeqCst);
            loop {
                let next = {
                    let mut timers = self.timers.lock().unwrap_or_else(PoisonError::into_inner);
                    let pos = timers
                        .iter()
                        .enumerate()
                        .filter(|(_, (at, _, active, _))| *active && *at <= target)
                        .min_by_key(|(_, (at, id, _, _))| (*at, *id))
                        .map(|(i, _)| i);
                    pos.map(|i| timers.swap_remove(i))
                };
                match next {
                    Some((_, _, true, fire)) => fire(),
                    _ => break,
                }
            }
        }
    }

    impl StartupClock for VirtualClock {
        fn now_ms(&self) -> u64 {
            self.now.load(Ordering::SeqCst)
        }

        fn schedule_at(&self, at_ms: u64, fire: Box<dyn FnOnce() + Send>) -> StartupTimerHandle {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            let timers_ref = &self.timers;
            timers_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((at_ms, id, true, fire));
            // Use a weak/no-op cancel or mark inactive via shared list if needed.
            StartupTimerHandle::noop()
        }
    }

    struct CancellableVirtualClock {
        now: AtomicU64,
        timers: Arc<Mutex<Vec<CancellableTimerEntry>>>,
        next_id: AtomicU64,
    }

    impl CancellableVirtualClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                now: AtomicU64::new(0),
                timers: Arc::new(Mutex::new(Vec::new())),
                next_id: AtomicU64::new(1),
            })
        }

        fn advance_to(&self, target: u64) {
            self.now.store(target, Ordering::SeqCst);
            loop {
                let next = {
                    let mut timers = self.timers.lock().unwrap_or_else(PoisonError::into_inner);
                    let pos = timers
                        .iter()
                        .enumerate()
                        .filter(|(_, (at, _, _))| *at <= target)
                        .min_by_key(|(_, (at, id, _))| (*at, *id))
                        .map(|(i, _)| i);
                    pos.map(|i| timers.swap_remove(i))
                };
                match next {
                    Some((_, _, fire)) => fire(),
                    None => break,
                }
            }
        }
    }

    impl StartupClock for CancellableVirtualClock {
        fn now_ms(&self) -> u64 {
            self.now.load(Ordering::SeqCst)
        }

        fn schedule_at(&self, at_ms: u64, fire: Box<dyn FnOnce() + Send>) -> StartupTimerHandle {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            self.timers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((at_ms, id, fire));
            let timers = self.timers.clone();
            StartupTimerHandle::new(move || {
                timers
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .retain(|(_, entry_id, _)| *entry_id != id);
            })
        }
    }

    #[test]
    fn initial_launch_advances_through_all_phases_to_ready() {
        let clock = CancellableVirtualClock::new();
        let ctrl = start_startup_controller(StartupLaunchKind::Initial, clock.clone(), None);
        assert_eq!(ctrl.phase(), StartupPhase::SupervisorBootstrap);
        let phases = [
            StartupPhase::SupervisorBootstrap,
            StartupPhase::SupervisorSetup,
            StartupPhase::HostBootstrap,
            StartupPhase::HostRuntime,
            StartupPhase::InternalReady,
            StartupPhase::ObserverReady,
            StartupPhase::PublicPublished,
            StartupPhase::PresenterResolved,
            StartupPhase::PublicVerified,
        ];
        for phase in phases {
            assert_eq!(ctrl.complete(phase), CompleteResult::Accepted);
            assert_eq!(ctrl.complete(phase), CompleteResult::Ignored);
        }
        assert_eq!(
            ctrl.terminal(),
            Some(StartupOutcome::Ready { elapsed_ms: 0 })
        );
    }

    #[test]
    fn expiry_at_deadline_beats_late_ingress_and_clips_to_overall_ceiling() {
        let clock = CancellableVirtualClock::new();
        let ctrl =
            start_startup_controller(StartupLaunchKind::Initial, clock.clone(), Some(40_000));
        // supervisor_bootstrap has 30s allowance -> phase limit
        clock.advance_to(30_000);
        assert_eq!(
            ctrl.complete(StartupPhase::SupervisorBootstrap),
            CompleteResult::Rejected
        );
        assert_eq!(
            ctrl.terminal(),
            Some(StartupOutcome::Timeout(StartupTimeout {
                phase: StartupPhase::SupervisorBootstrap,
                limit: TimeoutLimit::Phase,
                phase_budget_ms: 30_000,
                elapsed_ms: 30_000,
            }))
        );

        let clock2 = CancellableVirtualClock::new();
        let ctrl2 =
            start_startup_controller(StartupLaunchKind::Initial, clock2.clone(), Some(40_000));
        clock2.advance_to(25_000);
        assert_eq!(
            ctrl2.complete(StartupPhase::SupervisorBootstrap),
            CompleteResult::Accepted
        );
        clock2.advance_to(35_000);
        assert_eq!(
            ctrl2.complete(StartupPhase::SupervisorSetup),
            CompleteResult::Accepted
        );
        // Remaining overall budget is 5_000ms (< 30_000ms phase budget for host_bootstrap)
        assert_eq!(ctrl2.remaining_ms(), 5_000);
        clock2.advance_to(40_000);
        assert_eq!(
            ctrl2.terminal(),
            Some(StartupOutcome::Timeout(StartupTimeout {
                phase: StartupPhase::HostBootstrap,
                limit: TimeoutLimit::Overall,
                phase_budget_ms: 30_000,
                elapsed_ms: 40_000,
            }))
        );
    }

    #[test]
    fn opaque_host_replaces_child_phases_and_maps_internal_ready() {
        let clock = CancellableVirtualClock::new();
        let ctrl = start_startup_controller(StartupLaunchKind::Initial, clock.clone(), None);
        assert_eq!(ctrl.declare_opaque_host(), OpaqueHostResult::Rejected);
        assert_eq!(
            ctrl.complete(StartupPhase::SupervisorBootstrap),
            CompleteResult::Accepted
        );
        assert_eq!(ctrl.declare_opaque_host(), OpaqueHostResult::Accepted);
        assert_eq!(
            ctrl.complete(StartupPhase::SupervisorSetup),
            CompleteResult::Accepted
        );
        assert_eq!(ctrl.phase(), StartupPhase::OpaqueHost);
        assert_eq!(
            ctrl.complete(StartupPhase::InternalReady),
            CompleteResult::Accepted
        );
        assert_eq!(ctrl.phase(), StartupPhase::ObserverReady);
    }

    #[test]
    fn envelope_and_detail_formatting_match_constants() {
        let env = startup_envelope(None);
        assert_eq!(env.decision, 180_000);
        assert_eq!(env.stop, 60_000 + 30_000 + 5_000 + 2_000);
        assert_eq!(env.cleanup, env.stop + 7_000);
        assert_eq!(env.settled, env.decision + env.cleanup);
        assert_eq!(env.holder, 90_000 + env.stop + env.settled);
        assert_eq!(env.lock_wait, env.holder + 10_000);
        assert_eq!(env.handover_reply, 2 * env.holder + 20_000);
        let detail = startup_timeout_detail(
            &StartupTimeout {
                phase: StartupPhase::HostRuntime,
                limit: TimeoutLimit::Phase,
                phase_budget_ms: 60_000,
                elapsed_ms: 60_000,
            },
            "completed",
        );
        assert_eq!(
            detail,
            "RPC host startup timed out: phase=host_runtime; limit=phase; phase_budget_ms=60000; startup_budget_ms=180000; elapsed_ms=60000; cleanup=completed"
        );
    }
}
