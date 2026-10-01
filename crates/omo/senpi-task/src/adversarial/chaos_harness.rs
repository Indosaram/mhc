//! Rust port of `__adversarial__/chaos-harness.ts`: the aggregate `ChaosHarness` struct and
//! `build_harness`, which wires the observing store, three chaos engines, the chaos notifier and
//! the instrumented completion notifier into one disposable per-iteration fixture.
//!
//! The TS source is test-only harness code, so the module is compiled only for tests.
#![cfg(test)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tempfile::TempDir;

use crate::completion::{CompletionNotifierDeps, CompletionNotifierStore, ScheduledCancel, ScheduledTask, create_completion_notifier};
use crate::lifecycle::TaskSettings;
use crate::manager::types::ManagerConfig;
use crate::manager::{AbortSignal, ManagedChildHandle, TaskManager};
use crate::store::{StateDirConfig, TaskRecordStore};

use super::chaos_engine::{ChaosProcessTable, LifecycleChaosObservations};
use super::chaos_engine_factory::{BuildChaosEnginesInput, ChaosEngine, build_chaos_engines};
use super::chaos_invariants::{
    InstrumentedNotifier, NotificationEpochTracker, create_chaos_notifier, instrument_completion_notifier,
};
use super::observing_store::{SharedObservations, create_observing_store};

pub const CHAOS_MODEL: &str = "anthropic/claude";
pub const CHAOS_SESSION: &str = "parent-chaos";

// TS `CLOCK_BASE` / `CLOCK_STEP` / `makeClock`: the TS harness hands a fake clock to
// `buildChaosEngines`. The Rust engines read the real clock (there is no injectable clock seam),
// so the pair is kept for parity with the TS surface and has no reader.
#[allow(dead_code)]
const CLOCK_BASE: i64 = 1_800_000_000_000;
#[allow(dead_code)]
const CLOCK_STEP: i64 = 1_000;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct ScheduledRetry {
    run: ScheduledTask,
    #[allow(dead_code)]
    delay_ms: u64,
}

/// `ChaosRetryScheduler`: a fake `CompletionRetrySchedule` that queues tasks for manual, seeded
/// selection instead of a real timer.
pub struct ChaosRetryScheduler {
    pending: Mutex<HashMap<u64, ScheduledRetry>>,
    next_id: AtomicU64,
}

impl ChaosRetryScheduler {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::default(),
            next_id: AtomicU64::new(0),
        })
    }

    pub fn pending_count(&self) -> usize {
        lock(&self.pending).len()
    }

    pub fn run(&self, index: usize) -> bool {
        let selected = {
            let mut pending = lock(&self.pending);
            let mut keys: Vec<u64> = pending.keys().copied().collect();
            keys.sort_unstable();
            let Some(key) = keys.get(index).copied() else {
                return false;
            };
            pending.remove(&key)
        };
        match selected {
            Some(retry) => {
                (retry.run)();
                true
            }
            None => false,
        }
    }

    fn schedule(self: &Arc<Self>, task: ScheduledTask, delay_ms: u64) -> ScheduledCancel {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.pending).insert(id, ScheduledRetry { run: task, delay_ms });
        let scheduler = Arc::clone(self);
        Box::new(move || {
            lock(&scheduler.pending).remove(&id);
        })
    }
}

/// One registered waiter: its step-scheduled abort signal, the step it is due at, and the thread
/// blocked in `manager.wait_for` on its behalf.
type ScheduledWaiter = (AbortSignal, u64, std::thread::JoinHandle<()>);

/// `ChaosWaiters`: registers a real `manager.wait_for` per TS `manager.waitFor`, and settles it
/// through the `AbortSignal` port (TS `AbortController`) either when the step-scheduled abort comes
/// due or in `abort_all`, exactly as the TS harness aborts its controllers.
pub struct ChaosWaiters {
    registrations: AtomicU64,
    settlements: Arc<AtomicU64>,
    current_step: Mutex<u64>,
    scheduled: Mutex<Vec<ScheduledWaiter>>,
}

