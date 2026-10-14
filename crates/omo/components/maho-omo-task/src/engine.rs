//! Production composition over the current public manager and lifecycle ports.
use std::{path::PathBuf, sync::{Arc, Mutex, OnceLock, PoisonError}};
use serde_json::Value;
use senpi_task::{completion::{CompletionNotifier, CompletionNotifierDeps, create_completion_notifier}, lifecycle::{LifecycleDeps, TaskLifecycle, TaskSettings, create_task_lifecycle}, manager::{TaskManager, types::{ManagedRunners, TaskManagerOptions, ManagerConfig}}, store::{StateDirConfig, TaskRecordStore}};
use crate::{runtime_context::TaskRuntimeContext, parent_notifier::{TaskParentNotifier, CompletionCoordinator}, residency_registry::ManagerResidencyRegistry, lifecycle_adapters::{TaskLifecycleDestruction, admit_lifecycle}, planner::create_task_child_planner, engine_runners::resolve_task_agents};

pub struct ComposeTaskEngineDeps {
    pub cwd: PathBuf,
    pub config: Value,
    pub runners: ManagedRunners,
    pub actions: Arc<dyn maho_ext_api::ExtensionActions>,
    pub coordinator: Option<Arc<dyn CompletionCoordinator>>,
    pub resolve_registry: crate::planner::ResolveModelRegistry,
    pub host_transport: Option<Arc<dyn senpi_task::lifecycle::HostTransport>>,
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
    compose_task_engine_with_clock(deps, rpc_respawn, None)
}
/// Composes the engine with an explicit lifecycle clock. Production callers keep the system clock
/// through [`compose_task_engine`]; deterministic recovery and expiry tests inject a fixed clock
/// through the existing `LifecycleContext::now` seam so a disposal timestamp and the TTL cutoff can
/// never race the wall clock.
pub fn compose_task_engine_with_clock(deps: ComposeTaskEngineDeps, rpc_respawn: Option<Arc<dyn senpi_task::manager::types::RpcRespawnRunner>>, now: Option<senpi_task::lifecycle::context::NowFn>) -> TaskEngine {
    let settings = deps.config.get("task").cloned().unwrap_or_else(|| serde_json::json!({}));
    let isolation_settings = senpi_task::manager::isolation_wiring::IsolationSettings::from_config(&settings);
    let isolation_runtime = senpi_task::isolation::create_isolation_runtime(&senpi_task::isolation::IsolationRuntimeOptions::default());
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
    let mut lifecycle_deps = LifecycleDeps::new(Arc::new(store.clone()), registry, TaskSettings::from_resolved(&settings));
    lifecycle_deps.now = now;
    lifecycle_deps.host_transport = deps.host_transport;
    lifecycle_deps.isolation = Some(isolation_runtime.clone());
    let lifecycle = Arc::new(create_task_lifecycle(lifecycle_deps));
    let agents = resolve_task_agents(&deps.config);
    let planner = create_task_child_planner(deps.config.clone(), agents.clone().into_iter().collect(), deps.resolve_registry);
    let state = runtime.clone();
    let warning_state = runtime.clone();
    let planner = crate::category_unavailable_warning::create_category_unavailable_warning_planner(planner, deps.config.clone(), settings.clone(), Arc::new(move || state.lock().unwrap_or_else(PoisonError::into_inner).session_id().map(str::to_owned)), Arc::new(move |text, details| {
        let ui = warning_state.lock().unwrap_or_else(PoisonError::into_inner).ui().cloned();
        if let Err(error) = crate::category_unavailable_warning::deliver_category_warning(actions.as_ref(), ui.as_deref(), text, details) { maho_ext_api::diagnostic(&format!("task category warning failed: {error}")); }
    }));
    let mut options = TaskManagerOptions::new(store.clone(), deps.runners, planner, deps.cwd.to_string_lossy());
    options.rpc_respawn_runner = rpc_respawn;
    options.isolation = Some(isolation_runtime);
    options.isolation_settings = isolation_settings;
    options.config = ManagerConfig { max_depth: settings["max_depth"].as_u64().and_then(|depth| u32::try_from(depth).ok()).unwrap_or(1), default_execution_mode: settings["default_execution_mode"].as_str().and_then(senpi_task::manager::execution_mode::ExecutionMode::parse).unwrap_or_default(), ..ManagerConfig::default() };
    options.config.concurrency = senpi_task::manager::concurrency::TaskConcurrencyConfig {
        default_concurrency: settings["default_concurrency"].as_u64().and_then(|value| usize::try_from(value).ok()),
        provider_concurrency: settings["provider_concurrency"].as_object().map(|values| values.iter().filter_map(|(name,value)| value.as_u64().and_then(|value| usize::try_from(value).ok()).map(|value| (name.clone(),value))).collect()),
        model_concurrency: settings["model_concurrency"].as_object().map(|values| values.iter().filter_map(|(name,value)| value.as_u64().and_then(|value| usize::try_from(value).ok()).map(|value| (name.clone(),value))).collect()),
    };
    options.destruction = Some(Arc::new(TaskLifecycleDestruction((*lifecycle).clone())));
    options.trusted_respawn_launch = trusted_respawn_launch(&deps.cwd, &settings);
    let admission = lifecycle.clone();
    // Bookkeeping failure must remain a launch failure, never a retryable capacity denial.
    options.fallible_admit = Some(Arc::new(move |session| admit_lifecycle(&admission, session).map_err(|error| senpi_task::host::HostError { message: error.to_string() })));
    let manager = Arc::new(TaskManager::new(options));
    let _ = manager_ref.set(Arc::downgrade(&manager));
    TaskEngine { manager, lifecycle, notifier, runtime, store, config: deps.config, agents }
}

