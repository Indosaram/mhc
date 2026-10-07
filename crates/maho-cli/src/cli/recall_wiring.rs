//! CLI production construction of the resident Kibitzer recall wiring (latest `recall-wiring.ts`).
//!
//! The CLI owns the ports that need host objects: the resident child spawner (its own
//! `HostRuntimeFactory::create` session primitive), the runtime executor the sidecar schedules and
//! times on, the real parent `ExtensionActions` a steer is delivered through, and the OMO runtime's
//! idle coordinator. The memory component owns the delivery policy; the only memory-typed inputs the
//! CLI passes through are the three `kibitzer_delivery` ports it is handed by the memory composition.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::ExtensionActions;
use maho_omo_memory::context::MemoryIdentityContext;
use maho_omo_memory::kibitzer_child::KibitzerChildSpawner;
use maho_omo_memory::kibitzer_contract::{KibitzerSidecarTimers, KibitzerWakeSpawn};
use maho_omo_memory::kibitzer_delivery::{KibitzerIdleCoordinator, KibitzerPendingPort, KibitzerSteerMessage};
use maho_omo_memory::kibitzer_session_resources::KibitzerSessionResources;
use maho_omo_memory::kibitzer_events::KibitzerEventCaps;
use maho_omo_memory::kibitzer_sidecar_admission::{BlockingAcquire, BlockingAcquireFuture, KibitzerBlockingExecutor, WakeAdmissionAttempt};
use maho_omo_memory::prompt::PromptContextResolver;
use maho_omo_memory::recall_drain::PendingNudgesPort;
use maho_omo_memory::recall_wiring::{MemoryRecallWiring, MemoryRecallWiringOptions};
use memory_core::git::{GitMemoryRepo, GitMemoryRepoOptions};
use memory_core::locks::recall_wake_domain::RecallWakeError;
use memory_core::recall::RecallLedger;
use serde_json::Value;

use super::kibitzer_child::{CliKibitzerChildSpawner, KibitzerChildResourcesFactory};

/// The real sidecar timers: `set` schedules the callback on the retained host runtime and `clear`
/// aborts that task, so a timer can never outlive the runtime or block a sole reactor.
pub struct CliSidecarTimers {
    executor: tokio::runtime::Handle,
    next: AtomicU64,
    tasks: Mutex<BTreeMap<u64, tokio::task::JoinHandle<()>>>,
}

impl CliSidecarTimers {
    pub fn new(executor: tokio::runtime::Handle) -> Arc<Self> {
        Arc::new(Self { executor, next: AtomicU64::new(1), tasks: Mutex::new(BTreeMap::new()) })
    }
}

impl KibitzerSidecarTimers for CliSidecarTimers {
    fn set(&self, callback: Box<dyn FnOnce() + Send>, ms: i64) -> u64 {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let delay = std::time::Duration::from_millis(ms.max(0).unsigned_abs());
        let handle = self.executor.spawn(async move {
            tokio::time::sleep(delay).await;
            callback();
        });
        self.tasks.lock().unwrap_or_else(PoisonError::into_inner).insert(id, handle);
        id
    }

    fn clear(&self, handle: u64) {
        if let Some(task) = self.tasks.lock().unwrap_or_else(PoisonError::into_inner).remove(&handle) {
            task.abort();
        }
    }
}

/// The retained NATIVE blocking executor the machine-wide wake lease acquire runs on.
///
/// The worker is spawned EAGERLY, so dropping the returned future only drops the `JoinHandle`: a
/// `spawn_blocking` task is never cancelled by that drop, so a lease the worker wins late is still
/// handed back. When the worker completes with nobody awaiting, tokio drops its produced
/// `WakeAdmissionAttempt`, and that drop is what releases an unconsumed `PendingLease` and reports
/// its outcome - nothing is retained, buffered or forgotten.
struct CliBlockingExecutor {
    executor: tokio::runtime::Handle,
}

