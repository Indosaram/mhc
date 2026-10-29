//! The sync engine: one cycle for both directions (sync-design §5). Triggers - startup, an
//! advertised head beyond the applied cursor, a 10 s poll while the event stream is down, a local
//! commit after a 2 s debounce - only request a cycle; cycles run one at a time. A cycle resumes
//! any journaled work, receives outside the lock, captures and applies under it, then pushes
//! outside it; a stale base receives first and retries.
//!
//! The engine is single-threaded: `start` and the triggers spawn onto the current
//! `tokio::task::LocalSet`.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use super::apply::{advance, apply_verified, capture};
use super::engine_deps::EngineDeps;
use super::error::{EngineError, EngineResult};
use super::failure::{SyncFailure, describe_sync_failure, is_failure_shown};
use super::journal::{CycleStage, EngineState, SyncJournal};
use super::local_signal::{Generation, Signal};
use super::ports::{Cancel, RefusalCode, StreamHandlers, Unsubscribe};
use super::push::{PushOutcome, push, stale_backoff_ms, system_random};
use super::receive::receive;

pub use super::engine_deps::{CrashInjected, EngineFault, INBOUND_POINTS, OUTBOUND_POINTS};

pub const POLL_MS: u64 = 10_000;
pub const LOCAL_DEBOUNCE_MS: u64 = 2_000;
const PUSH_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineStatus {
    Failing {
        failure: SyncFailure,
        since: i64,
        attempts: u64,
    },
    NeverSynced,
    Synced {
        at: i64,
        updated_while_offline: u64,
    },
    Paused {
        seq: u64,
        code: RefusalCode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleReport {
    pub applied: usize,
    pub refused: u64,
    pub pushed: bool,
    pub snapshot: Option<u64>,
    pub waiting: bool,
}

#[derive(Clone)]
struct Failing {
    failure: SyncFailure,
    since: i64,
    attempts: u64,
}

struct Inner {
    stopped: Cell<bool>,
    failing: RefCell<Option<Failing>>,
    shown: RefCell<Option<Failing>>,
    last_error: RefCell<Option<String>>,
    deps: EngineDeps,
    journal: SyncJournal,
    cycles_run: Cell<u64>,
    loaded: Cell<bool>,
    running: Cell<bool>,
    rerun: Cell<bool>,
    event_head: Cell<u64>,
    applied_mirror: Cell<u64>,
    /// Bumped whenever a cycle settles (successfully or not).
    settled: Generation,
    /// Bumped on every successful cycle, carrying its report.
    reports: Generation,
    last_report: RefCell<Option<CycleReport>>,
    unsubscribe: RefCell<Option<Unsubscribe>>,
    cancel_poll: RefCell<Option<Cancel>>,
    cancel_debounce: RefCell<Option<Cancel>>,
}

pub struct SyncEngine {
    inner: Rc<Inner>,
}

impl SyncEngine {
    pub fn new(deps: EngineDeps) -> Self {
        let journal = SyncJournal::new(deps.repo.git_dir());
        Self {
            inner: Rc::new(Inner {
                deps,
                stopped: Cell::new(false),
                failing: RefCell::new(None),
                shown: RefCell::new(None),
                last_error: RefCell::new(None),
                journal,
                cycles_run: Cell::new(0),
                loaded: Cell::new(false),
                running: Cell::new(false),
                rerun: Cell::new(false),
                event_head: Cell::new(0),
                applied_mirror: Cell::new(0),
                settled: Generation::default(),
                reports: Generation::default(),
                last_report: RefCell::new(None),
                unsubscribe: RefCell::new(None),
                cancel_poll: RefCell::new(None),
                cancel_debounce: RefCell::new(None),
            }),
        }
    }

    pub fn journal(&self) -> &SyncJournal {
        &self.inner.journal
    }

    pub fn cycles_run(&self) -> u64 {
        self.inner.cycles_run.get()
    }

    pub fn state(&self) -> EngineResult<EngineState> {
        self.inner.journal.state()
    }

    /// What a UI shows: paused at a refused entry, else the last successful sync.
    pub fn status(&self) -> EngineResult<EngineStatus> {
        if let Some(failing) = self.inner.shown.borrow().as_ref() {
            return Ok(EngineStatus::Failing {
                failure: failing.failure.clone(),
                since: failing.since,
                attempts: failing.attempts,
            });
        }
        let s = self.inner.journal.state()?;
        if let Some(p) = s.paused {
            return Ok(EngineStatus::Paused {
                seq: p.seq,
                code: p.code,
            });
        }
        Ok(match s.last_synced_at {
            None => EngineStatus::NeverSynced,
            Some(at) => EngineStatus::Synced {
                at,
                updated_while_offline: s.updated_while_offline,
            },
        })
    }

    pub fn start(&self) {
        self.inner.stopped.set(false);
        let advanced = Rc::downgrade(&self.inner);
        let connected = Rc::downgrade(&self.inner);
        let unsubscribe = self.inner.deps.transport.subscribe(StreamHandlers {
            on_advanced: Rc::new(move |head| {
                if let Some(inner) = advanced.upgrade() {
                    inner.event_head.set(inner.event_head.get().max(head));
                    if head > inner.applied_mirror.get() {
                        Inner::request(&inner);
                    }
                }
            }),
            on_connected: Rc::new(move |up| {
                if let Some(inner) = connected.upgrade() {
                    if up {
                        inner.stop_polling();
                    } else {
                        Inner::poll(&inner);
                    }
                }
            }),
        });
        *self.inner.unsubscribe.borrow_mut() = Some(unsubscribe);
        Inner::request(&self.inner);
    }

    /// Stops the triggers and waits for a running cycle to settle.
    pub async fn stop(&self) {
        self.inner.stopped.set(true);
        self.inner.rerun.set(false);
        if let Some(unsubscribe) = self.inner.unsubscribe.borrow_mut().take() {
            unsubscribe();
        }
        self.inner.stop_polling();
        if let Some(cancel) = self.inner.cancel_debounce.borrow_mut().take() {
            cancel();
        }
        while self.inner.running.get() {
            let seen = self.inner.settled.current();
            self.inner.settled.changed(seen).await;
        }
    }

    pub fn notify_local_commit(&self) {
        if self.inner.stopped.get() {
            return;
        }
        if let Some(cancel) = self.inner.cancel_debounce.borrow_mut().take() {
            cancel();
        }
        let weak = Rc::downgrade(&self.inner);
        let cancel = self.inner.deps.clock.after(
            LOCAL_DEBOUNCE_MS,
            Box::new(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.cancel_debounce.borrow_mut().take();
                    Inner::request(&inner);
                }
            }),
        );
        *self.inner.cancel_debounce.borrow_mut() = Some(cancel);
    }

    /// The next successful cycle's report (the running cycle's when one is running).
    pub async fn next_cycle(&self) -> EngineResult<CycleReport> {
        let mut seen = self.inner.reports.current();
        loop {
            self.inner.reports.changed(seen).await;
            seen = self.inner.reports.current();
            if let Some(error) = self.inner.last_error.borrow().as_ref() {
                return Err(EngineError::Other(error.clone()));
            }
            if let Some(report) = self.inner.last_report.borrow().clone() {
                return Ok(report);
            }
        }
    }

    /// Runs one cycle after any running one has settled.
    pub async fn run_cycle(&self) -> EngineResult<CycleReport> {
        Inner::run_cycle(&self.inner).await
    }
}

