//! Port of senpi `terminal/lease-keeper.ts`: the WAITER side of the terminal lease. It polls the
//! file system only (stat + kill 0, never a subprocess) and hands the session over exactly once
//! when the holder is gone. Losing the race to another waiter is not an error: the keeper keeps
//! waiting on the new holder.
//!
//! Native lease shape: the Rust `manifest_lease` records `{pid, startedAtMs}` and takes a
//! synchronous liveness probe, so the keeper's holder is a `LeaseRecord` and acquisition goes
//! through `acquire_terminal_lease` directly.
use crate::manifest_lease::{
    AcquireTerminalLeaseResult, LeaseRecord, acquire_terminal_lease, release_terminal_lease,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const LEASE_KEEPER_INTERVAL_MS: u64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseKeeperState {
    Idle,
    Waiting,
    Owner,
    Stopped,
}

pub type ProcessAliveProbe = Arc<dyn Fn(f64) -> bool + Send + Sync>;

pub struct LeaseKeeperOptions {
    pub dir: PathBuf,
    pub encoded_session_id: String,
    /// The acquired lease: its path and the pid it records.
    pub on_takeover: Box<dyn FnMut(PathBuf, f64) + Send>,
    /// A tick that failed (e.g. the state dir became unwritable); the keeper keeps waiting.
    pub on_error: Option<Box<dyn FnMut(std::io::Error) + Send>>,
    pub interval_ms: Option<u64>,
    pub self_pid: f64,
    pub self_started_at_ms: f64,
    pub is_process_alive: Option<ProcessAliveProbe>,
}

struct Inner {
    options: LeaseKeeperOptions,
    state: LeaseKeeperState,
    holder: Option<LeaseRecord>,
    timer: Option<tokio::task::JoinHandle<()>>,
    on_tick: Option<Box<dyn FnMut() + Send>>,
    in_flight: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone)]
pub struct LeaseKeeper {
    inner: Arc<Mutex<Inner>>,
}

fn alive(pid: f64, probe: Option<&ProcessAliveProbe>) -> bool {
    match probe {
        Some(probe) => probe(pid),
        // EPERM counts as alive; a probe error other than ESRCH also keeps waiting.
        None => crate::manifest_lease::probe_alive(pid).unwrap_or(true),
    }
}

impl LeaseKeeper {
    pub fn state(&self) -> LeaseKeeperState {
        self.lock().state
    }