impl CliBlockingExecutor {
    fn new(executor: tokio::runtime::Handle) -> Arc<Self> {
        Arc::new(Self { executor })
    }
}

impl KibitzerBlockingExecutor for CliBlockingExecutor {
    fn run_blocking(&self, task: BlockingAcquire) -> BlockingAcquireFuture {
        let worker = self.executor.spawn_blocking(task);
        Box::pin(async move {
            match worker.await {
                Ok(attempt) => attempt,
                // The worker produced no verdict: it panicked, or the runtime is shutting down.
                // `Error(RecallWakeError)` is the only error channel the attempt enum carries, so the
                // lock domain reports it; no admission is invented and no lease is fabricated.
                Err(error) => WakeAdmissionAttempt::Error(RecallWakeError::Lock(format!("the blocking wake acquire did not complete: {error}"))),
            }
        })
    }
}

/// The CLI ports one recall wiring needs. The three `kibitzer_delivery` ports (`send_message`,
/// `pending_for`, `coordinator`) are memory-owned types and are handed in by the memory composition.
pub type RecallEnvironment = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
pub type RecallPendingResolver = Arc<dyn Fn(&MemoryIdentityContext) -> Arc<dyn KibitzerPendingPort> + Send + Sync>;
pub type RecallDrainResolver = Arc<dyn Fn(&MemoryIdentityContext) -> Arc<dyn PendingNudgesPort> + Send + Sync>;
pub type RecallQueuedDrain = Arc<dyn Fn(&str, &MemoryIdentityContext) -> Vec<memory_core::recall::RecallNudge> + Send + Sync>;
type RecallEntryAppender = Arc<dyn Fn(&str, Value) + Send + Sync>;
type RecallRepoFactory = Arc<dyn Fn(&MemoryIdentityContext) -> Result<GitMemoryRepo, String> + Send + Sync>;

pub struct CliRecallWiringPorts {
    pub executor: tokio::runtime::Handle,
    pub cwd: String,
    pub agent_dir: String,
    /// The FULL omo config the resident model resolver reads (`LiveMemoryConfig`).
    pub config: super::memory_runtime::LiveMemoryConfig,
    /// `memory.recall.category`; the resolver falls back to the pinned default when absent.
    pub category: Option<String>,
    /// The per-session registry snapshot the resident model resolver reads at child start.
    pub registry_for: super::kibitzer_child::SessionModelRegistry,
    /// The per-session resources factory: called once per child, so each session binds its own
    /// workspace/session-entries/budget rather than sharing the mount's tool set.
    pub resources: KibitzerChildResourcesFactory,
    /// The SAME per-session resources registry the sidecar uses: the wiring forwards it into
    /// `MemoryRecallWiringOptions`, so the sidecar retrieves the identical
    /// `KibitzerSessionResources` the factory captured - one object per session, never a copy.
    pub session_resources_for: Arc<dyn Fn(&str) -> KibitzerSessionResources + Send + Sync>,
    pub actions: Arc<dyn ExtensionActions>,
    pub resolve_context: PromptContextResolver,
    pub resolve_settings: Arc<dyn Fn() -> Result<Value, String> + Send + Sync>,
    pub env: RecallEnvironment,
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
    pub caps: KibitzerEventCaps,
    pub task_summary: Option<String>,
    pub tool_budget: Option<usize>,
    pub max_concurrent_wakes: Option<usize>,
    pub sidecar_max_tokens: Option<i64>,
    pub send_message: Arc<dyn Fn(KibitzerSteerMessage) -> Result<(), String> + Send + Sync>,
    pub pending_for: RecallPendingResolver,
    pub coordinator: Option<Arc<dyn KibitzerIdleCoordinator>>,
    pub drain_pending_for: RecallDrainResolver,
    pub drain_queued: Option<RecallQueuedDrain>,
}

