//! Port of omo-senpi `components/task/dag-runtime.ts` (the DAG composition root) plus the DAG
//! wiring of `components/task/index.ts` (`registerDagTool`, `wireDagLifecycle`). One manager/store
//! graph is shared by the tool, scheduler, RPC, TUI, wake, and lifecycle adapters.
use crate::dag_runtime::DurableDagListener;
use crate::engine::TaskEngine;
use idle_injection::IdleInjectionCoordinator;
use maho_ext_api::ComponentLogger;
use senpi_task::dag::{
    manager::{DagManager, DagManagerOptions, DagRunRecordV1, create_dag_manager},
    recovery::{
        DagRecovery, DagRecoveryOptions, DagRecoveryOutcome, DagRecoverySkipReason,
        create_dag_recovery,
    },
    recovery_lease_watch::{
        DagLeaseWatch, DagLeaseWatchCancel, DagLeaseWatchOptions, create_dag_lease_watch,
    },
    scheduler::{
        DagExecutionModeOptions, DagNodeSpawnPolicy, DagNodeSpawnPolicyVerdict,
        DagSchedulerAncestry, DagSchedulerContext, DagSchedulerOptions, create_dag_scheduler,
        observe_dag_schedulers,
    },
    store::DagFileStore,
    types::{DagRunEvent, DagRunEventType, DagRunId, DagRunStatus},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, PoisonError, Weak},
    thread::JoinHandle,
};

const TERMINAL_RUN_STATUSES: [DagRunStatus; 3] = [
    DagRunStatus::Completed,
    DagRunStatus::Failed,
    DagRunStatus::Cancelled,
];
const SCHEDULABLE_RUN_STATUSES: [DagRunStatus; 2] = [DagRunStatus::Pending, DagRunStatus::Running];

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
fn failure(error: impl std::fmt::Display) -> maho_ext_api::ExtensionFailure {
    maho_ext_api::ExtensionFailure::new(error.to_string())
}

/// Writes back one run's published seq as a max: a concurrent ring subscriber may have advanced it
/// (or any other run's seq) meanwhile, and lowering it would replay already-delivered events.
fn raise_delivered(delivered: &Mutex<BTreeMap<DagRunId, u64>>, run: &DagRunId, seq: u64) {
    let mut delivered = lock(delivered);
    let entry = delivered.entry(run.clone()).or_insert(0);
    *entry = (*entry).max(seq);
}

/// TS `schedulers` map value: the scheduler plus its background `running` promise.
struct DagOwnedScheduler {
    scheduler: Arc<DagSchedulerContext>,
    running: Option<JoinHandle<()>>,
}

/// TS `RecoveryScope`.
#[derive(Clone)]
struct RecoveryScope {
    session_id: String,
    fork_source_session_id: Option<String>,
}

/// The scheduler-option inputs every scheduler this runtime builds shares (TS `schedulerOptions`).
#[derive(Clone, Default)]
pub struct DagRuntimeSchedulingOptions {
    pub ancestry: Option<DagSchedulerAncestry>,
    pub node_spawn_policy: Option<DagNodeSpawnPolicy>,
    /// Live read of the parent session's `auto` execution-mode resolution (TS
    /// `host.executionModeGate.current()`); `None` until it settles.
    pub auto_mode: Option<
        Arc<dyn Fn() -> Option<senpi_task::manager::execution_mode::ExecutionMode> + Send + Sync>,
    >,
    pub lease_watch: Option<DagLeaseWatchOptions>,
    pub retention_sweep: Option<crate::dag_retention_sweep::DagRetentionSweepScheduler>,
    pub logger: Option<Arc<dyn ComponentLogger>>,
}

pub struct TaskDagEngine {
    pub manager: DagManager,
    pub store: Arc<DagFileStore>,
    recovery: Arc<DagRecovery>,
    settings: senpi_task::dag::types::DagSettings,
    task_manager: Arc<senpi_task::manager::TaskManager>,
    agents: Arc<BTreeMap<String, senpi_task::agents::AgentDefinition>>,
    config: serde_json::Value,
    scheduling: DagRuntimeSchedulingOptions,
    runtime: Arc<Mutex<crate::runtime_context::TaskRuntimeContext>>,
    schedulers: Arc<Mutex<BTreeMap<DagRunId, DagOwnedScheduler>>>,
    stopped_admissions: Arc<Mutex<BTreeSet<DagRunId>>>,
    recovery_task_subscriptions: Arc<Mutex<BTreeMap<String, senpi_task::manager::Unsubscribe>>>,
    lease_watch: DagLeaseWatch,
    lease_watches: Mutex<BTreeMap<DagRunId, DagLeaseWatchCancel>>,
    rpc: Mutex<Option<Arc<crate::dag_rpc_bridge::DagRpcBridge>>>,
    surfaces: Mutex<Option<Arc<DagSurfaces>>>,
    active_session: Mutex<Option<String>>,
    wake: Mutex<Option<crate::dag_wake::DagWake>>,
    delivered: Mutex<BTreeMap<DagRunId, u64>>,
    /// Event-log (byte length, mtime) at the last successful durable publish per run: a log that
    /// has not changed since holds no undelivered event, so `on_checkpoint` skips re-reading it.
    published_log_len: Mutex<BTreeMap<DagRunId, (u64, Option<std::time::SystemTime>)>>,
    run_listeners: Mutex<BTreeMap<DagRunId, Vec<DurableDagListener>>>,
    terminal_wake_seq: Mutex<BTreeMap<DagRunId, u64>>,
    paused_wake_seq: Mutex<BTreeMap<DagRunId, u64>>,
    durable_listener_enabled: std::sync::atomic::AtomicBool,
    stop_observing: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    this: Mutex<Weak<TaskDagEngine>>,
}

struct DagSurfaces {
    status: Arc<crate::dag_status_ui::DagStatusUi>,
    wake: crate::dag_wake_source::DagWakeSource,
    activity: Mutex<BTreeMap<String, (String, String, senpi_task::manager::Unsubscribe)>>,
}
impl DagSurfaces {
    fn clear_activity(&self) {
        let subscriptions = std::mem::take(&mut *lock(&self.activity));
        for (_, _, unsubscribe) in subscriptions.into_values() {
            unsubscribe();
        }
    }
    fn bind_activity(
        self: &Arc<Self>,
        run_id: &str,
        node_id: &str,
        task_id: &str,
        tasks: &senpi_task::manager::TaskManager,
        bridge: &Arc<crate::dag_rpc_bridge::DagRpcBridge>,
    ) {
        if lock(&self.activity).contains_key(task_id) {
            return;
        }
        let Some(record) = tasks.get(task_id) else {
            return;
        };
        let started_at = chrono::DateTime::parse_from_rfc3339(&record.created_at)
            .map_or(0, |date| date.timestamp_millis().max(0) as u64);
        let progress = Mutex::new(senpi_task::progress::create_child_progress(
            task_id,
            senpi_task::progress::ChildProgressTarget {
                name: record.name,
                task_summary: record.task_summary,
                description: record.description,
                category: record.category,
                agent_type: record.agent_type,
                resolved_model: record.resolved_model,
                model: Some(record.model),
            },
            started_at,
            || chrono::Utc::now().timestamp_millis().max(0) as u64,
        ));
        let status = Arc::downgrade(&self.status);
        let bridge = Arc::downgrade(bridge);
        let run = run_id.to_owned();
        let node = node_id.to_owned();
        let task = task_id.to_owned();
        let unsubscribe = tasks.subscribe_child(
            task_id,
            Arc::new(move |event| {
                let details = {
                    let mut progress = lock(&progress);
                    if !progress.accept(event) {
                        return;
                    }
                    progress.details()
                };
                if let Some(bridge) = bridge.upgrade() {
                    let at =
                        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                    bridge.publish_activity(crate::dag_runtime::dag_activity_payload(
                        &run, &node, &task, &at, &details,
                    ));
                }
                if let Some(status) = status.upgrade() {
                    status.on_activity(
                        &run,
                        &node,
                        details
                            .current_tool
                            .as_deref()
                            .or(details.last_assistant_line.as_deref())
                            .unwrap_or(&details.progress.activity),
                    );
                }
            }),
        );
        let duplicate = {
            let mut activity = lock(&self.activity);
            match activity.entry(task_id.to_owned()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert((run_id.to_owned(), node_id.to_owned(), unsubscribe));
                    None
                }
                std::collections::btree_map::Entry::Occupied(_) => Some(unsubscribe),
            }
        };
        if let Some(unsubscribe) = duplicate {
            unsubscribe();
        }
    }
    /// TS `syncActivitySubscriptions`.
    fn reconcile_activity(
        self: &Arc<Self>,
        manager: &DagManager,
        session: Option<&str>,
        tasks: &senpi_task::manager::TaskManager,
        bridge: &Arc<crate::dag_rpc_bridge::DagRpcBridge>,
    ) -> Result<(), senpi_task::dag::manager::DagManagerError> {
        use senpi_task::dag::types::DagNodeState;
        let mut wanted = BTreeMap::new();
        if let Some(session) = session {
            for run in manager.list(session, None)? {
                if TERMINAL_RUN_STATUSES.contains(&run.status) {
                    continue;
                }
                for node in manager.record(&run.run_id, session)?.nodes {
                    if matches!(
                        node.state,
                        DagNodeState::Completed
                            | DagNodeState::Failed
                            | DagNodeState::Cancelled
                            | DagNodeState::Skipped
                    ) {
                        continue;
                    }
                    if let Some(task) = node.task_id {
                        wanted.insert(task, (run.run_id.clone(), node.id));
                    }
                }
            }
        }
        let stale = {
            let mut activity = lock(&self.activity);
            let stale: Vec<_> = activity
                .keys()
                .filter(|task| !wanted.contains_key(*task))
                .cloned()
                .collect();
            stale
                .into_iter()
                .filter_map(|task| activity.remove(&task))
                .collect::<Vec<_>>()
        };
        for (_, _, unsubscribe) in stale {
            unsubscribe();
        }
        for (task, (run, node)) in wanted {
            self.bind_activity(&run, &node, &task, tasks, bridge);
        }
        Ok(())
    }
}

