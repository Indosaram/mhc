//! Port of the host half of senpi `omo-senpi/src/extension/index.ts` composition.

use std::path::PathBuf;
use std::sync::Arc;

use maho_ext_api::ExtensionActions;
use maho_omo::OmoSenpiComponent;
use maho_omo_memory::composition::MemoryExtensionOptions;
use maho_omo_memory::index::{MemoryComponent, MemoryComponentOptions};
use maho_omo_memory::wiring::{MemoryWiring, create_memory_wiring};
use maho_omo_memory::wiring_static::MemoryStaticOptions;
use maho_omo_memory::wiring_types::{MemoryWiringOptions, NativeMemoryWiringOptions};
use maho_omo_task::engine::{ComposeTaskEngineDeps, TaskEngine, compose_task_engine};
use maho_omo_task::parent_notifier::CompletionCoordinator;
use senpi_task::manager::types::ManagedRunners;
use serde_json::Value;

use super::omo_mount::{memory_entry, task_component};

pub struct MemoryInputs {
    pub wiring_options: fn() -> NativeMemoryWiringOptions,
    pub static_options: fn() -> MemoryStaticOptions,
}

pub struct TaskInputs {
    pub wiring: TaskWiring,
    pub spawn: senpi_task::tools::task::execute_spec::TaskToolDeps,
    pub ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps,
}

pub fn component_options(cwd: &PathBuf, env: &std::collections::BTreeMap<String, String>) -> maho_omo::OmoComponentOptions {
    maho_omo::OmoComponentOptions {
        skills_root: maho_omo::builtin_skills_root(),
        state_dir: cwd.join(".maho"),
        env: env.clone(),
    }
}

pub struct MemoryStore {
    pub component: Arc<MemoryComponent>,
    pub wiring: Arc<tokio::sync::Mutex<MemoryWiring>>,
}

impl MemoryStore {
    pub fn new(cwd: &PathBuf, env: std::collections::BTreeMap<String, String>, config: Arc<dyn Fn() -> Result<Value, String> + Send + Sync>) -> Self {
        let component = MemoryComponent::new(MemoryComponentOptions {
            env,
            load_config: config,
            cwd: cwd.clone(),
            now: Arc::new(maho_ai::utils::diagnostics::now_ms),
            disabled: Arc::new(|| false),
        });
        let wiring = Arc::new(tokio::sync::Mutex::new(create_memory_wiring(MemoryWiringOptions {
            runtime: Default::default(),
            skills_usage: Default::default(),
        })));
        Self { component, wiring }
    }

    pub fn entry(
        &self,
        wiring_options: fn() -> NativeMemoryWiringOptions,
        static_options: fn() -> MemoryStaticOptions,
    ) -> OmoSenpiComponent {
        let component = Arc::clone(&self.component);
        let wiring = Arc::clone(&self.wiring);
        memory_entry(move || MemoryExtensionOptions {
            component: Arc::clone(&component),
            wiring: Arc::clone(&wiring),
            wiring_options: wiring_options(),
            static_options: static_options(),
        })
    }
}

pub struct TaskWiring {
    pub cwd: PathBuf,
    pub config: Value,
    pub runners: ManagedRunners,
    pub resolve_registry: maho_omo_task::planner::ResolveModelRegistry,
    pub coordinator: Option<Arc<dyn CompletionCoordinator>>,
}

pub fn compose_task_engine_for_session(wiring: TaskWiring, actions: Arc<dyn ExtensionActions>) -> TaskEngine {
    compose_task_engine(ComposeTaskEngineDeps {
        cwd: wiring.cwd,
        config: wiring.config,
        runners: wiring.runners,
        actions,
        coordinator: wiring.coordinator,
        resolve_registry: wiring.resolve_registry,
    })
}

pub fn task_entry(engine: TaskEngine, spawn: senpi_task::tools::task::execute_spec::TaskToolDeps, ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps) -> OmoSenpiComponent {
    task_component(engine, spawn, ownership)
}