impl ChaosWaiters {
    fn new() -> Self {
        Self {
            registrations: AtomicU64::new(0),
            settlements: Arc::new(AtomicU64::new(0)),
            current_step: Mutex::new(0),
            scheduled: Mutex::default(),
        }
    }

    pub fn counts(&self) -> (u64, u64) {
        (
            self.registrations.load(Ordering::SeqCst),
            self.settlements.load(Ordering::SeqCst),
        )
    }

    pub fn register(&self, manager: &TaskManager, task_id: &str, abort_after_steps: u64) {
        self.registrations.fetch_add(1, Ordering::SeqCst);
        let signal = AbortSignal::default();
        let target_step = *lock(&self.current_step) + abort_after_steps;
        let manager = manager.clone();
        let task_id = task_id.to_string();
        let thread_signal = signal.clone();
        let settled = Arc::clone(&self.settlements);
        let handle = std::thread::spawn(move || {
            // TS waits unbounded and relies on the controller abort to settle; the signal is polled
            // inside `wait_for`, so an abort returns the thread promptly with no timer involved.
            let _ = manager.wait_for(&task_id, Some(&thread_signal), None);
            settled.fetch_add(1, Ordering::SeqCst);
        });
        lock(&self.scheduled).push((signal, target_step, handle));
    }

    pub fn advance(&self) {
        let step = {
            let mut current = lock(&self.current_step);
            *current += 1;
            *current
        };
        let due = {
            let mut scheduled = lock(&self.scheduled);
            let mut due = Vec::new();
            let mut index = 0;
            while index < scheduled.len() {
                if scheduled[index].1 <= step {
                    due.push(scheduled.remove(index));
                } else {
                    index += 1;
                }
            }
            due
        };
        for (signal, _step, handle) in due {
            signal.abort();
            let _ = handle.join();
        }
    }

    pub fn abort_all(&self) {
        let scheduled = std::mem::take(&mut *lock(&self.scheduled));
        for (signal, _step, handle) in scheduled {
            signal.abort();
            let _ = handle.join();
        }
    }
}

#[allow(dead_code)]
fn make_clock() -> Arc<dyn Fn() -> i64 + Send + Sync> {
    let ticks = std::sync::atomic::AtomicI64::new(0);
    Arc::new(move || {
        let tick = ticks.fetch_add(1, Ordering::SeqCst);
        CLOCK_BASE + CLOCK_STEP * tick
    })
}

/// `ChaosHarness`: everything one chaos iteration or lifecycle probe drives.
pub struct ChaosHarness {
    pub model: String,
    pub session_id: String,
    pub manager: TaskManager,
    /// TS `harness.lifecycle` (the primary engine's lifecycle); actions read
    /// `engines[i].lifecycle`, so this handle has no reader in the port either.
    #[allow(dead_code)]
    pub lifecycle: Arc<crate::lifecycle::TaskLifecycle>,
    pub notifier: InstrumentedNotifier,
    pub parent_notifier: Arc<super::chaos_invariants::ChaosNotifier>,
    pub retry_scheduler: Arc<ChaosRetryScheduler>,
    pub waiters: ChaosWaiters,
    pub runner: Arc<super::chaos_engine::ChaosRunner>,
    pub engines: Vec<ChaosEngine>,
    pub probe_engine_index: usize,
    pub processes: Arc<ChaosProcessTable>,
    pub lifecycle_observations: Arc<LifecycleChaosObservations>,
    pub observations: SharedObservations,
    pub store: Arc<TaskRecordStore>,
    pub pending_cancelled_task_ids: Arc<Mutex<HashSet<String>>>,
    pub residency_max: usize,
    pub limit: usize,
    project: TempDir,
}

impl ChaosHarness {
    pub fn probe_engine(&self) -> &ChaosEngine {
        &self.engines[self.probe_engine_index]
    }