    pub fn set_on_tick(&self, on_tick: Option<Box<dyn FnMut() + Send>>) {
        self.lock().on_tick = on_tick;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait on `holder`; `None` when it is unknown (e.g. the acquire failed), so every tick
    /// re-acquires. Must run inside a tokio runtime.
    pub fn start(&self, holder: Option<LeaseRecord>) {
        let mut inner = self.lock();
        if matches!(
            inner.state,
            LeaseKeeperState::Stopped | LeaseKeeperState::Owner
        ) {
            return;
        }
        inner.holder = holder;
        inner.state = LeaseKeeperState::Waiting;
        if inner.timer.is_none() {
            drop(inner);
            self.arm();
        }
    }

    pub fn stop(&self) {
        let mut inner = self.lock();
        if let Some(timer) = inner.timer.take() {
            timer.abort();
        }
        if inner.state != LeaseKeeperState::Owner {
            inner.state = LeaseKeeperState::Stopped;
        }
    }

    /// Resolves after the tick currently in flight (if any) has settled.
    pub async fn settled(&self) {
        let gate = self.lock().in_flight.clone();
        drop(gate.lock().await);
    }

    fn arm(&self) {
        let keeper = self.clone();
        let interval = self
            .lock()
            .options
            .interval_ms
            .unwrap_or(LEASE_KEEPER_INTERVAL_MS);
        let gate = self.lock().in_flight.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
            let _running = gate.lock().await;
            keeper.lock().timer = None;
            if let Err(error) = keeper.tick() {
                let rearm = keeper.lock().state == LeaseKeeperState::Waiting;
                if rearm {
                    keeper.arm();
                }
                if let Some(on_error) = keeper.lock().options.on_error.as_mut() {
                    on_error(error);
                }
            }
        });
        self.lock().timer = Some(handle);
    }

    fn lease_path(&self) -> PathBuf {
        let inner = self.lock();
        inner
            .options
            .dir
            .join(format!("{}.lease", inner.options.encoded_session_id))
    }

    fn tick(&self) -> std::io::Result<()> {
        if self.state() != LeaseKeeperState::Waiting {
            return Ok(());
        }
        if let Some(on_tick) = self.lock().on_tick.as_mut() {
            on_tick();
        }
        let (holder, probe) = {
            let inner = self.lock();
            (inner.holder.clone(), inner.options.is_process_alive.clone())
        };
        let gone = match holder {
            None => true,
            Some(holder) => !self.lease_path().exists() || !alive(holder.pid, probe.as_ref()),
        };
        if self.state() != LeaseKeeperState::Waiting {
            return Ok(());
        }
        if gone {
            return self.try_acquire();
        }
        self.arm();
        Ok(())
    }

    fn try_acquire(&self) -> std::io::Result<()> {
        let (dir, encoded, pid, started, probe) = {
            let inner = self.lock();
            let options = &inner.options;
            (
                options.dir.clone(),
                options.encoded_session_id.clone(),
                options.self_pid,
                options.self_started_at_ms,
                options.is_process_alive.clone(),
            )
        };
        let result = acquire_terminal_lease(&dir, &encoded, pid, started, |holder| {
            Ok(match &probe {
                Some(probe) => probe(holder),
                None => crate::manifest_lease::probe_alive(holder)?,
            })
        })?;
        if self.state() != LeaseKeeperState::Waiting {
            // stop() landed while the acquire was in flight: hand the lease straight back.
            if let AcquireTerminalLeaseResult::Acquired { path, pid } = result {
                release_terminal_lease(&path, pid)?;
            }
            return Ok(());
        }
        match result {
            AcquireTerminalLeaseResult::Acquired { path, pid } => {
                let mut inner = self.lock();
                inner.state = LeaseKeeperState::Owner;
                inner.timer = None;
                (inner.options.on_takeover)(path, pid);
            }
            AcquireTerminalLeaseResult::Held { holder } => {
                self.lock().holder = Some(holder);
                self.arm();
            }
        }
        Ok(())
    }
}

/// `createLeaseKeeper`.
pub fn create_lease_keeper(options: LeaseKeeperOptions) -> LeaseKeeper {
    LeaseKeeper {
        inner: Arc::new(Mutex::new(Inner {
            options,
            state: LeaseKeeperState::Idle,
            holder: None,
            timer: None,
            on_tick: None,
            in_flight: Arc::new(tokio::sync::Mutex::new(())),
        })),
    }
}

