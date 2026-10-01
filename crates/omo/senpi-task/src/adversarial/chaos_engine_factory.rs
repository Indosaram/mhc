//! Rust port of `__adversarial__/chaos-engine-factory.ts`: wires three independent manager +
//! lifecycle engines against the shared chaos process table and store, mirroring the
//! `manager_tests::fakes::make_lifecycle_manager` composition pattern used across this crate.
//!
//! The TS source is test-only harness code, so the module is compiled only for tests.
#![cfg(test)]

use std::sync::{Arc, Mutex, PoisonError};

use crate::host::HostError;
use crate::lifecycle::port::{ResidencyRegistry, ResidentHandle, ResidentKind};
use crate::lifecycle::{
    AdmissionResult, DestroyCause, LifecycleDeps, TaskLifecycle, TaskSettings, create_task_lifecycle,
};
use crate::manager::types::{
    ChildPlanner, ManagedRunner, ManagedRunners, ManagerConfig, ManagerStartSpec, ResolvedChildPlan,
    SpawnAdmission, TaskManagerOptions,
};
use crate::manager::{ManagedChildHandle, TaskManager, create_task_manager};
use crate::steering::DestructionPort;
use crate::store::TaskRecordStore;

use super::chaos_engine::{ChaosProcessTable, ChaosRpcRespawnRunner, ChaosRunner, LifecycleChaosObservations};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct EngineResident(Arc<dyn ManagedChildHandle>);

impl ResidentHandle for EngineResident {
    fn task_id(&self) -> &str {
        self.0.task_id()
    }

    fn kind(&self) -> ResidentKind {
        if self.0.pid().is_none() {
            ResidentKind::InProcess
        } else {
            ResidentKind::Rpc
        }
    }

    fn pid(&self) -> Option<i64> {
        self.0.pid()
    }

    fn abort(&self) -> Result<(), HostError> {
        self.0.abort()
    }

    fn dispose(&self) -> Result<(), HostError> {
        let handle = self.0.as_ref();
        let dispose = <dyn ManagedChildHandle>::dispose;
        dispose(handle)
    }