    /// Runs `mutate`, then feeds its net before/after effect on `task_id` through the same
    /// observation [`super::observing_store::ObservingStore`] would have performed had `mutate`
    /// written through it. The manager and lifecycle hold a concrete `TaskRecordStore` handle (not
    /// `ObservingStore`, which only a duck-typed store substitution like the TS harness's could
    /// wrap in place), so any caller driving a write through them observes it explicitly here.
    pub fn observe_mutation<R>(&self, task_id: &str, mutate: impl FnOnce() -> R) -> R {
        let before = self.store.load(task_id).ok().flatten();
        let result = mutate();
        let after = self.store.load(task_id).ok().flatten();
        super::observing_store::observe_store_write(&self.store, &self.observations, before.as_ref(), after.as_ref());
        result
    }

    /// Refreshes the resident-count observation without a specific `task_id` before/after diff, for
    /// a caller (session-wide eviction, reconcile) whose effect spans more than one record.
    pub fn observe_reconcile(&self) {
        super::observing_store::observe_store_write(&self.store, &self.observations, None, None);
    }

    pub fn write_session(&self, task_id: &str) -> std::path::PathBuf {
        let directory = self.store.state_dir().join("children").join(task_id).join("sessions");
        let _ = std::fs::create_dir_all(&directory);
        let path = directory.join(format!("{task_id}.jsonl"));
        let line = serde_json::json!({
            "type": "message",
            "message": { "role": "assistant", "content": "done" },
        });
        let _ = std::fs::write(&path, format!("{line}\n"));
        path
    }

    /// Applies the TS `flushMicrotasks()` point to this port's real outcome-watcher threads. The
    /// pinned TS tracks a child outcome as a promise continuation, so the outcome is applied on the
    /// microtask queue the chaos bench drains with `flushMicrotasks()`; this port hands the settle to
    /// a watcher thread, so the harness waits for the watcher to consume it before it reads state.
    /// Only a handle whose task is still non-terminal is waited on: a terminal task's watcher has
    /// already returned, so its later settles have no consumer and would block forever.
    pub fn flush_outcomes(&self) {
        for engine in &self.engines {
            for handle in engine.in_process_runner.all_handles() {
                self.flush_handle_outcome(handle.as_ref());
            }
            for handle in engine.process_runner.all_handles() {
                self.flush_handle_outcome(handle.as_ref());
            }
        }
    }

    fn flush_handle_outcome(&self, handle: &super::chaos_engine::ChaosRunnerHandle) {
        let terminal = self
            .store
            .load(handle.task_id())
            .ok()
            .flatten()
            .is_some_and(|record| super::observing_store::is_terminal_status(record.status));
        if terminal {
            return;
        }
        crate::manager::outcome::test_barrier::wait_until_consumed(handle as *const _ as usize);
    }

    pub fn observe_live_handles(&self) {
        let mut owners: HashMap<String, Vec<String>> = HashMap::new();
        for engine in &self.engines {
            for task_id in engine.manager.resident_task_ids() {
                owners.entry(task_id).or_default().push(engine.id.clone());
            }
        }
        for (task_id, ids) in owners {
            if ids.len() > 1 {
                let trace = lock(&self.lifecycle_observations.action_trace)
                    .iter()
                    .rev()
                    .take(12)
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" -> ");
                let record = self.store.load(&task_id).ok().flatten();
                lock(&self.lifecycle_observations.live_handle_breaches).push(format!(
                    "{task_id} live in {} host={} status={}/{} after {trace}",
                    ids.join(","),
                    record.as_ref().and_then(|record| record.host_pid).map_or("none".to_string(), |pid| pid.to_string()),
                    record.as_ref().map_or("missing".to_string(), |record| record.status.as_str().to_string()),
                    record.as_ref().map_or("missing".to_string(), |record| record.residency_state.as_str().to_string()),
                ));
            }
        }
    }
}

impl Drop for ChaosHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.project.path());
    }
}