/// The `--extension` entry a team member process loads the native member extension through
/// (upstream `resolveMemberExtensionEntryPath()`, the `omo-member.js` bundle).
pub const MEMBER_EXTENSION_ENTRY: &str = "builtin:task-member";

/// omo `task.team` bounds with the omo-config schema defaults.
pub fn task_team_bounds(settings: &Value) -> senpi_task::team::runtime_config::TeamTaskBounds {
    let team = &settings["team"];
    let bound = |key: &str, default: u64| team[key].as_u64().unwrap_or(default);
    senpi_task::team::runtime_config::TeamTaskBounds { max_members: bound("max_members", 8), max_parallel_members: bound("max_parallel_members", 4), max_wall_clock_minutes: bound("max_wall_clock_minutes", 120) }
}

/// `trustedRespawnLaunch: createTeamMemberRespawnLaunchResolver({ stateDir, taskSettings, memberExtension })`.
fn trusted_respawn_launch(cwd: &std::path::Path, settings: &Value) -> Option<senpi_task::manager::types::TrustedRespawnLaunchResolver> {
    use senpi_task::{manager::types::{ManagedRunnerError, TrustedRespawnLaunch}, team::{member_respawn::{TeamMemberRespawnLaunchResolverOptions, create_team_member_respawn_launch_resolver}, runtime_types::TeamMemberExtensionConfig}};
    let resolver = create_team_member_respawn_launch_resolver(TeamMemberRespawnLaunchResolverOptions {
        state_dir: StateDirConfig { project_dir: cwd.to_path_buf(), task_state_dir: settings["state_dir"].as_str().map(PathBuf::from) },
        team_bounds: task_team_bounds(settings),
        member_extension: TeamMemberExtensionConfig {
            entry_path: MEMBER_EXTENSION_ENTRY.into(),
            inherited_extensions: Some(senpi_task::runners::rpc::parent_extensions::parse_extension_entries(&std::env::args().collect::<Vec<_>>())),
        },
    });
    let resolver = match resolver {
        Ok(resolver) => resolver,
        Err(error) => { maho_ext_api::diagnostic(&format!("task team respawn launch resolver unavailable: {error}")); return None; }
    };
    Some(Arc::new(move |record| match resolver.resolve(record.name.as_deref(), &record.task_id) {
        Ok(launch) => Ok(Some(TrustedRespawnLaunch { extensions: Some(launch.extensions), member_env: launch.member_env })),
        Err(error) => Err(ManagedRunnerError::TeamRespawnLaunch { code: error.code.as_str().into() }),
    }))
}