    fn terminate(&self) -> Result<(), HostError> {
        if self.0.pid().is_some() {
            self.0.abort()
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
struct EngineRegistry(Mutex<Option<TaskManager>>);

impl EngineRegistry {
    fn manager(&self) -> TaskManager {
        lock(&self.0)
            .clone()
            .expect("engine manager accessed before composition finished")
    }
}

impl ResidencyRegistry for EngineRegistry {
    fn get(&self, task_id: &str) -> Option<Arc<dyn ResidentHandle>> {
        self.manager()
            .get_resident_handle(task_id)
            .map(|handle| Arc::new(EngineResident(handle)) as Arc<dyn ResidentHandle>)
    }

    fn entries(&self) -> Vec<Arc<dyn ResidentHandle>> {
        let manager = self.manager();
        manager
            .resident_task_ids()
            .iter()
            .filter_map(|task_id| manager.get_resident_handle(task_id))
            .map(|handle| Arc::new(EngineResident(handle)) as Arc<dyn ResidentHandle>)
            .collect()
    }

    fn forget(&self, task_id: &str) {
        self.manager().forget(task_id);
    }

    fn has_pending_sends(&self, _task_id: &str) -> bool {
        false
    }
}

struct EngineDestruction(Arc<Mutex<Option<Arc<TaskLifecycle>>>>);

impl DestructionPort for EngineDestruction {
    fn destroy_resident_task(&self, task_id: &str, _cause: DestroyCause) -> Result<(), HostError> {
        let lifecycle = lock(&self.0).clone().expect("engine lifecycle composed");
        lifecycle
            .destroy_resident_task(task_id, DestroyCause::Cancel)
            .map_err(|error| HostError {
                message: error.to_string(),
            })
    }
}

/// One chaos engine: an independent manager + lifecycle pair sharing the chaos store and process
/// table, matching TS `ChaosEngine`.
pub struct ChaosEngine {
    pub id: String,
    pub host_pid: i64,
    pub manager: TaskManager,
    pub lifecycle: Arc<TaskLifecycle>,
    /// TS `ChaosEngine.registry`; kept for parity with the TS engine surface (no Rust reader).
    #[allow(dead_code)]
    pub registry: Arc<dyn ResidencyRegistry>,
    pub in_process_runner: Arc<ChaosRunner>,
    pub process_runner: Arc<ChaosRunner>,
}

fn single_model_planner(model: &str) -> ChildPlanner {
    let model = model.to_string();
    Arc::new(move |spec: &ManagerStartSpec| {
        Ok(ResolvedChildPlan {
            model: spec.model.clone().unwrap_or_else(|| model.clone()),
            ..ResolvedChildPlan::default()
        })
    })
}

pub struct BuildChaosEnginesInput {
    pub store: Arc<TaskRecordStore>,
    pub config: TaskSettings,
    pub manager_config: ManagerConfig,
    pub cwd: String,
    pub model: String,
    pub processes: Arc<ChaosProcessTable>,
    pub observations: Arc<LifecycleChaosObservations>,
}

/// `buildChaosEngines`: builds three engines (`engine-1`..`engine-3`), each with its own manager,
/// lifecycle, in-process/process chaos runners and host pid (10_000 + index), all reading and
/// writing the one shared store and process table.
pub fn build_chaos_engines(input: BuildChaosEnginesInput) -> Vec<ChaosEngine> {
    let mut engines = Vec::with_capacity(3);
    for index in 0..3 {
        let id = format!("engine-{}", index + 1);
        let host_pid = 10_000 + i64::from(index);
        input.processes.insert_alive(host_pid);

        let registry = Arc::new(EngineRegistry::default());
        let respawn_registry = Arc::clone(&registry);
        let reattach_registry = Arc::clone(&registry);
        let lifecycle = Arc::new(create_task_lifecycle(LifecycleDeps {
            host_pid: Some(host_pid),
            orphan_kill_delay_ms: Some(0),
            signaller: Some(Arc::clone(&input.processes) as Arc<dyn crate::lifecycle::ProcessSignaller>),
            respawn: Some(Arc::new(move |record: &crate::state::TaskRecord, session_path: Option<&std::path::Path>| {
                respawn_registry.manager().respawn(record, session_path)
            })),
            reattach: Some(Arc::new(move |record: &crate::state::TaskRecord, handle: Arc<dyn ManagedChildHandle>| {
                reattach_registry.manager().reattach(record, handle)
            })),
            ..LifecycleDeps::new(
                Arc::clone(&input.store) as Arc<dyn crate::lifecycle::LifecycleStore>,
                Arc::clone(&registry) as Arc<dyn ResidencyRegistry>,
                input.config.clone(),
            )
        }));
        let lifecycle_slot = Arc::new(Mutex::new(Some(Arc::clone(&lifecycle))));

        let in_process_runner = ChaosRunner::new(
            &id,
            "in-process",
            Arc::clone(&input.store),
            Arc::clone(&input.processes),
            Arc::clone(&input.observations),
        );
        let process_runner = ChaosRunner::new(
            &id,
            "process",
            Arc::clone(&input.store),
            Arc::clone(&input.processes),
            Arc::clone(&input.observations),
        );

        let mut manager_options = TaskManagerOptions::new(
            (*input.store).clone(),
            ManagedRunners {
                in_process: Arc::clone(&in_process_runner) as Arc<dyn ManagedRunner>,
                process: Arc::clone(&process_runner) as Arc<dyn ManagedRunner>,
            },
            single_model_planner(&input.model),
            input.cwd.clone(),
        );
        manager_options.config = input.manager_config.clone();
        manager_options.host_pid = Some(host_pid);
        manager_options.destruction = Some(Arc::new(EngineDestruction(lifecycle_slot)));
        manager_options.rpc_respawn_runner = Some(ChaosRpcRespawnRunner::new(
            Arc::clone(&input.store),
            Arc::clone(&input.processes),
            Arc::clone(&input.observations),
        ));
        let admit_lifecycle = Arc::clone(&lifecycle);
        manager_options.admit = Some(Arc::new(move |parent: &str| {
            match admit_lifecycle.admit_resident(parent) {
                Ok(AdmissionResult::Admitted) => SpawnAdmission::Admitted,
                Ok(AdmissionResult::Evicted { evicted_task_id }) => {
                    SpawnAdmission::Evicted { evicted_task_id }
                }
                Ok(AdmissionResult::Rejected(error)) => SpawnAdmission::Rejected {
                    message: error.to_string(),
                },
                Err(error) => SpawnAdmission::Rejected {
                    message: error.to_string(),
                },
            }
        }));

        let manager = create_task_manager(manager_options);
        *lock(&registry.0) = Some(manager.clone());

        engines.push(ChaosEngine {
            id,
            host_pid,
            manager,
            lifecycle,
            registry: registry as Arc<dyn ResidencyRegistry>,
            in_process_runner,
            process_runner,
        });
    }
    engines
}
