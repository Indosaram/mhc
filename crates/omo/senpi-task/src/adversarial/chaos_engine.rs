//! Rust port of `__adversarial__/chaos-engine.ts`: the fake process table and the scripted
//! in-process/RPC runners the chaos bench launches children through.
//!
//! The TS source is test-only harness code, so the module is compiled only for tests.
#![cfg(test)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::lifecycle::{OrphanSignal, ProcessSignaller};
use crate::manager::manager_tests::fakes::FakeHandle;
use crate::manager::types::{ManagedRunner, ManagedRunnerResult, ManagedStartSpec};
use crate::runners::types::{RpcRunnerSpec, RpcSpawnSpec};
use crate::store::TaskRecordStore;

/// `LifecycleChaosObservations`: counters and breach logs the lifecycle-focused chaos actions and
/// invariant checks read and write.
#[derive(Default)]
pub struct LifecycleChaosObservations {
    pub action_counts: Mutex<ActionCounts>,
    pub live_handle_breaches: Mutex<Vec<String>>,
    pub pid_breaches: Mutex<Vec<String>>,
    pub reclamation_breaches: Mutex<Vec<String>>,
    pub terminal_relaunches: Mutex<Vec<String>>,
    pub action_trace: Mutex<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActionCounts {
    pub suspend_session: u32,
    pub resume_session: u32,
    pub crash_mid_suspend: u32,
    pub sibling_reconcile_race: u32,
    pub mass_revive_at_cap: u32,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl LifecycleChaosObservations {
    pub fn push_trace(&self, action: &str) {
        lock(&self.action_trace).push(action.to_string());
    }
}

/// `ChaosProcessTable`: a fake `ProcessSignaller` handing out monotonic fake pids starting at
/// 20_000 (kept clear of the engines' 10_000-range host pids).
#[derive(Default)]
pub struct ChaosProcessTable {
    pub alive: Mutex<std::collections::BTreeSet<i64>>,
    pub signals: Mutex<Vec<String>>,
    next_pid: Mutex<i64>,
}

impl ChaosProcessTable {
    pub fn new() -> Self {
        Self {
            alive: Mutex::default(),
            signals: Mutex::default(),
            next_pid: Mutex::new(20_000),
        }
    }

    pub fn spawn(&self) -> i64 {
        let mut next = lock(&self.next_pid);
        let pid = *next;
        *next += 1;
        lock(&self.alive).insert(pid);
        pid
    }

    pub fn is_alive_pid(&self, pid: i64) -> bool {
        lock(&self.alive).contains(&pid)
    }

    pub fn insert_alive(&self, pid: i64) {
        lock(&self.alive).insert(pid);
    }

    pub fn remove_alive(&self, pid: i64) {
        lock(&self.alive).remove(&pid);
    }
}

impl ProcessSignaller for ChaosProcessTable {
    fn is_alive(&self, pid: i64) -> bool {
        self.is_alive_pid(pid)
    }

    fn signal(&self, pid: i64, signal: OrphanSignal) {
        lock(&self.signals).push(format!("{}:{pid}", signal.as_str()));
        lock(&self.alive).remove(&pid);
    }
}

fn has_session(store: &TaskRecordStore, task_id: &str) -> bool {
    let directory = store.state_dir().join("children").join(task_id).join("sessions");
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().ends_with(".jsonl"))
}

fn observe_launch(
    _engine_id: &str,
    task_id: &str,
    store: &TaskRecordStore,
    processes: &ChaosProcessTable,
    observations: &LifecycleChaosObservations,
) {
    let record = store.load(task_id).ok().flatten();
    if let Some(record) = &record {
        let is_relaunchable_terminal = matches!(
            record.status,
            crate::state::TaskStatus::Completed
                | crate::state::TaskStatus::Error
                | crate::state::TaskStatus::Interrupted
        );
        if is_relaunchable_terminal && !has_session(store, task_id) {
            lock(&observations.terminal_relaunches).push(task_id.to_string());
        }
        if let Some(pid) = record.pid
            && processes.is_alive_pid(pid)
        {
            lock(&observations.pid_breaches).push(format!(
                "replacement for {task_id} spawned before retained pid {pid} was terminated"
            ));
        }
    }
}

/// `ChaosRunner`: a `ManagedRunner` whose handles are [`FakeHandle`]s from the manager test
/// fixtures, wired through the shared chaos process table so `process`-mode launches allocate a
/// fake pid.
pub struct ChaosRunner {
    engine_id: String,
    mode: &'static str,
    store: Arc<TaskRecordStore>,
    processes: Arc<ChaosProcessTable>,
    observations: Arc<LifecycleChaosObservations>,
    pub handles: Mutex<HashMap<String, Arc<FakeHandle>>>,
    pub started_task_ids: Mutex<Vec<String>>,
    pub resumed_task_ids: Mutex<Vec<String>>,
}

impl ChaosRunner {
    pub fn new(
        engine_id: &str,
        mode: &'static str,
        store: Arc<TaskRecordStore>,
        processes: Arc<ChaosProcessTable>,
        observations: Arc<LifecycleChaosObservations>,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine_id: engine_id.to_string(),
            mode,
            store,
            processes,
            observations,
            handles: Mutex::default(),
            started_task_ids: Mutex::default(),
            resumed_task_ids: Mutex::default(),
        })
    }

    pub fn all_handles(&self) -> Vec<Arc<FakeHandle>> {
        lock(&self.handles).values().cloned().collect()
    }

    pub fn handle(&self, task_id: &str) -> Option<Arc<FakeHandle>> {
        lock(&self.handles).get(task_id).cloned()
    }

    pub fn started_task_ids(&self) -> Vec<String> {
        lock(&self.started_task_ids).clone()
    }

    fn launch(&self, task_id: &str) -> Arc<FakeHandle> {
        observe_launch(&self.engine_id, task_id, &self.store, &self.processes, &self.observations);
        let pid = if self.mode == "process" {
            Some(self.processes.spawn())
        } else {
            None
        };
        let handle = FakeHandle::new(task_id, pid);
        lock(&self.handles).insert(task_id.to_string(), Arc::clone(&handle));
        handle
    }
}