pub struct ChaosHarnessOptions {
    pub concurrency: usize,
    pub residency_max: usize,
    pub max_depth: u32,
}

/// `buildHarness`: builds a fresh temp-dir-backed store, three chaos engines, the chaos notifier
/// and the instrumented completion notifier for one iteration.
pub fn build_harness(options: ChaosHarnessOptions) -> ChaosHarness {
    let project = tempfile::Builder::new()
        .prefix("senpi-chaos-")
        .tempdir()
        .expect("tempdir");
    let backing = TaskRecordStore::new(&StateDirConfig {
        project_dir: project.path().to_path_buf(),
        task_state_dir: None,
    });
    let observed = create_observing_store(backing, None);
    let store = Arc::new(observed.backing().clone());
    let observations: SharedObservations = Arc::clone(&observed.observations);
    let pending_cancelled_task_ids: Arc<Mutex<HashSet<String>>> = Arc::default();

    let config = TaskSettings::resolve(&serde_json::json!({
        "default_concurrency": options.concurrency,
        "residency_max_children": options.residency_max,
        "max_depth": options.max_depth,
        "resume_children": true,
    }))
    .expect("valid task settings");
    let manager_config = ManagerConfig {
        concurrency: crate::manager::concurrency::TaskConcurrencyConfig {
            default_concurrency: Some(options.concurrency),
            provider_concurrency: None,
            model_concurrency: None,
        },
        max_depth: options.max_depth,
        default_execution_mode: crate::manager::execution_mode::ExecutionMode::InProcess,
    };

    let processes = Arc::new(ChaosProcessTable::new());
    let lifecycle_observations = Arc::new(LifecycleChaosObservations::default());

    let engines = build_chaos_engines(BuildChaosEnginesInput {
        store: Arc::clone(&store),
        config,
        manager_config,
        cwd: project.path().to_string_lossy().into_owned(),
        model: CHAOS_MODEL.to_string(),
        processes: Arc::clone(&processes),
        observations: Arc::clone(&lifecycle_observations),
    });

    let primary_manager = engines[0].manager.clone();
    let retry_scheduler = ChaosRetryScheduler::new();
    let waiters = ChaosWaiters::new();
    let notification_epochs = Arc::new(NotificationEpochTracker::default());
    let parent_notifier = create_chaos_notifier(Arc::clone(&store), Arc::clone(&observations), Arc::clone(&notification_epochs));

    let completion_store: Arc<dyn CompletionNotifierStore> = Arc::clone(&store) as Arc<dyn CompletionNotifierStore>;
    let scheduler_for_closure = Arc::clone(&retry_scheduler);
    let mut deps = CompletionNotifierDeps::new(
        Arc::clone(&parent_notifier) as Arc<dyn crate::completion::ParentNotifier>,
        completion_store,
    );
    deps.schedule = Some(Arc::new(move |task, delay_ms| scheduler_for_closure.schedule(task, delay_ms)));
    deps.get_current_session_id = Some(Arc::new(|| Some(CHAOS_SESSION.to_string())));
    deps.get_parent_state = Some(Arc::new(|| crate::completion::ParentState::Idle));
    let base_notifier = create_completion_notifier(deps);
    let notifier = instrument_completion_notifier(base_notifier, Arc::clone(&store), Arc::clone(&notification_epochs));

    let runner = Arc::clone(&engines[0].in_process_runner);
    let residency_max = options.residency_max;
    let limit = options.concurrency;

    ChaosHarness {
        model: CHAOS_MODEL.to_string(),
        session_id: CHAOS_SESSION.to_string(),
        manager: primary_manager,
        lifecycle: Arc::clone(&engines[0].lifecycle),
        notifier,
        parent_notifier,
        retry_scheduler,
        waiters,
        runner,
        engines,
        probe_engine_index: 2,
        processes,
        lifecycle_observations,
        observations,
        store,
        pending_cancelled_task_ids,
        residency_max,
        limit,
        project,
    }
}