impl crate::dag_status_ui::DagStatusUiManager for DagManager {
    fn list(&self, session: &str) -> Vec<senpi_task::dag::manager::DagRunSummary> {
        match self.list(session, None) {
            Ok(runs) => runs,
            Err(error) => {
                maho_ext_api::diagnostic(&format!("DAG status list failed: {error}"));
                vec![]
            }
        }
    }
    fn snapshot(&self, run: &str, session: &str) -> Option<senpi_task::dag::types::DagRunSnapshot> {
        self.snapshot(&run.to_owned(), session).ok()
    }
}

/// TS `createDagRetentionSweep` logger seam over the component logger.
struct RetentionLogger(Option<Arc<dyn ComponentLogger>>);
impl crate::dag_retention_sweep::DagRetentionSweepLogger for RetentionLogger {
    fn info(&self, message: &str) {
        match &self.0 {
            Some(logger) => logger.info(message, None),
            None => maho_ext_api::diagnostic(message),
        }
    }
    fn warn(&self, message: &str) {
        match &self.0 {
            Some(logger) => logger.warn(message, None),
            None => maho_ext_api::diagnostic(message),
        }
    }
}

/// TS `index.ts` `nodeSpawnPolicy`: the task tool's spawn policy applied to an agent-routed node.
pub fn dag_node_spawn_policy(
    policy: Arc<dyn senpi_task::tools::task::spawn_policy::SpawnPolicyDeps + Send + Sync>,
) -> DagNodeSpawnPolicy {
    Arc::new(move |_node_id, subagent_type, prompt, parent_session_id| {
        match senpi_task::tools::task::spawn_policy::evaluate_spawn_policy(
            policy.as_ref(),
            subagent_type.trim(),
            prompt,
            parent_session_id,
        ) {
            senpi_task::tools::task::spawn_policy::SpawnPolicyVerdict::Allow => {
                DagNodeSpawnPolicyVerdict::Allow
            }
            senpi_task::tools::task::spawn_policy::SpawnPolicyVerdict::Deny { message } => {
                DagNodeSpawnPolicyVerdict::Deny { message }
            }
            senpi_task::tools::task::spawn_policy::SpawnPolicyVerdict::Force { prompt } => {
                DagNodeSpawnPolicyVerdict::Force { prompt }
            }
        }
    })
}

/// TS `readSessionAncestry(pi, env)` (senpi-task runners/rpc-host/session-role.ts): a session
/// launched with a role, or a child process carrying the task env, reports its depth and root.
pub fn read_session_ancestry(
    session_context: &BTreeMap<String, String>,
    env: &BTreeMap<String, String>,
) -> Option<DagSchedulerAncestry> {
    fn depth_of(value: Option<&String>) -> u32 {
        value
            .and_then(|depth| depth.parse::<u32>().ok())
            .filter(|depth| *depth >= 1)
            .unwrap_or(1)
    }
    if session_context
        .get("role")
        .is_some_and(|role| !role.is_empty())
    {
        return Some(DagSchedulerAncestry {
            depth: depth_of(session_context.get("depth")),
        });
    }
    let member = env
        .get("SENPI_TASK_MEMBER")
        .is_some_and(|identity| !identity.is_empty());
    if !member && env.get("OMO_SENPI_TASK_RPC_CHILD").map(String::as_str) != Some("1") {
        return None;
    }
    Some(DagSchedulerAncestry {
        depth: depth_of(env.get("OMO_SENPI_TASK_DEPTH")),
    })
}

pub fn read_session_traceparent(
    session_context: &BTreeMap<String, String>,
    env: &BTreeMap<String, String>,
) -> Option<String> {
    senpi_task::runners::rpc_host::session_role::read_session_traceparent(
        &serde_json::json!({"sessionContext": session_context}),
        env,
    )
}

impl TaskDagEngine {
    pub fn register_queries(
        &self,
        api: &mut maho_ext_api::ExtensionApi,
        component: &crate::component::TaskComponent,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        crate::dag_commands::register_dag_commands(
            api,
            Arc::new(crate::dag_commands::DagCommandAdapter {
                manager: self.manager.clone(),
                tasks: (*component.engine.manager).clone(),
            }),
        );
        let active = self
            .this
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        crate::dag_rpc_handlers::register_dag_rpc_handlers(
            api,
            self.manager.clone(),
            Arc::new(move || {
                active
                    .upgrade()
                    .and_then(|dag| lock(&dag.active_session).clone())
            }),
        )
    }

    pub fn compose(
        engine: &TaskEngine,
        materialize: Option<senpi_task::dag::manager::DagMaterializeSkills>,
    ) -> Result<Arc<Self>, senpi_task::dag::store::DagStoreError> {
        Self::compose_with_options(engine, materialize, DagRuntimeSchedulingOptions::default())
    }