/// The lease path a keeper polls, for callers that inspect it.
pub fn lease_path(dir: &Path, encoded_session_id: &str) -> PathBuf {
    dir.join(format!("{encoded_session_id}.lease"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const HOLDER_PID: f64 = 424_242.0;
    const SELF_PID: f64 = 777.0;
    const TICK_MS: u64 = 10_000;

    fn write_foreign_lease(dir: &Path) {
        std::fs::write(
            dir.join("s.lease"),
            serde_json::json!({"pid": HOLDER_PID, "startedAtMs": 1.0}).to_string(),
        )
        .unwrap();
    }

    struct Harness {
        keeper: LeaseKeeper,
        takeovers: Arc<AtomicUsize>,
        ticks: Arc<AtomicUsize>,
    }

    fn keeper_for(dir: &Path, holder_alive: Arc<AtomicBool>) -> Harness {
        let takeovers = Arc::new(AtomicUsize::new(0));
        let count = takeovers.clone();
        let keeper = create_lease_keeper(LeaseKeeperOptions {
            dir: dir.to_path_buf(),
            encoded_session_id: "s".into(),
            on_takeover: Box::new(move |_: PathBuf, _: f64| {
                count.fetch_add(1, Ordering::SeqCst);
            }),
            on_error: None,
            interval_ms: Some(TICK_MS),
            self_pid: SELF_PID,
            self_started_at_ms: 2.0,
            is_process_alive: Some(Arc::new(move |pid: f64| {
                pid != HOLDER_PID || holder_alive.load(Ordering::SeqCst)
            })),
        });
        let ticks = Arc::new(AtomicUsize::new(0));
        let tick_count = ticks.clone();
        keeper.set_on_tick(Some(Box::new(move || {
            tick_count.fetch_add(1, Ordering::SeqCst);
        })));
        Harness {
            keeper,
            takeovers,
            ticks,
        }
    }

    async fn ticks(harness: &Harness, count: u64) {
        for _ in 0..count {
            tokio::time::advance(std::time::Duration::from_millis(TICK_MS)).await;
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            harness.keeper.settled().await;
        }
    }

    fn holder() -> Option<LeaseRecord> {
        Some(LeaseRecord {
            pid: HOLDER_PID,
            started_at_ms: 1.0,
        })
    }

    #[tokio::test(start_paused = true)]
    async fn stays_a_waiter_while_the_holder_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        write_foreign_lease(dir.path());
        let harness = keeper_for(dir.path(), Arc::new(AtomicBool::new(true)));
        harness.keeper.start(holder());
        ticks(&harness, 3).await;
        assert_eq!(harness.keeper.state(), LeaseKeeperState::Waiting);
        assert_eq!(harness.takeovers.load(Ordering::SeqCst), 0);
        assert_eq!(harness.ticks.load(Ordering::SeqCst), 3);
        harness.keeper.stop();
    }

    #[tokio::test(start_paused = true)]
    async fn takes_over_exactly_once_when_the_lease_file_disappears_and_then_stops_ticking() {
        let dir = tempfile::tempdir().unwrap();
        write_foreign_lease(dir.path());
        let harness = keeper_for(dir.path(), Arc::new(AtomicBool::new(true)));
        harness.keeper.start(holder());
        ticks(&harness, 1).await;
        std::fs::remove_file(dir.path().join("s.lease")).unwrap();
        ticks(&harness, 3).await;
        assert_eq!(harness.keeper.state(), LeaseKeeperState::Owner);
        assert_eq!(harness.takeovers.load(Ordering::SeqCst), 1);
        assert_eq!(harness.ticks.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn takes_over_when_the_holder_pid_dies_even_though_its_lease_file_remains() {
        let dir = tempfile::tempdir().unwrap();
        write_foreign_lease(dir.path());
        let alive = Arc::new(AtomicBool::new(true));
        let harness = keeper_for(dir.path(), alive.clone());
        harness.keeper.start(holder());
        alive.store(false, Ordering::SeqCst);
        ticks(&harness, 1).await;
        assert_eq!(harness.keeper.state(), LeaseKeeperState::Owner);
        assert_eq!(harness.takeovers.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn never_fires_after_stop_and_stop_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let harness = keeper_for(dir.path(), Arc::new(AtomicBool::new(false)));
        harness.keeper.start(None);
        harness.keeper.stop();
        harness.keeper.stop();
        ticks(&harness, 3).await;
        assert_eq!(harness.keeper.state(), LeaseKeeperState::Stopped);
        assert_eq!(harness.takeovers.load(Ordering::SeqCst), 0);
        assert!(!dir.path().join("s.lease").exists());
        harness.keeper.start(None);
        assert_eq!(harness.keeper.state(), LeaseKeeperState::Stopped);
    }

    #[tokio::test(start_paused = true)]
    async fn re_classifies_when_the_holder_hands_the_lease_to_another_live_process() {
        let dir = tempfile::tempdir().unwrap();
        // Unknown holder: the first tick tries to acquire and finds a live foreign lease.
        write_foreign_lease(dir.path());
        let harness = keeper_for(dir.path(), Arc::new(AtomicBool::new(true)));
        harness.keeper.start(None);
        ticks(&harness, 2).await;
        assert_eq!(harness.keeper.state(), LeaseKeeperState::Waiting);
        assert_eq!(harness.takeovers.load(Ordering::SeqCst), 0);
        harness.keeper.stop();
    }
}
