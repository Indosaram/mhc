//! Production composition over the current public manager and lifecycle ports.
use std::{path::PathBuf, sync::{Arc, Mutex, OnceLock, PoisonError}};
use serde_json::Value;
use senpi_task::{completion::{CompletionNotifier, CompletionNotifierDeps, create_completion_notifier}, lifecycle::{LifecycleDeps, TaskLifecycle, TaskSettings, create_task_lifecycle}, manager::{TaskManager, types::{ManagedRunners, TaskManagerOptions, ManagerConfig, SpawnAdmission}}, store::{StateDirConfig, TaskRecordStore}};
use crate::{runtime_context::TaskRuntimeContext, parent_notifier::{TaskParentNotifier, CompletionCoordinator}, residency_registry::ManagerResidencyRegistry, lifecycle_adapters::{TaskLifecycleDestruction, admit_lifecycle}, planner::create_task_child_planner, engine_runners::resolve_task_agents};

pub struct ComposeTaskEngineDeps {
    pub cwd: PathBuf,
    pub config: Value,
    pub runners: ManagedRunners,
    pub actions: Arc<dyn maho_ext_api::ExtensionActions>,
    pub coordinator: Option<Arc<dyn CompletionCoordinator>>,
    pub resolve_registry: crate::planner::ResolveModelRegistry,
}
pub struct TaskEngine {
    pub manager: Arc<TaskManager>,
    pub lifecycle: Arc<TaskLifecycle>,
    pub notifier: CompletionNotifier,
    pub runtime: Arc<Mutex<TaskRuntimeContext>>,
    pub store: TaskRecordStore,
    pub config: Value,
    pub agents: std::collections::BTreeMap<String, senpi_task::agents::AgentDefinition>,
}

pub fn compose_task_engine(deps: ComposeTaskEngineDeps) -> TaskEngine {
    compose_task_engine_with_rpc_respawn(deps, None)
}
pub fn compose_task_engine_with_rpc_respawn(deps: ComposeTaskEngineDeps, rpc_respawn: Option<Arc<dyn senpi_task::manager::types::RpcRespawnRunner>>) -> TaskEngine {
    let settings = deps.config.get("task").cloned().unwrap_or_else(|| serde_json::json!({}));
    let store = TaskRecordStore::new(&StateDirConfig { project_dir: deps.cwd.clone(), task_state_dir: settings["state_dir"].as_str().map(PathBuf::from) });
    let runtime = Arc::new(Mutex::new(TaskRuntimeContext::new(deps.cwd.clone())));
    let state = runtime.clone();
    let actions = deps.actions.clone();
    let parent = Arc::new(TaskParentNotifier { actions: deps.actions, coordinator: deps.coordinator, is_streaming: Arc::new(move || state.lock().unwrap_or_else(PoisonError::into_inner).parent_state() == senpi_task::completion::ParentState::Streaming) });
    let mut notification = CompletionNotifierDeps::new(parent, Arc::new(store.clone()));
    notification.state_dir = Some(store.state_dir().into());
    let state = runtime.clone();
    notification.get_parent_state = Some(Arc::new(move || state.lock().unwrap_or_else(PoisonError::into_inner).parent_state()));
    let state = runtime.clone();
    notification.get_current_session_id = Some(Arc::new(move || state.lock().unwrap_or_else(PoisonError::into_inner).session_id().map(str::to_owned)));
    let notifier = create_completion_notifier(notification);
    let manager_ref = Arc::new(OnceLock::<std::sync::Weak<TaskManager>>::new());
    let registry_ref = manager_ref.clone();
    let registry = Arc::new(ManagerResidencyRegistry { get_manager: Arc::new(move || match registry_ref.get().and_then(std::sync::Weak::upgrade) { Some(manager) => (*manager).clone(), None => panic!("task manager accessed outside composed lifetime") }) });
    let lifecycle = Arc::new(create_task_lifecycle(LifecycleDeps::new(Arc::new(store.clone()), registry, TaskSettings::from_resolved(&settings))));
    let agents = resolve_task_agents(&deps.config);
    let planner = create_task_child_planner(deps.config.clone(), agents.clone().into_iter().collect(), deps.resolve_registry);
    let state = runtime.clone();
    let warning_state = runtime.clone();
    let planner = crate::category_unavailable_warning::create_category_unavailable_warning_planner(planner, deps.config.clone(), settings.clone(), Arc::new(move || state.lock().unwrap_or_else(PoisonError::into_inner).session_id().map(str::to_owned)), Arc::new(move |text, details| {
        let ui = warning_state.lock().unwrap_or_else(PoisonError::into_inner).ui().cloned();
        if let Err(error) = crate::category_unavailable_warning::deliver_category_warning(actions.as_ref(), ui.as_deref(), text, details) { eprintln!("task category warning failed: {error}"); }
    }));
    let mut options = TaskManagerOptions::new(store.clone(), deps.runners, planner, deps.cwd.to_string_lossy());
    options.rpc_respawn_runner = rpc_respawn;
    options.config = ManagerConfig { max_depth: settings["max_depth"].as_u64().and_then(|depth| u32::try_from(depth).ok()).unwrap_or(1), default_execution_mode: settings["default_execution_mode"].as_str().and_then(senpi_task::manager::execution_mode::ExecutionMode::parse).unwrap_or_default(), ..ManagerConfig::default() };
    let concurrency = &deps.config["background_task"];
    options.config.concurrency = senpi_task::manager::concurrency::TaskConcurrencyConfig {
        default_concurrency: concurrency["defaultConcurrency"].as_u64().and_then(|value| usize::try_from(value).ok()),
        provider_concurrency: concurrency["providerConcurrency"].as_object().map(|values| values.iter().filter_map(|(name,value)| value.as_u64().and_then(|value| usize::try_from(value).ok()).map(|value| (name.clone(),value))).collect()),
        model_concurrency: concurrency["modelConcurrency"].as_object().map(|values| values.iter().filter_map(|(name,value)| value.as_u64().and_then(|value| usize::try_from(value).ok()).map(|value| (name.clone(),value))).collect()),
    };
    options.destruction = Some(Arc::new(TaskLifecycleDestruction((*lifecycle).clone())));
    let admission = lifecycle.clone();
    // The manager's admission callback cannot return a lifecycle error. Reject rather
    // than admitting a child after failed residency bookkeeping.
    options.admit = Some(Arc::new(move |session| admit_lifecycle(&admission, session).unwrap_or_else(|error| SpawnAdmission::Rejected { message: error.to_string() })));
    let manager = Arc::new(TaskManager::new(options));
    let _ = manager_ref.set(Arc::downgrade(&manager));
    TaskEngine { manager, lifecycle, notifier, runtime, store, config: deps.config, agents }
}