    /// TS `createDagRuntime(deps)`.
    pub fn compose_with_options(
        engine: &TaskEngine,
        materialize: Option<senpi_task::dag::manager::DagMaterializeSkills>,
        scheduling: DagRuntimeSchedulingOptions,
    ) -> Result<Arc<Self>, senpi_task::dag::store::DagStoreError> {
        let mut settings = senpi_task::dag::types::DagSettings::default();
        let config = &engine.config["task"]["dag"];
        macro_rules! resolved { ($($field:ident),*) => { $(if let Some(value) = config[stringify!($field)].as_u64() { settings.$field = value.try_into().map_err(|_| senpi_task::dag::store::DagStoreError::Message(format!("invalid dag setting {}", stringify!($field))))?; })* }; }
        resolved!(
            max_nodes_per_run,
            max_runs_per_session,
            subscriber_ring,
            heartbeat_ms,
            history_default_limit,
            history_max_limit,
            retention_days,
            max_prompt_bytes
        );
        let store = Arc::new(senpi_task::dag::store::create_dag_file_store(
            &senpi_task::dag::store::DagStoreConfig {
                project_dir: engine
                    .runtime
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .cwd()
                    .into(),
                task: Some(senpi_task::dag::store::DagStoreTaskConfig {
                    state_dir: Some(engine.store.state_dir().into()),
                    dag: Some(senpi_task::dag::store::DagSettingsOverrides {
                        max_nodes_per_run: Some(settings.max_nodes_per_run),
                        max_runs_per_session: Some(settings.max_runs_per_session),
                        subscriber_ring: Some(settings.subscriber_ring),
                        heartbeat_ms: Some(settings.heartbeat_ms),
                        history_default_limit: Some(settings.history_default_limit),
                        history_max_limit: Some(settings.history_max_limit),
                        retention_days: Some(settings.retention_days),
                        max_prompt_bytes: Some(settings.max_prompt_bytes),
                    }),
                }),
            },
            Default::default(),
        )?);
        crate::dag_retention_sweep::schedule_dag_retention_sweep(
            store.clone(),
            crate::dag_retention_sweep::DagRetentionSweepOptions {
                schedule: scheduling.retention_sweep.clone(),
                logger: Arc::new(RetentionLogger(scheduling.logger.clone())),
            },
        );
        let schedulers = Arc::new(Mutex::new(BTreeMap::<DagRunId, DagOwnedScheduler>::new()));
        let stopped_admissions = Arc::new(Mutex::new(BTreeSet::<DagRunId>::new()));
        let recovery_task_subscriptions = Arc::new(Mutex::new(BTreeMap::<
            String,
            senpi_task::manager::Unsubscribe,
        >::new()));
        let manager = create_dag_manager(DagManagerOptions {
            store: store.clone(),
            new_run_id: None,
            now: None,
            materialize_skills: materialize,
            settings: Some(settings),
        });
        let held = schedulers.clone();
        let stopped = stopped_admissions.clone();
        let stop_schedulers = schedulers.clone();
        let reattach_subscriptions = recovery_task_subscriptions.clone();
        let reattach_tasks = engine.manager.clone();
        let mutation_runtime: Arc<Mutex<Weak<TaskDagEngine>>> = Arc::new(Mutex::new(Weak::new()));
        let reattach_mutation = mutation_runtime.clone();
        let recovery = Arc::new(create_dag_recovery(DagRecoveryOptions {
            store: store.clone(),
            task_manager: engine.manager.clone(),
            host_pid: None,
            is_process_alive: scheduling
                .lease_watch
                .as_ref()
                .and_then(|watch| watch.is_process_alive.clone()),
            // A holder pid equal to our own says nothing about liveness; what fences the claim is
            // whether THIS runtime still schedules the run (TS isRunHeldInProcess).
            is_run_held_in_process: Some(Arc::new(move |run: &DagRunId| {
                lock(&held).contains_key(run)
            })),
            now: None,
            subscriber_ring: Some(settings.subscriber_ring),
            stop_admission: Some(Arc::new(move |run: &DagRunId| {
                lock(&stopped).insert(run.clone());
                let scheduler = lock(&stop_schedulers)
                    .get(run)
                    .map(|owned| owned.scheduler.clone());
                if let Some(scheduler) = scheduler {
                    scheduler.stop_admission();
                }
            })),
            reattach: Some(Arc::new(move |run: &DagRunId, task: &str| {
                let key = format!("{run}\0{task}");
                if lock(&reattach_subscriptions).contains_key(&key) {
                    return;
                }
                let mutation = reattach_mutation.clone();
                let unsubscribe = reattach_tasks.subscribe_child(
                    task,
                    Arc::new(move |_| {
                        let dag = lock(&mutation).upgrade();
                        if let Some(dag) = dag {
                            dag.mutation_listener();
                        }
                    }),
                );
                lock(&reattach_subscriptions).insert(key, unsubscribe);
            })),
            ancestry: scheduling.ancestry,
            paused_children: None,
            resume_credential: None,
            node_spawn_policy: scheduling.node_spawn_policy.clone(),
        }));
        let lease_watch =
            create_dag_lease_watch(scheduling.lease_watch.clone().unwrap_or_default());
        let dag = Arc::new(Self {
            manager,
            store,
            recovery,
            settings,
            task_manager: engine.manager.clone(),
            agents: Arc::new(engine.agents.clone()),
            config: engine.config.clone(),
            scheduling,
            runtime: engine.runtime.clone(),
            schedulers,
            stopped_admissions,
            recovery_task_subscriptions,
            lease_watch,
            lease_watches: Mutex::new(BTreeMap::new()),
            rpc: Mutex::new(None),
            surfaces: Mutex::new(None),
            active_session: Mutex::new(None),
            wake: Mutex::new(None),
            delivered: Mutex::new(BTreeMap::new()),
            published_log_len: Mutex::new(BTreeMap::new()),
            run_listeners: Mutex::new(BTreeMap::new()),
            terminal_wake_seq: Mutex::new(BTreeMap::new()),
            paused_wake_seq: Mutex::new(BTreeMap::new()),
            durable_listener_enabled: std::sync::atomic::AtomicBool::new(true),
            stop_observing: Mutex::new(None),
            this: Mutex::new(Weak::new()),
        });
        *lock(&dag.this) = Arc::downgrade(&dag);
        *lock(&mutation_runtime) = Arc::downgrade(&dag);
        // TS store wrapper: every durable checkpoint notifies the mutation listener and, for a run no
        // live scheduler owns, publishes the durable events it just wrote.
        let observer = Arc::downgrade(&dag);
        dag.store.set_checkpoint_listener(Some(Arc::new(move || {
            if let Some(dag) = observer.upgrade() {
                dag.on_checkpoint();
            }
        })));
        // TS observeDagSchedulers: every scheduler built over this task manager publishes its events.
        let observer = Arc::downgrade(&dag);
        let stop = observe_dag_schedulers(
            &dag.task_manager,
            Arc::new(move |scheduler: Arc<DagSchedulerContext>| {
                let observer = observer.clone();
                let _ = scheduler.subscribe(Arc::new(move |event: &DagRunEvent| {
                    if let Some(dag) = observer.upgrade() {
                        dag.publish_scheduler_event(event);
                    }
                }));
            }),
        );
        *lock(&dag.stop_observing) = Some(stop);
        dag.register_pause_source(engine);
        Ok(dag)
    }

    fn logger_warn(&self, message: &str, details: serde_json::Value) {
        match &self.scheduling.logger {
            Some(logger) => logger.warn(message, Some(&details)),
            None => maho_ext_api::diagnostic(&format!("{message} {details}")),
        }
    }
    fn logger_error(&self, message: &str, details: serde_json::Value) {
        match &self.scheduling.logger {
            Some(logger) => logger.error(message, Some(&details)),
            None => maho_ext_api::diagnostic(&format!("{message} {details}")),
        }
    }

    fn register_pause_source(self: &Arc<Self>, engine: &TaskEngine) {
        let records = Arc::downgrade(self);
        let options = Arc::downgrade(self);
        let retire = Arc::downgrade(self);
        let prepare = Arc::downgrade(self);
        let recovery = self.recovery.clone();
        let adopt = Arc::downgrade(self);
        let cancel = Arc::downgrade(self);
        let tasks = engine.run_pause.clone();
        engine.pause.register(Arc::new(
            crate::dag_run_pause_source::DagRunPauseSource::new(Arc::new(
                crate::dag_run_pause_source::DagRunPauseSourcePorts {
                    barrier: engine.manager.admission_barrier().clone(),
                    records: Arc::new(move |parent| {
                        let Some(dag) = records.upgrade() else {
                            return vec![];
                        };
                        match dag.manager.list(parent, None) {
                            Ok(runs) => runs
                                .into_iter()
                                .filter_map(|run| dag.manager.record(&run.run_id, parent).ok())
                                .collect(),
                            Err(error) => {
                                maho_ext_api::diagnostic(&format!(
                                    "DAG pause listing failed: {error}"
                                ));
                                vec![]
                            }
                        }
                    }),
                    options: Arc::new(move |record| {
                        options
                            .upgrade()
                            .expect("live DAG runtime")
                            .scheduler_options(record.clone())
                    }),
                    pauses: self.pause_store(),
                    retire: Arc::new(move |run| {
                        let Some(dag) = retire.upgrade() else {
                            return Ok(());
                        };
                        let owned = lock(&dag.schedulers).remove(run);
                        if let Some(mut owned) = owned {
                            owned.scheduler.suspend();
                            if let Some(running) = owned.running.take() {
                                running
                                    .join()
                                    .map_err(|_| "DAG scheduler panicked".to_string())?;
                            }
                        }
                        Ok(())
                    }),
                    prepare_resume: Arc::new(move |run| {
                        if let Some(dag) = prepare.upgrade() {
                            lock(&dag.stopped_admissions).remove(run);
                        }
                    }),
                    install_resumed_run: Arc::new(
                        move |run, parent, seq, credential| match recovery
                            .install_resumed_run_with_credential(
                                &run.to_string(),
                                parent,
                                seq,
                                Some(credential.clone()),
                            )? {
                            senpi_task::dag::recovery::InstallResumedRunResult::Installed(
                                installed,
                            ) => Ok(installed),
                            senpi_task::dag::recovery::InstallResumedRunResult::Skipped(
                                outcome,
                            ) => Err(format!("{outcome:?}")),
                        },
                    ),
                    adopt: Arc::new(move |installed| {
                        if let Some(dag) = adopt.upgrade() {
                            lock(&dag.schedulers).insert(
                                installed.run_id.clone(),
                                DagOwnedScheduler {
                                    scheduler: installed.scheduler.clone(),
                                    running: None,
                                },
                            );
                            dag.spawn_running(installed.run_id, installed.scheduler, true);
                        }
                    }),
                    cancel_run: Arc::new(move |run| {
                        cancel
                            .upgrade()
                            .ok_or_else(|| "DAG runtime disposed".to_string())?
                            .cancel(run, None)
                    }),
                    deliver_guidance: Some(Arc::new(move |task, input| {
                        tasks
                            .hold_task_input(
                                task,
                                &senpi_task::manager::run_pause::HeldTaskInput {
                                    input_id: input.input_id.clone(),
                                    revision: input.revision,
                                    text: input.text.clone(),
                                },
                            )
                            .map(|result| match result {
                                senpi_task::pause::ports::EngineHoldInputResult::Held {
                                    entry_id,
                                    ..
                                } => Some(entry_id),
                                _ => None,
                            })
                    })),
                },
            )),
        ));
    }

    fn session_id(&self) -> Option<String> {
        lock(&self.runtime).session_id().map(str::to_owned)
    }