impl Inner {
    async fn run_cycle(this: &Rc<Self>) -> EngineResult<CycleReport> {
        while this.running.get() {
            let seen = this.settled.current();
            this.settled.changed(seen).await;
        }
        if this.stopped.get() {
            return Ok(CycleReport {
                applied: 0,
                refused: 0,
                pushed: false,
                snapshot: None,
                waiting: true,
            });
        }
        this.running.set(true);
        let outcome = this.cycle().await;
        this.running.set(false);
        let before = this.shown_key();
        match &outcome {
            Ok(report) => {
                *this.failing.borrow_mut() = None;
                *this.shown.borrow_mut() = None;
                *this.last_error.borrow_mut() = None;
                *this.last_report.borrow_mut() = Some(report.clone());
            }
            Err(error) => {
                let failure = describe_sync_failure(error);
                let previous = this.failing.borrow().clone();
                let (since, attempts) = previous
                    .filter(|p| p.failure.kind == failure.kind)
                    .map_or((this.deps.clock.now(), 1), |p| (p.since, p.attempts + 1));
                let failing = Failing {
                    failure,
                    since,
                    attempts,
                };
                if is_failure_shown(failing.failure.kind, attempts) {
                    *this.shown.borrow_mut() = Some(failing.clone());
                }
                *this.failing.borrow_mut() = Some(failing);
                *this.last_error.borrow_mut() = Some(error.to_string());
            }
        }
        if before != this.shown_key()
            && let Some(notify) = &this.deps.on_status_change {
                notify();
            }
        this.reports.bump();
        this.settled.bump();
        if this.rerun.get()
            && (outcome.is_err() || this.event_head.get() > this.applied_mirror.get())
        {
            this.rerun.set(false);
            Self::request(this);
        }
        outcome
    }

    fn shown_key(&self) -> Option<(super::failure::SyncFailureKind, String)> {
        self.shown
            .borrow()
            .as_ref()
            .map(|f| (f.failure.kind, f.failure.action.clone()))
    }