/// Build the ONE resident recall wiring for a mount.
pub fn create_memory_recall_wiring(ports: CliRecallWiringPorts) -> Arc<MemoryRecallWiring> {
    let CliRecallWiringPorts {
        executor,
        cwd,
        agent_dir,
        config,
        category,
        registry_for,
        resources,
        session_resources_for,
        actions,
        resolve_context,
        resolve_settings,
        env,
        warn,
        caps,
        tool_budget,
        max_concurrent_wakes,
        sidecar_max_tokens,
        send_message,
        pending_for,
        coordinator,
        drain_pending_for,
        drain_queued,
        // `task_summary` has no consumer on the memory side (receipt B6): it stays a port and is not
        // destructured here, rather than being silently dropped.
        ..
    } = ports;

    let spawn: KibitzerWakeSpawn = {
        let executor = executor.clone();
        Arc::new(move |future| {
            executor.spawn(future);
        })
    };
    let timers: Arc<dyn KibitzerSidecarTimers> = CliSidecarTimers::new(executor.clone());
    // The retained NATIVE blocking executor: the machine-wide wake lease acquire runs on it, off the
    // reactor, and a dropped waiting future never cancels the worker (`CliBlockingExecutor`).
    let blocking_executor: Arc<dyn KibitzerBlockingExecutor> = CliBlockingExecutor::new(executor.clone());
    let spawner: Arc<dyn KibitzerChildSpawner> =
        CliKibitzerChildSpawner::from_agent_dir(executor, cwd, agent_dir, config, category, registry_for, resources);

    let append_entry: RecallEntryAppender = {
        let actions = Arc::clone(&actions);
        let warn = Arc::clone(&warn);
        Arc::new(move |kind: &str, data: Value| {
            if let Err(error) = actions.append_entry(kind, Some(data)) {
                warn(&error.to_string());
            }
        })
    };
    let ledger_for: Arc<dyn Fn(&MemoryIdentityContext) -> RecallLedger + Send + Sync> =
        Arc::new(|identity: &MemoryIdentityContext| RecallLedger::new(identity.identity_paths.recall_ledger.clone()));
    let create_repo: RecallRepoFactory =
        Arc::new(|identity: &MemoryIdentityContext| {
            GitMemoryRepo::new(GitMemoryRepoOptions::new(identity.identity_paths.repo.clone(), identity.identity.clone()))
                .map_err(|error| error.to_string())
        });
    let now_ms: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(memory_core::support::time::now_millis);
    let random: Arc<dyn Fn() -> f64 + Send + Sync> = Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| f64::from(elapsed.subsec_nanos()) / 1_000_000_000.0)
            .unwrap_or(0.0)
    });
    let drain_resolve_settings: Arc<dyn Fn() -> Value + Send + Sync> = {
        let resolve_settings = Arc::clone(&resolve_settings);
        Arc::new(move || resolve_settings().unwrap_or(Value::Null))
    };

    MemoryRecallWiring::new(MemoryRecallWiringOptions {
        resolve_context,
        resolve_settings,
        env,
        create_repo,
        ledger_for,
        pending_for,
        coordinator,
        send_message,
        append_entry,
        spawn,
        timers,
        now_ms,
        random,
        warn,
        spawner,
        executor: blocking_executor,
        event_caps: Some(caps),
        tool_budget,
        max_concurrent_wakes,
        sidecar_max_tokens,
        drain_resolve_settings,
        drain_pending_for,
        drain_queued,
        session_resources_for,
    })
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::task::{Context, Poll, Wake, Waker};
    use std::time::Duration;

    use maho_omo_memory::kibitzer_contract::KibitzerWakeSpawn;
    use maho_omo_memory::kibitzer_sidecar_admission::{
        Admission, BlockingAcquire, BlockingAcquireFuture, KibitzerBlockingExecutor, SidecarAdmission, WakeAdmissionAttempt, create_admission,
    };
    use maho_omo_memory::kibitzer_sidecar_core::{KibitzerSidecarCoreOptions, SidecarCore, create_sidecar_core};
    use maho_omo_memory::kibitzer_sidecar_recovery::create_recovery;
    use maho_omo_memory::kibitzer_sidecar_turn::create_turn_lifecycle;
    use maho_omo_memory::kibitzer_session_resources::KibitzerSessionResourceRegistry;
    use maho_omo_memory::kibitzer_wake_slot::{KibitzerWakeAdmission, KibitzerWakeSlot, KibitzerWakeSlotOptions};
    use memory_core::locks::recall_wake_domain::RecallWakeError;
    use tokio::sync::mpsc::error::TryRecvError;

    use super::CliBlockingExecutor;

    /// The failure deadline for an exact-signal wait. It is NEVER the pass condition: a signal that does
    /// not arrive inside it fails the test instead of hanging the suite.
    const SIGNAL_DEADLINE: Duration = Duration::from_secs(10);

    /// A no-op waker: the one poll each test performs is deliberate, and no test waits for a wake-up.
    struct NoopWake;

    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    /// A runtime with ONE blocking thread. Two properties the tests rest on, both structural rather than
    /// timing-based: a task spawned while that thread is busy cannot start (no second thread exists), and
    /// a task's produced value is dropped by that same thread before it takes the next task.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_time().max_blocking_threads(1).build().unwrap()
    }

    fn core_with_warnings(warnings: tokio::sync::mpsc::UnboundedSender<String>) -> Arc<SidecarCore> {
        create_sidecar_core(KibitzerSidecarCoreOptions {
            session_id: "session".to_owned(),
            resources: KibitzerSessionResourceRegistry::new().for_session("session"),
            tool_budget: None,
            wake_deadline_ms: None,
            sidecar_max_tokens: None,
            event_caps: None,
            now: Some(Arc::new(|| 0)),
            random: None,
            timers: None,
            warn: Some(Arc::new(move |message: &str| {
                let _ = warnings.send(message.to_owned());
            })),
        })
    }

    /// One machine slot over a temp locks directory that NEVER waits: `wait_timeout_ms: Some(0)` takes a
    /// free slot on its first attempt and reports `Busy` otherwise.
    fn slot(locks_directory: &std::path::Path) -> KibitzerWakeSlot {
        KibitzerWakeSlot::new(KibitzerWakeSlotOptions {
            locks_directory: locks_directory.to_path_buf(),
            max_concurrent: 1,
            wait_timeout_ms: Some(0),
            poll_ms: None,
            now: Arc::new(|| 0),
        })
        .unwrap()
    }

    fn noop_spawn() -> KibitzerWakeSpawn {
        Arc::new(|_future| {})
    }

    fn sidecar_admission(core: Arc<SidecarCore>, wake_slot: Arc<KibitzerWakeSlot>, executor: Arc<dyn KibitzerBlockingExecutor>) -> SidecarAdmission {
        let turns = Arc::new(create_turn_lifecycle(Arc::clone(&core), noop_spawn(), Arc::new(|_outcome| {})));
        let recovery = Arc::new(create_recovery(Arc::clone(&core), noop_spawn()));
        create_admission(core, wake_slot, turns, recovery, executor)
    }

    /// Probes the machine slot WITHOUT waiting: `true` when it is free (the probe's own lease is handed
    /// straight back), `false` when the only slot is still held.
    fn slot_is_free(wake_slot: &KibitzerWakeSlot) -> bool {
        match wake_slot.acquire(None).unwrap() {
            KibitzerWakeAdmission::Acquired { lease, .. } => {
                assert!(lease.try_release().unwrap(), "the probe's own lease must release");
                true
            }
            KibitzerWakeAdmission::Busy { .. } => false,
            KibitzerWakeAdmission::Aborted => panic!("an uncancelled probe cannot abort"),
        }
    }

    /// The exact completion signal: a task queued on the runtime's ONE blocking thread WHILE that thread
    /// is still running the gated worker. It cannot start before that worker finished, so the signal
    /// arriving means the worker completed - and its produced value was dropped, since no handle is left.
    fn queue_completion_guard() -> tokio::sync::oneshot::Receiver<()> {
        let (finished, completed) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _ = finished.send(());
        });
        completed
    }

    async fn await_completion(completed: tokio::sync::oneshot::Receiver<()>) {
        tokio::time::timeout(SIGNAL_DEADLINE, completed).await.expect("the worker must finish").unwrap();
    }

    /// The production adapter PLUS the test's window onto its worker. The executor under test is `inner`
    /// The Handle, the eager spawn, the returned future and the dropped handle are all production's.
    /// The wrapper only wraps the WORKER closure so the test learns the exact moment the production worker
    /// produced its verdict, and can hold the task open past that point (`gate`), which is what makes
    /// "the waiting future is dropped while the worker is still running" deterministic instead of a race.
    struct WindowedExecutor {
        inner: Arc<CliBlockingExecutor>,
        produced: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl WindowedExecutor {
        fn new(handle: tokio::runtime::Handle, gate: Option<std::sync::mpsc::Receiver<()>>) -> Arc<Self> {
            Arc::new(Self { inner: CliBlockingExecutor::new(handle), produced: Mutex::new(None), gate: Mutex::new(gate) })
        }

        /// The exact signal that the production worker has produced its verdict: at that moment a REAL
        /// machine lease is held, and the verdict is still a local inside the running worker.
        fn take_produced(&self) -> tokio::sync::oneshot::Receiver<()> {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            *self.produced.lock().unwrap_or_else(PoisonError::into_inner) = Some(sender);
            receiver
        }
    }

    impl KibitzerBlockingExecutor for WindowedExecutor {
        fn run_blocking(&self, task: BlockingAcquire) -> BlockingAcquireFuture {
            let produced = self.produced.lock().unwrap_or_else(PoisonError::into_inner).take();
            let gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner).take();
            self.inner.run_blocking(Box::new(move || {
                let verdict = task();
                if let Some(produced) = produced {
                    let _ = produced.send(());
                }
                if let Some(gate) = gate {
                    let _ = gate.recv();
                }
                verdict
            }))
        }
    }

    /// THE OWED CASE: the waiting future is dropped WHILE the production worker is still running, so the
    /// worker produces its verdict with nobody holding the handle. That produced value must be DROPPED on
    /// completion - the drop is what releases the machine lease - and never retained.
    #[test]
    fn given_a_waiting_future_dropped_while_the_worker_runs_then_the_produced_verdict_is_dropped_and_the_lease_is_released() {
        runtime().block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let (warnings, mut reported) = tokio::sync::mpsc::unbounded_channel::<String>();
            let core = core_with_warnings(warnings);
            let wake_slot = Arc::new(slot(dir.path()));
            let (release, gate) = std::sync::mpsc::channel::<()>();
            let executor = WindowedExecutor::new(tokio::runtime::Handle::current(), Some(gate));
            let produced = executor.take_produced();
            let admission = sidecar_admission(Arc::clone(&core), Arc::clone(&wake_slot), executor);

            let mut waiting = Box::pin(admission.admit());
            let waker = Waker::from(Arc::new(NoopWake));
            let mut context = Context::from_waker(&waker);
            assert!(matches!(waiting.as_mut().poll(&mut context), Poll::Pending), "the worker has not produced a verdict yet");

            // The production worker ran and produced its verdict: a REAL lease is held, still as a local
            // inside the running (gated) task.
            tokio::time::timeout(SIGNAL_DEADLINE, produced).await.expect("the worker must produce its verdict").unwrap();
            assert!(!slot_is_free(&wake_slot), "the produced verdict holds the machine lease");

            // Queued while the ONE blocking thread is still running the gated worker: it cannot start
            // before that worker finished.
            let completed = queue_completion_guard();

            // THE CASE: the waiting future - and with it the only handle to the running worker - is
            // dropped before the worker handed its verdict to anyone.
            drop(waiting);
            assert_eq!(reported.try_recv(), Err(TryRecvError::Empty), "dropping the waiting future releases nothing by itself");
            assert!(!slot_is_free(&wake_slot), "the running worker still owns its lease");

            // Let the worker return; with no handle left, the pool drops what it produced.
            release.send(()).unwrap();
            await_completion(completed).await;

            // The lease is back: the produced verdict was dropped, not retained.
            assert!(slot_is_free(&wake_slot), "the produced verdict must have been dropped, releasing the machine lease");
            assert_eq!(reported.try_recv(), Err(TryRecvError::Empty), "a clean release is reported silently");
        });
    }

    /// The companion case: the worker COMPLETED while the handle was still alive, so its produced value
    /// was RETAINED; dropping the handle must drop that retained value and release the lease.
    #[test]
    fn given_a_completed_worker_when_the_waiting_future_is_dropped_then_the_retained_verdict_is_dropped_and_the_lease_is_released() {
        runtime().block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let (warnings, _reported) = tokio::sync::mpsc::unbounded_channel::<String>();
            let core = core_with_warnings(warnings);
            let wake_slot = Arc::new(slot(dir.path()));
            let (release, gate) = std::sync::mpsc::channel::<()>();
            let executor = WindowedExecutor::new(tokio::runtime::Handle::current(), Some(gate));
            let produced = executor.take_produced();
            let admission = sidecar_admission(Arc::clone(&core), Arc::clone(&wake_slot), executor);

            let mut waiting = Box::pin(admission.admit());
            let waker = Waker::from(Arc::new(NoopWake));
            let mut context = Context::from_waker(&waker);
            assert!(matches!(waiting.as_mut().poll(&mut context), Poll::Pending), "the worker has not produced a verdict yet");
            tokio::time::timeout(SIGNAL_DEADLINE, produced).await.expect("the worker must produce its verdict").unwrap();

            let completed = queue_completion_guard();
            // The worker returns while the handle is STILL alive, so its produced verdict is retained.
            release.send(()).unwrap();
            await_completion(completed).await;

            assert!(!slot_is_free(&wake_slot), "the retained verdict still holds the machine lease");
            // Dropping the handle drops the retained value, synchronously on this thread.
            drop(waiting);
            assert!(slot_is_free(&wake_slot), "the retained verdict must have been dropped, releasing the machine lease");
        });
    }

    /// The POLLED path is unchanged: a free machine slot is admitted through the bare production adapter
    /// and the real lease is handed over, releasing exactly once.
    #[test]
    fn given_a_free_machine_slot_when_admitted_through_the_production_adapter_then_the_real_lease_is_handed_over() {
        runtime().block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let (warnings, _reported) = tokio::sync::mpsc::unbounded_channel::<String>();
            let core = core_with_warnings(warnings);
            let wake_slot = Arc::new(slot(dir.path()));
            let admission = sidecar_admission(Arc::clone(&core), Arc::clone(&wake_slot), CliBlockingExecutor::new(tokio::runtime::Handle::current()));

            match admission.admit().await {
                Admission::Acquired { lease, .. } => {
                    assert_eq!(lease.slot, 1, "the single machine slot is the one admitted");
                    assert!(lease.try_release().unwrap(), "the handed-over lease releases exactly once");
                }
                _ => panic!("a free machine slot must be admitted"),
            }
            assert!(slot_is_free(&wake_slot), "the released lease must have freed the real slot");
        });
    }

    /// A worker that produced no verdict is reported as the lock-domain error, the only error channel the
    /// attempt carries; no admission is invented and no lease is fabricated.
    #[test]
    fn given_a_worker_that_produced_no_verdict_when_awaited_then_the_attempt_is_the_lock_domain_error() {
        runtime().block_on(async {
            let executor = CliBlockingExecutor::new(tokio::runtime::Handle::current());
            let attempt = executor.run_blocking(Box::new(|| -> WakeAdmissionAttempt { panic!("the worker produced no verdict") })).await;
            match attempt {
                WakeAdmissionAttempt::Error(RecallWakeError::Lock(message)) => assert!(!message.is_empty()),
                _ => panic!("a worker that produced no verdict must be reported as the lock-domain error"),
            }
        });
    }
}