    /// The paused-run sidecar every paused control verb reads and writes.
    fn pause_store(&self) -> Arc<senpi_task::dag::paused_controls::DagPauseStore> {
        Arc::new(senpi_task::dag::paused_controls::DagPauseStore::from_store(
            self.store.clone(),
        ))
    }

    /// The runtime's `cancelNode` capability: end one held node's child while the run stays paused.
    fn cancel_node_capability(&self) -> Option<crate::dag_tool::DagCancelNode> {
        let options = lock(&self.this).clone();
        let pauses = self.pause_store();
        let holds = crate::dag_paused_cancel::ended_child_closure(self.task_manager.clone());
        Some(Arc::new(move |run_id: &str, node_id: &str| {
            let Some(dag) = options.upgrade() else {
                return Err(crate::dag_tool::DagToolExecutionError::Control(
                    senpi_task::dag::scheduler::DagNodeControlError::new(
                        senpi_task::dag::scheduler::DagNodeControlErrorCode::RunNotFound,
                        format!("dag run \"{run_id}\" is no longer live in this session"),
                    ),
                ));
            };
            let context = dag.scheduler_options_for(run_id)?;
            senpi_task::dag::paused_controls::cancel_paused_dag_node_with_store(
                &context,
                &run_id.to_string(),
                &node_id.to_string(),
                holds.as_ref(),
                pauses.as_ref(),
            )
            .map_err(crate::dag_tool::DagToolExecutionError::Control)
        }))
    }

    /// The runtime's `guide` capability: validated add/edit/remove on a user-stopped run.
    fn guide_capability(&self) -> Option<crate::dag_tool::DagGuide> {
        let options = lock(&self.this).clone();
        let pauses = self.pause_store();
        Some(Arc::new(
            move |run_id: &str,
                  command: senpi_task::dag::paused_controls::PausedDagGuideCommand| {
                let Some(dag) = options.upgrade() else {
                    return Err(crate::dag_tool::DagToolExecutionError::Control(
                        senpi_task::dag::scheduler::DagNodeControlError::new(
                            senpi_task::dag::scheduler::DagNodeControlErrorCode::RunNotFound,
                            format!("dag run \"{run_id}\" is no longer live in this session"),
                        ),
                    ));
                };
                let context = dag.scheduler_options_for(run_id)?;
                senpi_task::dag::paused_controls::apply_paused_dag_guidance(
                    &context,
                    &run_id.to_string(),
                    command,
                    pauses.as_ref(),
                )
                .map_err(crate::dag_tool::DagToolExecutionError::Control)
            },
        ))
    }

    /// Why the run is paused right now, so the tool can refuse a model-driven `resume`.
    fn pause_reason_capability(&self) -> crate::dag_tool::PauseReasonCallback {
        let pauses = self.pause_store();
        Some(Arc::new(move |run_id: &str| {
            pauses
                .read(&run_id.to_string())
                .ok()
                .and_then(|state| state.pause)
                .map(|pause| match pause.reason {
                    senpi_task::dag::paused_controls::DagRunPauseReason::UserStop => {
                        "user_stop".to_string()
                    }
                    senpi_task::dag::paused_controls::DagRunPauseReason::SessionShutdown => {
                        "session_shutdown".to_string()
                    }
                })
        }))
    }

    /// The scheduler options of one existing run, rebuilt from its durable checkpoint. A run with no
    /// checkpoint is not this session's to control, so the caller refuses instead of inventing one.
    fn scheduler_options_for(
        &self,
        run_id: &str,
    ) -> Result<DagSchedulerOptions, senpi_task::dag::scheduler::DagNodeControlError> {
        let record = self
            .store
            .read_checkpoint::<DagRunRecordV1>(run_id)
            .ok()
            .flatten()
            .ok_or_else(|| {
                senpi_task::dag::scheduler::DagNodeControlError::new(
                    senpi_task::dag::scheduler::DagNodeControlErrorCode::RunNotFound,
                    format!("unknown dag run \"{run_id}\""),
                )
                .with_run_id(run_id.to_string())
            })?;
        Ok(self.scheduler_options(record))
    }

    /// TS `schedulerOptions` (shared by the first run, retry re-entry, amend re-entry).
    pub fn scheduler_options(&self, initial_record: DagRunRecordV1) -> DagSchedulerOptions {
        let configured = self.config["task"]["default_execution_mode"].as_str();
        DagSchedulerOptions {
            store: self.store.clone(),
            task_manager: self.task_manager.clone(),
            initial_record,
            execution_mode: Some(DagExecutionModeOptions {
                agents: self.agents.clone(),
                config: configured
                    .and_then(senpi_task::manager::execution_mode::ExecutionMode::parse),
                // Live read: a node dispatched before the session resolved `auto` names no mode.
                auto_mode: self
                    .scheduling
                    .auto_mode
                    .as_ref()
                    .and_then(|current| current()),
                config_is_auto: configured == Some("auto"),
            }),
            ancestry: self.scheduling.ancestry,
            node_spawn_policy: self.scheduling.node_spawn_policy.clone(),
            subscriber_ring: Some(self.settings.subscriber_ring),
            pre_attached_tasks: None,
            schedule_retry: None,
            resume_credential: None,
            now: None,
        }
    }

    /// TS `controller(runId, parentSessionId)`.
    pub fn scheduler(
        &self,
        run: &str,
        session: &str,
    ) -> Result<Arc<DagSchedulerContext>, senpi_task::dag::manager::DagManagerError> {
        if let Some(owned) = lock(&self.schedulers).get(run) {
            return Ok(owned.scheduler.clone());
        }
        let initial_record = self.manager.record(&run.to_owned(), session)?;
        let scheduler = create_dag_scheduler(self.scheduler_options(initial_record))
            .map_err(senpi_task::dag::manager::DagManagerError::from)?;
        let mut schedulers = lock(&self.schedulers);
        if let Some(owned) = schedulers.get(run) {
            return Ok(owned.scheduler.clone());
        }
        // TS admissionTaskManager: a run whose admission was stopped by recovery never admits.
        if lock(&self.stopped_admissions).contains(run) {
            scheduler.stop_admission();
        }
        schedulers.insert(
            run.to_owned(),
            DagOwnedScheduler {
                scheduler: scheduler.clone(),
                running: None,
            },
        );
        Ok(scheduler)
    }

    /// Spawns the scheduler's frontier loop in the background; on settle it waits for idle and
    /// leaves the map only if it is still the registered controller (TS ensureScheduled/adoptReentry).
    fn spawn_running(
        self: &Arc<Self>,
        run_id: DagRunId,
        scheduler: Arc<DagSchedulerContext>,
        already_running: bool,
    ) {
        let owner = Arc::downgrade(self);
        let registered = scheduler.clone();
        let registered_run_id = registered.snapshot().run_id;
        let handle = std::thread::Builder::new()
            .name("dag-scheduler".into())
            .spawn(move || {
                let outcome = if already_running {
                    Ok(())
                } else {
                    scheduler.run().map(|_| ())
                };
                scheduler.when_idle();
                if let Some(dag) = owner.upgrade() {
                    if let Err(error) = outcome {
                        dag.logger_error(
                            "omo-senpi dag scheduler failed",
                            serde_json::json!({"runId": run_id, "error": error.to_string()}),
                        );
                    }
                    let mut schedulers = lock(&dag.schedulers);
                    if schedulers
                        .get(&run_id)
                        .is_some_and(|owned| Arc::ptr_eq(&owned.scheduler, &scheduler))
                    {
                        schedulers.remove(&run_id);
                    }
                }
            });
        match handle {
            Ok(handle) => {
                let mut schedulers = lock(&self.schedulers);
                let owned =
                    schedulers
                        .entry(registered_run_id)
                        .or_insert_with(|| DagOwnedScheduler {
                            scheduler: registered.clone(),
                            running: None,
                        });
                if Arc::ptr_eq(&owned.scheduler, &registered) {
                    owned.running = Some(handle);
                }
            }
            Err(error) => self.logger_error(
                "omo-senpi dag scheduler failed",
                serde_json::json!({"error": error.to_string()}),
            ),
        }
    }

    /// TS `ensureScheduled`.
    pub fn ensure_scheduled(
        self: &Arc<Self>,
        run: &str,
        session: &str,
    ) -> Result<(), senpi_task::dag::manager::DagManagerError> {
        let record = self.manager.record(&run.to_owned(), session)?;
        if !SCHEDULABLE_RUN_STATUSES.contains(&record.status) {
            return Ok(());
        }
        let scheduler = self.scheduler(run, session)?;
        if lock(&self.schedulers)
            .get(run)
            .is_some_and(|owned| owned.running.is_some())
        {
            return Ok(());
        }
        self.spawn_running(run.to_owned(), scheduler, false);
        Ok(())
    }

    /// TS `adoptReentry`: the re-entry scheduler is already running on its own thread.
    fn adopt_reentry(
        self: &Arc<Self>,
        run: &str,
        reentry: senpi_task::dag::node_retry::DagRunReentry,
    ) {
        lock(&self.schedulers).insert(
            run.to_owned(),
            DagOwnedScheduler {
                scheduler: reentry.scheduler.clone(),
                running: None,
            },
        );
        self.spawn_running(run.to_owned(), reentry.scheduler, true);
    }

