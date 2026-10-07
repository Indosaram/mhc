//! manager/isolation-wiring.ts: the seam between the manager's start/outcome paths and isolation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use isolation_core::{BackendKind, IsolationHandle, IsolationOwner, OwnerChild, WorktreeBaseline};

use crate::isolation::{
    IsolationPreparation, IsolationRuntime, PrepareIsolationInput, SettleIsolationInput,
    prepare_isolation, settle_isolation,
};
use crate::manager::child_handle::ManagedChildHandle;
use crate::manager::types::ManagerStartSpec;
use crate::state::{IsolationMergeMode, IsolationRecord};
use crate::store::TaskRecordStore;

/// The isolation settings the wiring reads (port of the OmoTaskSettings isolation block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsolationSettings {
    pub enabled: bool,
    /// The configured backend. 'None' is the config schema's "auto" default - let the producer
    /// choose; a concrete value pins the choice.
    pub backend: Option<BackendKind>,
    pub merge: IsolationMergeMode,
    pub apply: bool,
}

impl IsolationSettings {
    /// Reads the isolation block of the RESOLVED 'task' settings, exactly like
    /// 'lifecycle::settings::TaskSettings::from_resolved': 'omo-config-core' has already applied its
    /// schema defaults ('enabled:false', 'backend:"auto"', 'apply:true', 'merge:"patch"'), so this
    /// consumes the resolved values and never invents one. The config-only sentinel "auto" (or any
    /// value that is not a concrete backend) becomes 'None', which the producer reads as "choose the
    /// backend". 'commits' is not a manager concern: the pinned settle path passes 'commit_message: None'.
    pub fn from_config(resolved: &serde_json::Value) -> Self {
        let isolation = resolved.get("isolation");
        let field = |key: &str| isolation.and_then(|block| block.get(key));
        Self {
            enabled: field("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            backend: field("backend")
                .and_then(serde_json::Value::as_str)
                .and_then(BackendKind::parse),
            merge: match field("merge").and_then(serde_json::Value::as_str) {
                Some("branch") => IsolationMergeMode::Branch,
                _ => IsolationMergeMode::Patch,
            },
            apply: field("apply")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
        }
    }
}

impl Default for IsolationSettings {
    fn default() -> Self {
        // The config schema's own defaults (enabled false, backend "auto", apply true, merge
        // "patch"), so the port never invents a value the immutable contract does not have.
        Self::from_config(&serde_json::Value::Null)
    }
}

/// The ports a wiring writes through (TS IsolationWiringPorts).
pub struct IsolationWiringPorts {
    pub runtime: Option<Arc<dyn IsolationRuntime>>,
    pub store: TaskRecordStore,
    pub cwd: String,
    pub host_pid: i64,
    pub settings: IsolationSettings,
}

struct Binding {
    handle: IsolationHandle,
    baseline: WorktreeBaseline,
}

/// The isolation capability the manager drives (TS IsolationWiring).
pub struct IsolationWiring {
    runtime: Option<Arc<dyn IsolationRuntime>>,
    store: TaskRecordStore,
    cwd: String,
    host_pid: i64,
    settings: IsolationSettings,
    bindings: Mutex<HashMap<String, Binding>>,
}

pub fn create_isolation_wiring(ports: IsolationWiringPorts) -> IsolationWiring {
    IsolationWiring {
        runtime: ports.runtime,
        store: ports.store,
        cwd: ports.cwd,
        host_pid: ports.host_pid,
        settings: ports.settings,
        bindings: Mutex::new(HashMap::new()),
    }
}

impl IsolationWiring {
    fn bindings(&self) -> std::sync::MutexGuard<'_, HashMap<String, Binding>> {
        self.bindings.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether this spawn asks for isolation (TS isolates).
    pub fn isolates(&self, spec: &ManagerStartSpec) -> bool {
        spec.isolated.unwrap_or(self.settings.enabled)
    }

    /// Build the sandbox before the record is committed to a launch.
    pub fn prepare(&self, spec: &ManagerStartSpec, task_id: &str) -> IsolationPreparation {
        let Some(runtime) = self.runtime.as_deref() else {
            return IsolationPreparation::Refused {
                reason: "no isolation runtime is wired into this session".to_string(),
            };
        };
        prepare_isolation(&PrepareIsolationInput {
            runtime,
            cwd: std::path::Path::new(&self.cwd),
            task_id,
            state_dir: self.store.state_dir(),
            backend: self.settings.backend,
            mode: spec.merge.unwrap_or(self.settings.merge),
            apply: spec.apply.unwrap_or(self.settings.apply),
            host_pid: self.host_pid,
        })
    }

    pub fn bind(&self, task_id: &str, handle: IsolationHandle, baseline: WorktreeBaseline) {
        self.bindings().insert(task_id.to_string(), Binding { handle, baseline });
    }

    /// Release a binding without a merge (a start that failed after prepare).
    pub fn discard(&self, task_id: &str) {
        let binding = self.bindings().remove(task_id);
        let (Some(binding), Some(runtime)) = (binding, self.runtime.as_deref()) else {
            return;
        };
        runtime.cleanup(&binding.handle);
    }

    /// Stamp the child's own identity onto the clone so a sweep can tell a live child from a dead host.
    pub fn stamp(&self, task_id: &str, handle: &dyn ManagedChildHandle) {
        let child = match handle.pid() {
            Some(pid) => u32::try_from(pid).ok().map(|pid| OwnerChild::Process { pid }),
            None => None,
        };
        let (Some(child), Some(runtime)) = (child, self.runtime.as_deref()) else {
            return;
        };
        let base_dir = {
            let bindings = self.bindings();
            let Some(binding) = bindings.get(task_id) else {
                return;
            };
            binding.handle.base_dir.clone()
        };
        runtime.write_owner(
            &base_dir,
            task_id,
            &IsolationOwner {
                host: isolation_core::HostOwner {
                    pid: u32::try_from(self.host_pid).unwrap_or_else(|_| std::process::id()),
                },
                child: Some(child),
            },
        );
    }

    /// Merge (or retain) the child's clone and record the result on the record.
    pub fn settle(&self, task_id: &str, merge: bool) {
        let Some(runtime) = self.runtime.as_deref() else {
            return;
        };
        let Ok(Some(record)) = self.store.load(task_id) else {
            return;
        };
        let Some(isolation) = record.isolation.as_ref() else {
            return;
        };
        if isolation.merge_result.is_some() {
            return;
        }
        let spec = isolation.spec.clone();
        let binding = self.bindings().remove(task_id);
        let reason = (!merge).then_some("child did not complete");
        let result = settle_isolation(&SettleIsolationInput {
            runtime,
            state_dir: self.store.state_dir(),
            task_id,
            isolation: &spec,
            merge,
            reason,
            baseline: binding.as_ref().map(|binding| &binding.baseline),
            handle: binding.as_ref().map(|binding| &binding.handle),
        });
        if let Err(error) = self.store.mutate(task_id, |fresh| match fresh.isolation.as_ref() {
            None => fresh.clone(),
            Some(current) => crate::state::TaskRecord {
                isolation: Some(IsolationRecord {
                    spec: current.spec.clone(),
                    merge_result: Some(result.clone()),
                }),
                ..fresh.clone()
            },
        }) {
            utils::logger::log(
                "senpi-task isolation settle failed",
                Some(&serde_json::json!({ "taskId": task_id, "error": error.to_string() })),
            );
        }
    }

    /// The state dir the manager wires the artifacts through.
    pub fn state_dir(&self) -> PathBuf {
        self.store.state_dir().to_path_buf()
    }
}