impl ManagedRunner for ChaosRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        lock(&self.started_task_ids).push(spec.task_id.clone());
        Ok(self.launch(&spec.task_id))
    }

    fn resume(&self, spec: &ManagedStartSpec, _session_path: &str) -> Option<ManagedRunnerResult> {
        lock(&self.resumed_task_ids).push(spec.task_id.clone());
        Some(Ok(self.launch(&spec.task_id)))
    }
}

/// `ChaosRpcRespawnRunner`: the RPC respawn seam the manager calls into on process-mode respawn.
pub struct ChaosRpcRespawnRunner {
    store: Arc<TaskRecordStore>,
    processes: Arc<ChaosProcessTable>,
    observations: Arc<LifecycleChaosObservations>,
}

impl ChaosRpcRespawnRunner {
    pub fn new(
        store: Arc<TaskRecordStore>,
        processes: Arc<ChaosProcessTable>,
        observations: Arc<LifecycleChaosObservations>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            processes,
            observations,
        })
    }
}

impl crate::manager::types::RpcRespawnRunner for ChaosRpcRespawnRunner {
    fn start(&self, spec: &RpcRunnerSpec) -> ManagedRunnerResult {
        observe_launch(
            "rpc-respawn",
            &spec.task_id,
            &self.store,
            &self.processes,
            &self.observations,
        );
        let pid = self.processes.spawn();
        let handle = RpcRespawnHandle {
            task_id: spec.task_id.clone(),
            pid,
            processes: Arc::clone(&self.processes),
            cwd: spec.cwd.clone(),
        };
        Ok(Arc::new(handle))
    }
}

struct RpcRespawnHandle {
    task_id: String,
    pid: i64,
    processes: Arc<ChaosProcessTable>,
    cwd: String,
}

impl crate::manager::ManagedChildHandle for RpcRespawnHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn session_id(&self) -> Option<String> {
        Some(format!("rpc-{}", self.task_id))
    }

    fn pid(&self) -> Option<i64> {
        Some(self.pid)
    }

    fn spawn_spec(&self) -> Option<RpcSpawnSpec> {
        Some(RpcSpawnSpec {
            cwd: self.cwd.clone(),
            extensions: None,
            member_env: None,
        })
    }

    fn steer(&self, _text: &str) -> Result<(), crate::host::HostError> {
        Ok(())
    }

    fn follow_up(&self, _text: &str) -> Result<(), crate::host::HostError> {
        Ok(())
    }

    fn abort(&self) -> Result<(), crate::host::HostError> {
        Ok(())
    }

    fn subscribe(
        &self,
        _listener: crate::manager::ManagedChildListener,
    ) -> crate::manager::Unsubscribe {
        Box::new(|| {})
    }

    fn wait_for_outcome(&self) -> crate::runners::RunnerOutcome {
        crate::runners::RunnerOutcome::completed("rpc resumed")
    }

    fn last_assistant_text(&self) -> Option<String> {
        Some("rpc resumed".to_string())
    }

    fn has_terminate(&self) -> bool {
        true
    }

    fn terminate(&self) -> Result<(), crate::host::HostError> {
        self.processes.remove_alive(self.pid);
        Ok(())
    }

    fn dispose(&self) -> Result<(), crate::host::HostError> {
        self.processes.remove_alive(self.pid);
        Ok(())
    }
}