    fn clear_admission_latch(&self, run: &str) {
        lock(&self.stopped_admissions).remove(run);
    }

    /// TS `cancel`.
    pub fn cancel(self: &Arc<Self>, run: &str, reason: Option<&str>) -> Result<(), String> {
        let session = self.session_id().unwrap_or_default();
        let record = self
            .manager
            .record(&run.to_owned(), &session)
            .map_err(|error| error.to_string())?;
        if TERMINAL_RUN_STATUSES.contains(&record.status) {
            return Ok(());
        }
        self.scheduler(run, &session)
            .map_err(|error| error.to_string())?
            .cancel(&run.to_owned(), reason)
    }

    /// TS `retry`.
    pub fn retry(
        self: &Arc<Self>,
        run: &str,
        node_ids: Option<&[String]>,
        prompt: Option<&str>,
    ) -> Result<senpi_task::dag::types::DagRunSnapshot, crate::dag_tool::DagToolExecutionError>
    {
        let session = self.session_id().unwrap_or_default();
        self.manager.record(&run.to_owned(), &session)?;
        self.clear_admission_latch(run);
        let scheduler = self.scheduler(run, &session)?;
        let reentry = scheduler.retry_node(
            run.to_owned(),
            node_ids,
            prompt.map(|prompt| senpi_task::dag::node_retry::DagRetryOptions {
                prompt: Some(prompt.to_owned()),
            }),
        )?;
        self.adopt_reentry(run, reentry);
        Ok(self.manager.snapshot(&run.to_owned(), &session)?)
    }

    /// TS `send`.
    pub fn send(
        self: &Arc<Self>,
        run: &str,
        node: &str,
        message: &str,
    ) -> Result<crate::dag_tool::DagToolSendOutcome, crate::dag_tool::DagToolExecutionError> {
        let session = self.session_id().unwrap_or_default();
        self.manager.record(&run.to_owned(), &session)?;
        let scheduler = self.scheduler(run, &session)?;
        let result = scheduler
            .send_to_node(&run.to_owned(), &node.to_owned(), message)
            .join()
            .map_err(|_| {
                crate::dag_tool::DagToolExecutionError::Tool(maho_ext_api::ToolError::Message(
                    "DAG send worker panicked".into(),
                ))
            })??;
        Ok(crate::dag_tool::DagToolSendOutcome {
            node_id: result.node_id,
            task_id: result.task_id,
            guidance_id: result.guidance_id,
            effect_id: None,
            delivery: match result.delivery {
                senpi_task::dag::node_send::DagNodeDeliveryResult::Guidance => crate::dag_tool::DagToolSendDelivery::Guidance,
                senpi_task::dag::node_send::DagNodeDeliveryResult::Steer => {
                    crate::dag_tool::DagToolSendDelivery::Steer
                }
                senpi_task::dag::node_send::DagNodeDeliveryResult::Revive => {
                    crate::dag_tool::DagToolSendDelivery::Revive
                }
                senpi_task::dag::node_send::DagNodeDeliveryResult::Queued => {
                    crate::dag_tool::DagToolSendDelivery::Queued
                }
            },
            queue_position: result.queue_position,
        })
    }

    /// TS `amend`.
    pub fn amend(
        self: &Arc<Self>,
        run: &str,
        definition: senpi_task::dag::graph::DagDefinition,
    ) -> Result<senpi_task::dag::types::DagRunSnapshot, crate::dag_tool::DagToolExecutionError>
    {
        let session = self.session_id().unwrap_or_default();
        let amended = self
            .manager
            .amend(senpi_task::dag::manager::DagAmendParams {
                run_id: Some(run.to_owned()),
                key: None,
                definition,
                parent_session_id: session.clone(),
            })?;
        self.clear_admission_latch(run);
        lock(&self.schedulers).remove(run);
        let reentry = senpi_task::dag::node_retry::reenter_dag_run(self.scheduler_options(amended))
            .map_err(crate::dag_tool::DagToolExecutionError::Control)?;
        self.adopt_reentry(run, reentry);
        Ok(self.manager.snapshot(&run.to_owned(), &session)?)
    }

    /// TS `wait`: the wait surface, then the owned scheduler's `whenIdle`.
    pub fn wait(
        self: &Arc<Self>,
        run: &str,
        session: &str,
    ) -> Result<serde_json::Value, maho_ext_api::ToolError> {
        let owner = Arc::downgrade(self);
        let surface = senpi_task::dag::handle::create_dag_wait_surface(
            senpi_task::dag::handle::DagWaitSurfaceOptions {
                store: self.store.clone(),
                subscribe: Arc::new(
                    move |run_id: &DagRunId,
                          wake: senpi_task::dag::journal::DagJournalListener|
                          -> senpi_task::dag::journal::DagJournalUnsubscribe {
                        let Some(dag) = owner.upgrade() else {
                            return Box::new(|| {});
                        };
                        dag.subscribe(run_id, wake)
                    },
                ),
                cancel: None,
                read_output: None,
            },
        );
        let result = surface
            .wait(&run.to_owned(), session)
            .map_err(|error| maho_ext_api::ToolError::Message(error.to_string()))?;
        let owned = lock(&self.schedulers)
            .get(run)
            .map(|owned| owned.scheduler.clone());
        if let Some(scheduler) = owned {
            scheduler.when_idle();
        }
        Ok(serde_json::json!({
            "runId": result.run_id, "status": result.status, "snapshot": result.snapshot,
            "nodes": result.nodes.iter().map(|(id, node)| (id.clone(), terminal_node_json(node))).collect::<serde_json::Map<_, _>>(),
        }))
    }

    /// TS `subscribe(runId, listener)` over the runtime's durable listener registry.
    fn subscribe(
        self: &Arc<Self>,
        run: &str,
        listener: DurableDagListener,
    ) -> senpi_task::dag::journal::DagJournalUnsubscribe {
        lock(&self.run_listeners)
            .entry(run.to_owned())
            .or_default()
            .push(listener.clone());
        let owner = Arc::downgrade(self);
        let run = run.to_owned();
        Box::new(move || {
            let Some(dag) = owner.upgrade() else {
                return;
            };
            let mut listeners = lock(&dag.run_listeners);
            if let Some(current) = listeners.get_mut(&run) {
                current.retain(|existing| !Arc::ptr_eq(existing, &listener));
                if current.is_empty() {
                    listeners.remove(&run);
                }
            }
        })
    }

    fn durable_listener(self: &Arc<Self>) -> DurableDagListener {
        let owner = Arc::downgrade(self);
        Arc::new(move |event: &DagRunEvent| {
            if let Some(dag) = owner.upgrade()
                && dag
                    .durable_listener_enabled
                    .load(std::sync::atomic::Ordering::SeqCst)
            {
                dag.on_event(event);
            }
        })
    }

    /// TS `publishSchedulerEvent`.
    fn publish_scheduler_event(self: &Arc<Self>, event: &DagRunEvent) {
        // Only this run's listeners are consulted below; cloning the whole registry per event is waste.
        let listeners: BTreeMap<DagRunId, Vec<DurableDagListener>> = lock(&self.run_listeners)
            .get(&event.run_id)
            .map(|run_listeners| (event.run_id.clone(), run_listeners.clone()))
            .into_iter()
            .collect();
        let on_event = self.durable_listener();
        let mut delivered = lock(&self.delivered);
        // A lagging ring subscriber coalesces dropped events into a StreamOverflow it appends and
        // delivers first, with a seq past the events still queued behind it. Delivering that seq
        // alone would mark the earlier events (e.g. dag.run.started/completed) delivered and drop
        // them, so a gap first replays the durable log in seq order.
        if event.seq > delivered.get(&event.run_id).copied().unwrap_or(0) + 1
            && let Err(error) = crate::dag_runtime::publish_durable_events(
                &self.store,
                &event.run_id,
                &mut delivered,
                &listeners,
                &on_event,
            )
        {
            maho_ext_api::diagnostic(&format!("DAG durable event catch-up failed: {error}"));
        }
        crate::dag_runtime::publish_scheduler_event(event, &mut delivered, &listeners, &on_event);
    }