    fn request(this: &Rc<Self>) {
        if this.stopped.get() {
            return;
        }
        if this.running.get() {
            this.rerun.set(true);
            return;
        }
        let inner = this.clone();
        tokio::task::spawn_local(async move {
            if let Err(error) = Self::run_cycle(&inner).await {
                inner.deps.log_line(&format!("sync cycle failed: {error}"));
            }
        });
    }

    /// A journal names the device peer it was created under. One from another peer arrived with a
    /// copied repo: its pending ops are that device's to push, so it is retired, never resumed.
    fn claim_journal(&self) -> EngineResult<()> {
        let peer = self.deps.store.peer()?;
        let owner = self.journal.state()?.peer;
        if let Some(owner) = &owner
            && *owner != peer
        {
            self.deps.log_line(&format!(
                "sync: journal belongs to peer {owner}, this device is {peer}; retiring it"
            ));
            self.journal.retire()?;
        }
        if owner.as_deref() != Some(peer.as_str()) {
            self.journal.update_state(|s| s.peer = Some(peer.clone()))?;
        }
        Ok(())
    }

    async fn sleep(&self, ms: u64) {
        let signal = Signal::default();
        let fire = signal.clone();
        // The cancel handle is dropped: a backoff sleep always runs to its end.
        let _cancel = self.deps.clock.after(ms, Box::new(move || fire.fire()));
        signal.wait().await;
    }

    fn poll(this: &Rc<Self>) {
        if this.stopped.get() {
            return;
        }
        this.stop_polling();
        let weak: Weak<Self> = Rc::downgrade(this);
        let cancel = this.deps.clock.after(
            POLL_MS,
            Box::new(move || {
                if let Some(inner) = weak.upgrade() {
                    Self::request(&inner);
                    Self::poll(&inner);
                }
            }),
        );
        *this.cancel_poll.borrow_mut() = Some(cancel);
    }

    fn stop_polling(&self) {
        if let Some(cancel) = self.cancel_poll.borrow_mut().take() {
            cancel();
        }
    }

    async fn inbound(
        &self,
        refused: &mut u64,
        snapshot: &mut Option<u64>,
        applied: &mut usize,
        memories: &mut usize,
    ) -> EngineResult<()> {
        let deps = &self.deps;
        let journal = &self.journal;
        let got = receive(deps, journal).await?;
        *refused += got.refused;
        *snapshot = got.snapshot.or(*snapshot);
        let mut done = None;
        deps.lock.with_lock(&mut || {
            capture(deps, journal, None)?;
            done = Some(apply_verified(deps, journal)?);
            Ok(())
        })?;
        if let Some(done) = done {
            *applied += done.batches;
            *memories += done.memories;
        }
        Ok(())
    }

    async fn cycle(&self) -> EngineResult<CycleReport> {
        let deps = &self.deps;
        let journal = &self.journal;
        if !self.loaded.get() {
            deps.store.load()?;
            self.claim_journal()?;
            self.loaded.set(true);
        }
        let head = self.event_head.get().max(journal.state()?.event_head);
        let before = journal.update_state(|s| s.event_head = head)?;
        if let Some(resumed) = before.cycle {
            deps.lock.with_lock(&mut || {
                // A cycle resumed at merged re-renders its keys over the working files: capture any
                // edit made since the crash first, so it merges into the doc instead of being
                // overwritten.
                if resumed.stage == CycleStage::Merged {
                    capture(deps, journal, Some(&resumed))?;
                }
                advance(deps, journal, resumed.clone())
            })?;
        }
        let (mut applied, mut memories, mut refused, mut snapshot) = (0usize, 0usize, 0u64, None);
        self.inbound(&mut refused, &mut snapshot, &mut applied, &mut memories)
            .await?;
        let mut pushed = false;
        let random: Rc<dyn Fn() -> f64> = deps
            .random
            .clone()
            .unwrap_or_else(|| Rc::new(system_random));
        for attempt in 0..PUSH_ATTEMPTS {
            let outcome = push(deps, journal).await?;
            if outcome != PushOutcome::Stale {
                pushed = outcome == PushOutcome::Pushed;
                break;
            }
            self.sleep(stale_backoff_ms(attempt, random.as_ref())).await;
            self.inbound(&mut refused, &mut snapshot, &mut applied, &mut memories)
                .await?;
        }
        if refused == 0 {
            let now = deps.clock.now();
            let offline = memories as u64;
            journal.update_state(|s| {
                s.last_synced_at = Some(now);
                s.updated_while_offline = offline;
            })?;
        }
        self.applied_mirror.set(journal.state()?.applied);
        self.cycles_run.set(self.cycles_run.get() + 1);
        Ok(CycleReport {
            applied,
            refused,
            pushed,
            snapshot,
            waiting: false,
        })
    }
}