    /// TS store wrapper `writeCheckpoint` + `mutationListener`.
    fn on_checkpoint(self: &Arc<Self>) {
        self.mutation_listener();
        let runs: Vec<String> = match std::fs::read_dir(&self.store.paths.runs) {
            Ok(entries) => entries
                .flatten()
                .filter_map(|entry| {
                    entry
                        .path()
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        let mut publish_ctx = None;
        for run in runs {
            if lock(&self.schedulers).contains_key(&run) {
                continue;
            }
            // Measured BEFORE the read: an append racing the publish leaves a smaller recorded
            // length, so the next checkpoint re-reads instead of missing it.
            let log_len = std::fs::metadata(self.store.paths.event(&run))
                .ok()
                .map(|metadata| (metadata.len(), metadata.modified().ok()));
            if log_len.is_some() && lock(&self.published_log_len).get(&run).copied() == log_len {
                continue;
            }
            // PERF-DIVERGENCE(omo dag store wrapper): upstream rebuilds the listener list and durable listener for every run it visits; we build them lazily once per checkpoint, only when a run actually needs publishing. The listeners delivered to are the same.
            let (listeners, on_event) = publish_ctx.get_or_insert_with(|| {
                (lock(&self.run_listeners).clone(), self.durable_listener())
            });
            let mut delivered: BTreeMap<DagRunId, u64> = lock(&self.delivered)
                .get(&run)
                .map(|seq| (run.clone(), *seq))
                .into_iter()
                .collect();
            match crate::dag_runtime::publish_durable_events(
                &self.store,
                &run,
                &mut delivered,
                listeners,
                on_event,
            ) {
                Ok(()) => {
                    if let Some(len) = log_len {
                        lock(&self.published_log_len).insert(run.clone(), len);
                    }
                }
                Err(error) => {
                    maho_ext_api::diagnostic(&format!("DAG durable event publish failed: {error}"))
                }
            }
            if let Some(seq) = delivered.get(&run).copied() {
                raise_delivered(&self.delivered, &run, seq);
            }
        }
    }

    /// TS `mutationListener`.
    fn mutation_listener(self: &Arc<Self>) {
        let bridge = lock(&self.rpc).clone();
        if let Some(bridge) = &bridge {
            bridge.sync();
            bridge.notify_store_mutation();
        }
        let surfaces = lock(&self.surfaces).clone();
        if let (Some(surfaces), Some(bridge)) = (&surfaces, &bridge) {
            let session = lock(&self.active_session).clone();
            if let Err(error) = surfaces.reconcile_activity(
                &self.manager,
                session.as_deref(),
                &self.task_manager,
                bridge,
            ) {
                maho_ext_api::diagnostic(&format!("DAG activity reconciliation failed: {error}"));
            }
        }
        if let Some(surfaces) = surfaces {
            surfaces.status.schedule_sync();
        }
    }

    /// TS `onEvent` + `notifyPaused`.
    fn on_event(self: &Arc<Self>, event: &DagRunEvent) {
        let surfaces = lock(&self.surfaces).clone();
        let event_type = event.payload.event_type();
        if event_type == DagRunEventType::RunStarted
            && let Some(surfaces) = &surfaces
        {
            surfaces.wake.on_run_start(&event.run_id);
        }
        if event_type == DagRunEventType::RunPaused {
            self.notify_wake(event, &self.paused_wake_seq);
            return;
        }
        if !matches!(
            event_type,
            DagRunEventType::RunCompleted
                | DagRunEventType::RunFailed
                | DagRunEventType::RunCancelled
        ) {
            return;
        }
        if let Some(surfaces) = &surfaces {
            surfaces.wake.on_run_terminal(&event.run_id);
        }
        self.notify_wake(event, &self.terminal_wake_seq);
    }

    fn notify_wake(&self, event: &DagRunEvent, sequences: &Mutex<BTreeMap<DagRunId, u64>>) {
        {
            let mut sequences = lock(sequences);
            if *sequences.get(&event.run_id).unwrap_or(&0) >= event.seq {
                return;
            }
            sequences.insert(event.run_id.clone(), event.seq);
        }
        let Ok(Some(record)) = self.store.read_checkpoint::<DagRunRecordV1>(&event.run_id) else {
            return;
        };
        if lock(&self.active_session).as_deref() != Some(record.parent_session_id.as_str()) {
            return;
        }
        let mut wake = lock(&self.wake);
        let Some(wake) = wake.as_mut() else {
            return;
        };
        wake.on_dag_event(
            crate::dag_wake::DagWakeRun {
                run_id: &record.run_id,
                name: &record.name,
                parent_session_id: &record.parent_session_id,
            },
            &event.payload,
        );
    }

    /// TS `recoverPausedRuns(scope)`.
    fn recover_paused_runs(self: &Arc<Self>, scope: &RecoveryScope) {
        let outcomes = self
            .recovery
            .resume_paused_runs_from(&scope.session_id, scope.fork_source_session_id.as_deref());
        let subscriptions = std::mem::take(&mut *lock(&self.recovery_task_subscriptions));
        for unsubscribe in subscriptions.into_values() {
            unsubscribe();
        }
        for outcome in outcomes {
            if let DagRecoveryOutcome::Skipped {
                run_id,
                reason: DagRecoverySkipReason::LiveLease,
                holder_pid: Some(holder_pid),
            } = outcome
            {
                self.watch_live_lease(scope, run_id, holder_pid);
            }
        }
        match self.manager.list(&scope.session_id, None) {
            Ok(runs) => {
                for run in runs {
                    if let Err(error) = self.ensure_scheduled(&run.run_id, &scope.session_id) {
                        self.logger_error(
                            "omo-senpi dag scheduler failed",
                            serde_json::json!({"runId": run.run_id, "error": error.to_string()}),
                        );
                    }
                }
            }
            Err(error) => self.logger_error(
                "omo-senpi DAG deferred recovery failed",
                serde_json::json!({"error": error.to_string()}),
            ),
        }
    }

    /// TS `watchLiveLease`.
    fn watch_live_lease(
        self: &Arc<Self>,
        scope: &RecoveryScope,
        run_id: DagRunId,
        holder_pid: i64,
    ) {
        if lock(&self.lease_watches).contains_key(&run_id) {
            return;
        }
        // A live_lease naming our own pid means a scheduler in this runtime still holds the run.
        if holder_pid == i64::from(std::process::id()) {
            return;
        }
        self.logger_warn("omo-senpi DAG run stays paused while its previous host exits; resuming once that pid is gone", serde_json::json!({"runId": run_id, "holderPid": holder_pid, "sessionId": scope.session_id}));
        let owner = Arc::downgrade(self);
        let watched_run = run_id.clone();
        let scope = scope.clone();
        let cancel = self.lease_watch.watch(
            holder_pid,
            Arc::new(move || {
                let Some(dag) = owner.upgrade() else {
                    return;
                };
                lock(&dag.lease_watches).remove(&watched_run);
                if lock(&dag.active_session).as_deref() != Some(scope.session_id.as_str()) {
                    return;
                }
                let dag_for_thread = dag.clone();
                let scope = scope.clone();
                let _ = std::thread::Builder::new()
                    .name("dag-deferred-recovery".into())
                    .spawn(move || {
                        dag_for_thread.recover_paused_runs(&scope);
                        dag_for_thread.mutation_listener();
                    });
            }),
        );
        lock(&self.lease_watches).insert(run_id, cancel);
    }

    fn cancel_lease_watches(&self) {
        let watches = std::mem::take(&mut *lock(&self.lease_watches));
        for cancel in watches.into_values() {
            cancel.cancel();
        }
        self.lease_watch.dispose();
    }

    /// TS `runtime.attach(event)`.
    pub fn attach(
        self: &Arc<Self>,
        event: Option<&maho_ext_api::SessionStartEvent>,
        coordinator: Option<Arc<dyn IdleInjectionCoordinator>>,
    ) {
        let session = self.session_id();
        *lock(&self.active_session) = session.clone();
        self.durable_listener_enabled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(coordinator) = coordinator {
            let parent_runtime = self.runtime.clone();
            let mut wake = lock(&self.wake);
            if wake.is_none() {
                *wake = Some(crate::dag_wake::create_dag_wake(
                    coordinator,
                    Arc::new(move || lock(&parent_runtime).parent_state()),
                ));
            }
        }
        if let Some(wake) = lock(&self.wake).as_mut() {
            wake.on_session_start(session.as_deref());
        }
        if let Some(session_id) = session.clone() {
            let session_file = lock(&self.runtime)
                .session_file()
                .map(|path| path.to_string_lossy().into_owned());
            let event_json = event.map_or(serde_json::Value::Null, |event| serde_json::json!({
                "reason": match event.reason { maho_ext_api::SessionReason::Startup => "startup", maho_ext_api::SessionReason::Reload => "reload", maho_ext_api::SessionReason::New => "new", maho_ext_api::SessionReason::Resume => "resume", maho_ext_api::SessionReason::Fork => "fork", maho_ext_api::SessionReason::Quit => "quit" },
                "previousSessionFile": event.previous_session_file,
            }));
            let fork_source = crate::dag_fork_source::resolve_dag_fork_source(
                crate::dag_fork_source::ResolveDagForkSourceInput {
                    event: event_json,
                    current_session_id: session_id.clone(),
                    current_session_file: session_file,
                },
            );
            if let Some(diagnostic) = fork_source.diagnostic() {
                self.logger_warn(
                    "omo-senpi DAG fork source rejected; recovering own runs only",
                    serde_json::json!({"sessionId": session_id, "reason": diagnostic}),
                );
            }
            self.recover_paused_runs(&RecoveryScope {
                session_id,
                fork_source_session_id: fork_source.session_id().map(str::to_owned),
            });
        }
        // #7316: the bridge attaches AFTER recovery so its first snapshot reflects recovered runs.
        let bridge = lock(&self.rpc).clone();
        if let Some(bridge) = &bridge {
            bridge.attach();
        }
        let surfaces = lock(&self.surfaces).clone();
        if let (Some(surfaces), Some(bridge)) = (&surfaces, &bridge)
            && let Err(error) = surfaces.reconcile_activity(
                &self.manager,
                session.as_deref(),
                &self.task_manager,
                bridge,
            )
        {
            maho_ext_api::diagnostic(&format!("DAG activity reconciliation failed: {error}"));
        }
        if let Some(surfaces) = surfaces {
            surfaces.status.sync_now();
        }
    }

    /// TS `runtime.detach()`.
    pub fn detach(&self) {
        self.durable_listener_enabled
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let detached_listeners = lock(&self.run_listeners).clone();
        let stopping: Vec<_> = lock(&self.schedulers)
            .iter()
            .map(|(run, owned)| (run.clone(), owned.scheduler.clone()))
            .collect();
        for (run, scheduler) in stopping {
            if let Err(error) = scheduler.cancel(&run, Some("runtime detached")) {
                maho_ext_api::diagnostic(&format!("DAG detach cancel failed: {error}"));
            }
        }
        {
            let mut listeners = lock(&self.run_listeners);
            for (run, stale) in detached_listeners {
                if let Some(current) = listeners.get_mut(&run) {
                    current.retain(|listener| !stale.iter().any(|old| Arc::ptr_eq(old, listener)));
                    if current.is_empty() {
                        listeners.remove(&run);
                    }
                }
            }
        }
        self.cancel_lease_watches();
        let subscriptions = std::mem::take(&mut *lock(&self.recovery_task_subscriptions));
        for unsubscribe in subscriptions.into_values() {
            unsubscribe();
        }
        let surfaces = lock(&self.surfaces).clone();
        if let Some(surfaces) = &surfaces {
            surfaces.clear_activity();
        }
        if let Some(bridge) = lock(&self.rpc).clone() {
            bridge.detach();
        }
        *lock(&self.active_session) = None;
        if let Some(surfaces) = surfaces {
            surfaces.status.dispose();
        }
    }

    /// The vetoable `session_before_switch` never detaches or suspends DAG work (TS
    /// `wireDagLifecycle`): only the old session's view is released - bridge subscriptions, live
    /// activity and the status widget - so the next `session_start` re-attaches the same live runs.
    pub fn detach_view(&self) {
        let surfaces = lock(&self.surfaces).clone();
        if let Some(surfaces) = &surfaces {
            surfaces.clear_activity();
        }
        if let Some(bridge) = lock(&self.rpc).clone() {
            bridge.detach();
        }
        if let Some(surfaces) = surfaces {
            surfaces.status.dispose();
        }
    }

    /// TS `runtime.pauseForShutdown()`: retire live schedulers WITHOUT cancelling them, drain their
    /// loops, then persist the pause.
    pub fn pause_for_shutdown(&self, session: &str) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.cancel_lease_watches();
        let retiring: Vec<_> = lock(&self.schedulers)
            .iter()
            .map(|(id, owned)| (id.clone(), owned.scheduler.clone()))
            .collect();
        for (_, scheduler) in &retiring {
            scheduler.suspend();
        }
        let handles: Vec<_> = {
            let mut schedulers = lock(&self.schedulers);
            retiring
                .iter()
                .filter_map(|(id, scheduler)| {
                    schedulers
                        .get_mut(id)
                        .filter(|owned| Arc::ptr_eq(&owned.scheduler, scheduler))
                        .and_then(|owned| owned.running.take())
                })
                .collect()
        };
        for handle in handles {
            let _ = handle.join();
        }
        {
            let mut schedulers = lock(&self.schedulers);
            for (id, scheduler) in retiring {
                if schedulers
                    .get(&id)
                    .is_some_and(|owned| Arc::ptr_eq(&owned.scheduler, &scheduler))
                {
                    schedulers.remove(&id);
                }
            }
        }
        let session = lock(&self.active_session)
            .clone()
            .unwrap_or_else(|| session.to_owned());
        self.recovery
            .try_pause_runs_for_shutdown(&session)
            .map_err(failure)?;
        Ok(())
    }

    /// TS `runtime.dispose()`.
    pub fn dispose(&self) {
        if let Some(stop) = lock(&self.stop_observing).take() {
            stop();
        }
        self.cancel_lease_watches();
        if let Some(bridge) = lock(&self.rpc).clone() {
            bridge.dispose();
        }
        *lock(&self.active_session) = None;
        let surfaces = lock(&self.surfaces).clone();
        if let Some(surfaces) = &surfaces {
            surfaces.status.dispose();
            surfaces.wake.emit_shutdown();
            surfaces.clear_activity();
        }
        let subscriptions = std::mem::take(&mut *lock(&self.recovery_task_subscriptions));
        for unsubscribe in subscriptions.into_values() {
            unsubscribe();
        }
        lock(&self.run_listeners).clear();
        self.store.set_checkpoint_listener(None);
    }

    pub fn register_rpc(
        self: &Arc<Self>,
        api: &mut maho_ext_api::ExtensionApi,
        component: &crate::component::TaskComponent,
    ) {
        self.register_rpc_with_status_timers(
            api,
            component,
            Arc::new(crate::timers::HostTimers::default()),
        );
    }
    pub fn register_rpc_with_status_timers(
        self: &Arc<Self>,
        api: &mut maho_ext_api::ExtensionApi,
        component: &crate::component::TaskComponent,
        timers: Arc<dyn crate::status_ui::StatusUiTimers>,
    ) {
        self.register_rpc_with_timers(
            api,
            component,
            timers,
            Arc::new(crate::timers::HostTimers::default()),
        );
    }
    /// Registers the RPC bridge, status widget, wake source, reload guard, and the DAG lifecycle
    /// (TS createDagRuntime surfaces + wireDagLifecycle).
    pub fn register_rpc_with_timers(
        self: &Arc<Self>,
        api: &mut maho_ext_api::ExtensionApi,
        component: &crate::component::TaskComponent,
        timers: Arc<dyn crate::status_ui::StatusUiTimers>,
        rpc_timers: Arc<dyn crate::status_ui::StatusUiTimers>,
    ) {
        let pause = Arc::downgrade(self);
        component.set_before_suspend(Arc::new(move |session| match pause.upgrade() {
            Some(dag) => dag.pause_for_shutdown(session),
            None => Ok(()),
        }));
        let reload_runtime = self.runtime.clone();
        let reload_source = crate::dag_reload_source::create_dag_reload_source(
            crate::dag_reload_source::DagReloadSourceDeps {
                manager: Box::new(self.manager.clone()),
                session_id: Box::new(move || lock(&reload_runtime).session_id().map(str::to_owned)),
            },
        );
        struct ReloadAdapter(crate::dag_reload_source::DagReloadSource);
        impl crate::reload_guard::ReloadGuardDagSource for ReloadAdapter {
            fn live_runs(&self) -> Vec<crate::reload_guard::ReloadGuardDagRun> {
                self.0.live_runs()
            }
        }
        crate::reload_guard::wire_dag_reload_guard(api, Arc::new(ReloadAdapter(reload_source)));
        let logger = self.scheduling.logger.clone();
        let status = crate::dag_status_ui::DagStatusUi::with_options(
            Arc::new(self.manager.clone()),
            self.runtime.clone(),
            timers,
            None,
            None,
            logger.map(|logger| {
                Arc::new(move |message: &str, error: &str| {
                    logger.warn(message, Some(&serde_json::json!({"error": error})))
                }) as Arc<dyn Fn(&str, &str) + Send + Sync>
            }),
        );
        let runtime = self.runtime.clone();
        let session: Arc<dyn Fn() -> Option<String> + Send + Sync> =
            Arc::new(move || lock(&runtime).session_id().map(str::to_owned));
        let surfaces = Arc::new(DagSurfaces {
            status,
            wake: crate::dag_wake_source::create_dag_wake_source(
                crate::dag_wake_source::DagWakeSourceDeps {
                    events: api.events.clone(),
                    manager: Arc::new(self.manager.clone()),
                    session_id: session.clone(),
                },
            ),
            activity: Mutex::new(BTreeMap::new()),
        });
        *lock(&self.surfaces) = Some(surfaces);
        let live = Arc::downgrade(self);
        let live_session = session.clone();
        let snapshots = Arc::downgrade(self);
        let snapshot_session = session.clone();
        let events = api.events.clone();
        let bridge = crate::dag_rpc_bridge::create_dag_rpc_bridge(
            crate::dag_rpc_bridge_contract::DagRpcBridgeDeps {
                live_runs: Arc::new(move || {
                    let (Some(dag), Some(session)) = (live.upgrade(), live_session()) else {
                        return vec![];
                    };
                    let runs = match dag.manager.list(&session, None) {
                        Ok(runs) => runs,
                        Err(error) => {
                            maho_ext_api::diagnostic(&format!("DAG live run list failed: {error}"));
                            return vec![];
                        }
                    };
                    runs.into_iter()
                        .map(|run| {
                            let owner = Arc::downgrade(&dag);
                            let id = run.run_id.clone();
                            crate::dag_rpc_bridge_contract::DagBridgeRun {
                                run_id: run.run_id,
                                status: run.status.as_str().into(),
                                subscribe: Arc::new(move |listener| {
                                    let Some(dag) = owner.upgrade() else {
                                        return Box::new(|| {}) as senpi_task::manager::Unsubscribe;
                                    };
                                    dag.subscribe(
                                        &id,
                                        Arc::new(move |event: &DagRunEvent| {
                                            match serde_json::to_value(event) {
                                                Ok(event) => listener(&event),
                                                Err(error) => maho_ext_api::diagnostic(&format!(
                                                    "DAG event serialization failed: {error}"
                                                )),
                                            }
                                        }),
                                    )
                                }),
                            }
                        })
                        .collect()
                }),
                run_snapshots: Some(Arc::new(move || {
                    let (Some(dag), Some(session)) = (snapshots.upgrade(), snapshot_session())
                    else {
                        return vec![];
                    };
                    match dag.manager.list(&session, None) {
                        Ok(runs) => runs
                            .into_iter()
                            .filter_map(|run| {
                                dag.manager
                                    .snapshot(&run.run_id, &session)
                                    .ok()
                                    .map(|snapshot| (snapshot, run.updated_at))
                            })
                            .collect(),
                        Err(error) => {
                            maho_ext_api::diagnostic(&format!("DAG snapshot list failed: {error}"));
                            vec![]
                        }
                    }
                })),
                parent_session_id: session,
                emit: Arc::new(move |name, data| {
                    events.emit(
                        "senpi:extension-rpc-event",
                        &serde_json::json!({"name":name,"data":data}),
                    )
                }),
                timers: rpc_timers,
                now: Arc::new(|| chrono::Utc::now().timestamp_millis()),
                heartbeat_ms: Some(self.settings.heartbeat_ms),
                activity_coalesce_ms: None,
                snapshot_debounce_ms: None,
            },
        );
        *lock(&self.rpc) = Some(bridge);
        // TS wireDagLifecycle: shutdown pauses then detaches BEFORE the task lifecycle runs (the
        // component already registered its handlers and calls pause via before_suspend), session
        // start attaches, and a final shutdown handler disposes.
        let detach = Arc::downgrade(self);
        api.on(
            maho_ext_api::EventKind::SessionBeforeSwitch,
            Arc::new(move |_, _| {
                let detach = detach.clone();
                Box::pin(async move {
                    if let Some(dag) = detach.upgrade() {
                        dag.detach_view();
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }),
        );
        let attach = Arc::downgrade(self);
        api.on(
            maho_ext_api::EventKind::SessionStart,
            Arc::new(move |event, context| {
                let attach = attach.clone();
                let start = match event {
                    maho_ext_api::ExtensionEvent::SessionStart(start) => Some(start.clone()),
                    _ => None,
                };
                let coordinator = idle_injection::from_slot(&context.idle_coordinator);
                Box::pin(async move {
                    if let Some(dag) = attach.upgrade() {
                        tokio::task::spawn_blocking(move || {
                            dag.attach(start.as_ref(), coordinator)
                        })
                        .await
                        .map_err(failure)?;
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }),
        );
        let dispose = Arc::downgrade(self);
        api.on(
            maho_ext_api::EventKind::SessionShutdown,
            Arc::new(move |_, _| {
                let dispose = dispose.clone();
                Box::pin(async move {
                    if let Some(dag) = dispose.upgrade() {
                        dag.detach();
                        dag.dispose();
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }),
        );
    }

    /// TS `registerDagTool`.
    pub fn register_tool(
        self: &Arc<Self>,
        api: &mut maho_ext_api::ExtensionApi,
        component: Arc<crate::component::TaskComponent>,
    ) {
        let runtime = component.engine.runtime.clone();
        let parent_session_id: Arc<dyn Fn() -> String + Send + Sync> =
            Arc::new(move || lock(&runtime).session_id().unwrap_or_default().to_owned());
        let wait_dag = self.clone();
        let cancel_dag = self.clone();
        let retry_dag = self.clone();
        let send_dag = self.clone();
        let amend_dag = self.clone();
        let cancel_node_dag = self.cancel_node_capability();
        let guide_dag = self.guide_capability();
        let pause_reason = self.pause_reason_capability();
        let mut tool = crate::dag_tool::create_dag_tool(crate::dag_tool::DagToolDeps {
            manager: self.manager.clone(),
            parent_session_id: parent_session_id.clone(),
            root_session_id: parent_session_id,
            wait: Some(Arc::new(move |run: &str, session: &str| {
                wait_dag.wait(run, session)
            })),
            cancel: Some(Arc::new(move |run: &str, reason: Option<&str>| {
                cancel_dag
                    .cancel(run, reason)
                    .map_err(maho_ext_api::ToolError::Message)
            })),
            retry: Some(Arc::new(move |run, nodes, prompt| {
                retry_dag.retry(run, nodes, prompt)
            })),
            send: Some(Arc::new(move |run, node, message| {
                send_dag.send(run, node, message)
            })),
            amend: Some(Arc::new(move |run, definition| {
                amend_dag.amend(run, definition)
            })),
            // The paused-run verbs the runtime supplies: cancellation of one held node's child, the
            // guidance custody of a user-stopped run, and why the run is paused right now.
            cancel_node: cancel_node_dag,
            guide: guide_dag,
            pause_reason,
        });
        // TS manager.start: the core start, then ensureScheduled in the background.
        let execute = tool.execute.clone();
        let dag = self.clone();
        tool.execute = Arc::new(move |call| {
            let start = call.params["action"] == "start";
            let future = execute(call);
            let dag = dag.clone();
            Box::pin(async move {
                let result = future.await?;
                if start
                    && let Some(run) = result
                        .details
                        .as_ref()
                        .and_then(|details| details["run_id"].as_str())
                    && let Some(session) = dag.session_id()
                    && let Err(error) = dag.ensure_scheduled(run, &session)
                {
                    dag.logger_error(
                        "omo-senpi dag scheduler failed",
                        serde_json::json!({"runId": run, "error": error.to_string()}),
                    );
                }
                Ok(result)
            })
        });
        api.register_tool(tool);
    }
}

fn terminal_node_json(node: &senpi_task::dag::handle::DagTerminalNodeResult) -> serde_json::Value {
    use senpi_task::dag::handle::DagTerminalNodeResult;
    match node {
        DagTerminalNodeResult::Completed {
            task_id,
            output,
            run_stats,
        } => {
            let mut value =
                serde_json::json!({"state":"completed","taskId":task_id,"output":output});
            if let Some(stats) = run_stats {
                value["runStats"] = serde_json::json!(stats);
            }
            value
        }
        DagTerminalNodeResult::Failed { task_id, error } => {
            let mut value = serde_json::json!({"state":"failed","error":error});
            if let Some(task) = task_id {
                value["taskId"] = serde_json::json!(task);
            }
            value
        }
        DagTerminalNodeResult::Cancelled { task_id, reason } => {
            let mut value = serde_json::json!({"state":"cancelled","reason":reason});
            if let Some(task) = task_id {
                value["taskId"] = serde_json::json!(task);
            }
            value
        }
        DagTerminalNodeResult::Skipped { dependency_ids } => {
            serde_json::json!({"state":"skipped","dependencyIds":dependency_ids})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_write_back_never_lowers_a_delivered_seq() {
        let delivered = Mutex::new(BTreeMap::from([
            ("live".to_owned(), 9),
            ("idle".to_owned(), 4),
        ]));
        // A checkpoint publishes "idle" 4 -> 6 while the live run's subscriber advances it 9 -> 12.
        lock(&delivered).insert("live".to_owned(), 12);
        raise_delivered(&delivered, &"idle".to_owned(), 6);
        assert_eq!(
            *lock(&delivered),
            BTreeMap::from([("live".to_owned(), 12), ("idle".to_owned(), 6)])
        );
        // A stale write-back for a run another publisher advanced keeps the larger seq.
        raise_delivered(&delivered, &"live".to_owned(), 9);
        raise_delivered(&delivered, &"new".to_owned(), 2);
        assert_eq!(
            *lock(&delivered),
            BTreeMap::from([
                ("live".to_owned(), 12),
                ("idle".to_owned(), 6),
                ("new".to_owned(), 2)
            ])
        );
    }
}
