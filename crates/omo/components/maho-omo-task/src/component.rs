use crate::{
    engine::TaskEngine,
    resumption_channel_emitter::{OwnedResumptionChannels, TaskResumptionChannelManager},
    session_transition_bridge::SessionTransitionBridge,
    status_ui::TaskStatusUi,
    timers::HostTimers,
    tools::TaskToolsDeps,
};
use maho_ext_api::{EventKind, EventResult, ExtensionApi, ExtensionEvent, FlagValue};
use senpi_task::{
    completion::{CompletionRequest, ReconcileUnnotifiedNotificationsInput, TransitionReason},
    manager::{AbortSignal, types::ListScope},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, PoisonError},
    thread::JoinHandle,
};

pub use crate::workpool_tool::register_workpool_tool;

#[derive(Clone)]
pub struct TaskTerminalEdge {
    pub record: senpi_task::state::TaskRecord,
    pub previous_status: Option<senpi_task::state::TaskStatus>,
    pub state_dir: std::path::PathBuf,
}
pub type TaskTerminalObserver = Arc<dyn Fn(&TaskTerminalEdge) + Send + Sync>;
#[derive(Default)]
struct TerminalObserverState {
    next_id: u64,
    observers: BTreeMap<u64, TaskTerminalObserver>,
}
#[derive(Clone, Default)]
pub struct TaskTerminalObservers(Arc<Mutex<TerminalObserverState>>);
pub struct TaskTerminalSubscription {
    observers: TaskTerminalObservers,
    id: u64,
}
impl Drop for TaskTerminalSubscription {
    fn drop(&mut self) {
        self.observers
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observers
            .remove(&self.id);
    }
}
impl TaskTerminalObservers {
    pub fn subscribe(&self, observer: TaskTerminalObserver) -> TaskTerminalSubscription {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let id = state.next_id;
        state.next_id += 1;
        state.observers.insert(id, observer);
        TaskTerminalSubscription {
            observers: self.clone(),
            id,
        }
    }
    pub fn notify(&self, edge: &TaskTerminalEdge) {
        let observers: Vec<_> = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observers
            .values()
            .cloned()
            .collect();
        for observer in observers {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(edge))).is_err() {
                maho_ext_api::diagnostic("task terminal observer panicked after commit");
            }
        }
    }
}
pub fn shared_task_terminal_observers() -> &'static TaskTerminalObservers {
    static OBSERVERS: std::sync::OnceLock<TaskTerminalObservers> = std::sync::OnceLock::new();
    OBSERVERS.get_or_init(TaskTerminalObservers::default)
}

struct CompletionWaiter {
    signal: AbortSignal,
    thread: JoinHandle<()>,
}

fn retire_completion_waiters(waiters: BTreeMap<String, CompletionWaiter>) {
    for waiter in waiters.values() {
        waiter.signal.abort();
    }
    if waiters.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for waiter in waiters.into_values() {
            if waiter.thread.join().is_err() {
                maho_ext_api::diagnostic("task completion waiter panicked");
            }
        }
    });
}
struct TeamRuntime {
    service: Arc<crate::team_service::TeamService>,
    pollers: Arc<crate::lead_poller_lifecycle::LeadPollerLifecycle>,
    liveness: Arc<crate::member_liveness::TeamMemberLivenessNotifier>,
}
/// The bind-time suspend callback the team runtime installs (`set_before_suspend`).
type BeforeSuspendCallback =
    Arc<dyn Fn(&str) -> Result<(), maho_ext_api::ExtensionFailure> + Send + Sync>;
pub struct TaskComponent {
    pub engine: TaskEngine,
    pub status: Arc<TaskStatusUi>,
    channels: OwnedResumptionChannels,
    transitions: Mutex<SessionTransitionBridge>,
    waiters: Mutex<BTreeMap<String, CompletionWaiter>>,
    disposed: std::sync::atomic::AtomicBool,
    rpc: Mutex<Option<Arc<crate::task_rpc_bridge::TaskRpcBridge>>>,
    terminal: Mutex<Option<crate::completion_bridge::TerminalObserver>>,
    delivery: Mutex<()>,
    terminal_epochs: Mutex<std::collections::BTreeSet<(String, i64)>>,
    team: Mutex<Option<Arc<TeamRuntime>>>,
    team_routing:
        Arc<Mutex<Option<senpi_task::tools::control::send_shutdown::TaskSendTeamRouting>>>,
    mutation_sync: Mutex<Option<u64>>,
    eval_handles: Mutex<Option<Arc<crate::eval_handle_host::TaskMutationObserver>>>,
    before_suspend: Mutex<Option<BeforeSuspendCallback>>,
    dag: Mutex<Option<Arc<crate::dag_engine::TaskDagEngine>>>,
    /// `event-bridge.ts` pause custody: restored before the reconcile, applied on the host's Resume.
    pause_custody: Mutex<Option<crate::event_bridge::TaskPauseCustody>>,
    pause_subscription: Mutex<Option<maho_ext_api::BusSubscription>>,
}
impl TaskComponent {
    /// `event-bridge.ts` pause custody: restored on session start, applied on the host's Resume edge.
    pub fn pause_custody(&self) -> Option<crate::event_bridge::TaskPauseCustody> {
        self.pause_custody
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Wire the custody the bridge restores before the reconcile and applies on a Resume.
    pub fn set_pause_custody(
        &self,
        api: &mut ExtensionApi,
        custody: crate::event_bridge::TaskPauseCustody,
    ) {
        *self
            .pause_subscription
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(custody.register(api));
        *self
            .pause_custody
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(custody);
    }

    pub fn bind_eval_workpools(
        &self,
        api: &mut ExtensionApi,
        workpools: Arc<tokio::sync::Mutex<senpi_task::workpool::engine::WorkpoolEngine>>,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let observer = self
            .eval_handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| maho_ext_api::ExtensionFailure::new("Task handle observer disposed"))?;
        let runtime = self.engine.runtime.clone();
        let manager = self.engine.manager.clone();
        self.engine
            .pause
            .register(Arc::new(self.engine.workpool_run_pause_source(
                workpools.clone(),
                manager.admission_barrier().clone(),
                Arc::new(move |parent| {
                    let records = manager
                        .list(&senpi_task::manager::types::ListScope::All)
                        .into_iter()
                        .map(|entry| entry.record)
                        .collect::<Vec<_>>();
                    let mut sessions = vec![parent.to_string()];
                    loop {
                        let before = sessions.len();
                        for record in &records {
                            if sessions.contains(&record.parent_session_id)
                                && let Some(child) = &record.child_session_id
                                    && !sessions.contains(child) {
                                        sessions.push(child.clone());
                                    }
                        }
                        if sessions.len() == before {
                            break;
                        }
                    }
                    sessions
                }),
            )));
        api.provide_eval_handle_host(Arc::new(
            crate::eval_handle_host::NativeEvalHandleHost::with_shared_workpools(
                self.engine.manager.clone(),
                workpools,
                observer,
                Arc::new(move || {
                    runtime
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .session_id()
                        .map(str::to_owned)
                }),
            )?,
        ));
        Ok(())
    }
    pub fn register_with_process_sweep(
        api: &mut ExtensionApi,
        engine: TaskEngine,
        spawn: senpi_task::tools::task::execute_spec::TaskToolDeps,
        ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps,
        member_process: bool,
        sweep: crate::process_sweep::SessionStartProcessSweepOptions,
    ) -> Result<Option<Arc<Self>>, maho_ext_api::ExtensionFailure> {
        if member_process {
            return Ok(None);
        }
        crate::process_sweep::wire_session_start_process_sweep(api, sweep);
        Self::register(api, engine, spawn, ownership, false)
    }
    pub fn register(
        api: &mut ExtensionApi,
        engine: TaskEngine,
        spawn: senpi_task::tools::task::execute_spec::TaskToolDeps,
        ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps,
        member_process: bool,
    ) -> Result<Option<Arc<Self>>, maho_ext_api::ExtensionFailure> {
        Self::register_with_status_timers(
            api,
            engine,
            spawn,
            ownership,
            member_process,
            Arc::new(HostTimers::default()),
        )
    }
    pub fn register_with_status_timers(
        api: &mut ExtensionApi,
        engine: TaskEngine,
        spawn: senpi_task::tools::task::execute_spec::TaskToolDeps,
        ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps,
        member_process: bool,
        timers: Arc<dyn crate::status_ui::StatusUiTimers>,
    ) -> Result<Option<Arc<Self>>, maho_ext_api::ExtensionFailure> {
        Self::register_with_dag_timers(
            api,
            engine,
            spawn,
            ownership,
            member_process,
            (
                timers,
                Arc::new(HostTimers::default()),
                Arc::new(HostTimers::default()),
            ),
        )
    }
    pub fn register_with_dag_timers(
        api: &mut ExtensionApi,
        engine: TaskEngine,
        spawn: senpi_task::tools::task::execute_spec::TaskToolDeps,
        ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps,
        member_process: bool,
        (timers, status_timers, rpc_timers): (
            Arc<dyn crate::status_ui::StatusUiTimers>,
            Arc<dyn crate::status_ui::StatusUiTimers>,
            Arc<dyn crate::status_ui::StatusUiTimers>,
        ),
    ) -> Result<Option<Arc<Self>>, maho_ext_api::ExtensionFailure> {
        if member_process {
            return Ok(None);
        }
        crate::registration::register_task_flags(api);
        if api.get_flag("omo-task") == Some(FlagValue::Boolean(false)) {
            return Ok(None);
        }
        let state = engine.runtime.clone();
        let session = Arc::new(move || {
            state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .session_id()
                .map(str::to_owned)
        });
        // The pause custody shares this session accessor; the channels take the original below.
        let custody_session = session.clone();
        let channels = OwnedResumptionChannels::new(
            api.events.clone(),
            Arc::new(TaskResumptionChannelManager {
                owned_workpool_task_ids: None,
                manager: engine.manager.clone(),
                ownership,
            }),
            session,
        )
        .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
        // The on-demand memory report's task section: registered once, read only when requested.
        if let Err(error) =
            crate::memory_reporter::register_task_memory_reporter(api, engine.memory.clone())
        {
            maho_ext_api::diagnostic(&format!("task memory reporter unavailable: {error}"));
        }
        let dag_status_timers = status_timers;
        let status = TaskStatusUi::new(
            engine.manager.clone(),
            engine.runtime.clone(),
            timers,
            Arc::new(|| chrono::Utc::now().timestamp_millis()),
            Arc::new(|| {
                use maho_tui::terminal::TerminalIo;
                maho_tui::terminal::ProcessIo
                    .size()
                    .map(|(columns, _)| usize::from(columns))
            }),
        );
        let transitions =
            SessionTransitionBridge::new(engine.runtime.clone(), engine.notifier.clone());
        let component = Arc::new(Self {
            engine,
            status,
            channels,
            transitions: Mutex::new(transitions),
            waiters: Mutex::new(BTreeMap::new()),
            disposed: std::sync::atomic::AtomicBool::new(false),
            rpc: Mutex::new(None),
            terminal: Mutex::new(None),
            delivery: Mutex::new(()),
            terminal_epochs: Mutex::new(std::collections::BTreeSet::new()),
            team: Mutex::new(None),
            team_routing: Arc::new(Mutex::new(None)),
            mutation_sync: Mutex::new(None),
            eval_handles: Mutex::new(None),
            before_suspend: Mutex::new(None),
            dag: Mutex::new(None),
            pause_custody: Mutex::new(None),
            pause_subscription: Mutex::new(None),
        });
        let eval_handles = Arc::new(crate::eval_handle_host::TaskMutationObserver::default());
        *component
            .eval_handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(eval_handles.clone());
        // The pause custody of this session: restored before the reconcile, resumed on the host's
        // Resume edge. The bridge shares the engine's controller, so a Stop and a Resume see one ledger.
        component.set_pause_custody(
            api,
            crate::event_bridge::TaskPauseCustody {
                bridge: Arc::new(crate::run_pause_bridge::EngineRunPauseBridge::new(
                    component.engine.pause.clone(),
                )),
                session_id: custody_session,
            },
        );
        let state_dir = component.engine.store.state_dir().to_path_buf();
        component.engine.store.set_write_listener(Some(Arc::new(
            move |record, previous_status| {
                if record.status.is_terminal()
                    && !previous_status.is_some_and(senpi_task::state::TaskStatus::is_terminal)
                {
                    shared_task_terminal_observers().notify(&TaskTerminalEdge {
                        record: record.clone(),
                        previous_status,
                        state_dir: state_dir.clone(),
                    });
                }
            },
        )));
        let weak_cost = Arc::downgrade(&component);
        component
            .engine
            .store
            .set_record_listener(Some(Arc::new(move |id, record| {
                if let Some(component) = weak_cost.upgrade() {
                    component.status.observe_record(
                        &crate::store_mutation_observer::StoreMutation {
                            task_id: id.into(),
                            previous_status: None,
                            record: record.cloned(),
                        },
                    );
                }
            })));
        let weak = Arc::downgrade(&component);
        let mutation_eval_handles = eval_handles.clone();
        component
            .engine
            .store
            .set_mutation_listener(Some(Arc::new(move || {
                mutation_eval_handles.notify();
                if let Some(component) = weak.upgrade() {
                    // pin `task/index.ts:112-120` statusUi.scheduleSync() and `event-bridge.ts:36` taskRpc.sync()
                    // run on every store mutation, so child subscriptions attach before a fast first tool event.
                    component.status.schedule_sync();
                    let rpc = component
                        .rpc
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    if let Some(rpc) = rpc {
                        rpc.sync();
                    }
                    let mut pending = component
                        .mutation_sync
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    if let Some(handle) = pending.take() {
                        component.status.timers.clear(handle);
                    }
                    let weak = Arc::downgrade(&component);
                    *pending = Some(component.status.timers.set(
                        Box::new(move || {
                            if let Some(component) = weak.upgrade() {
                                *component
                                    .mutation_sync
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner) = None;
                                component.sync();
                            }
                        }),
                        250,
                    ));
                    component.channels.emit_if_changed();
                }
            })));
        api.provide_eval_handle_host(Arc::new(
            crate::eval_handle_host::NativeEvalHandleHost::new(
                component.engine.manager.clone(),
                crate::workpool_admission::create_workpool_engine(&component.engine),
                eval_handles.clone(),
                {
                    let runtime = component.engine.runtime.clone();
                    Arc::new(move || {
                        runtime
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .session_id()
                            .map(str::to_owned)
                    })
                },
            ),
        ));
        let state = component.engine.runtime.clone();
        let events = api.events.clone();
        let rpc = crate::task_rpc_bridge::wire_task_rpc_bridge(
            api,
            component.engine.manager.clone(),
            Arc::new(move || {
                state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .session_id()
                    .map(str::to_owned)
            }),
            component
                .engine
                .store
                .state_dir()
                .to_string_lossy()
                .into_owned(),
            Arc::new(move |name, value| {
                events.emit(
                    "senpi:extension-rpc-event",
                    &serde_json::json!({"name":name,"data":value}),
                )
            }),
        )?;
        *component.rpc.lock().unwrap_or_else(PoisonError::into_inner) = Some(rpc);
        let tracker = crate::skill_invocation_tracker::SkillInvocationTracker::new()
            .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
        let dag_policy: Arc<
            dyn senpi_task::tools::task::spawn_policy::SpawnPolicyDeps + Send + Sync,
        > = tracker.clone();
        tracker.register(api);
        crate::renderers::register_task_message_renderers(api);
        crate::registration::register_removed_team_wait_hint(api);
        let weak = Arc::downgrade(&component);
        crate::commands::register_task_commands_with_sync(
            api,
            component.engine.manager.clone(),
            Arc::new(move || {
                if let Some(component) = weak.upgrade() {
                    component.sync();
                }
            }),
        );
        let weak = Arc::downgrade(&component);
        crate::tools::register_task_tools_with_sync(
            api,
            TaskToolsDeps {
                manager: component.engine.manager.clone(),
                state_dir: component
                    .engine
                    .store
                    .state_dir()
                    .to_string_lossy()
                    .into_owned(),
                omo_config: component.engine.config.clone(),
                agents: component.engine.agents.clone(),
                spawn,
                policy: tracker.clone(),
                team_routing: component.team_routing.clone(),
            },
            Arc::new(move || {
                if let Some(component) = weak.upgrade() {
                    component.sync();
                }
            }),
        );
        crate::reload_guard::wire_reload_guard(api, component.engine.manager.clone());
        let usage = api.get_flag("omo-task-usage-hint") != Some(FlagValue::Boolean(false));
        crate::event_bridge::wire_task_usage_guidance(api, Arc::new(move || usage));
        // The hosted child's own session context, read once: it names the task id this process may
        // claim on shutdown, and it must not be re-read per event.
        let shutdown_context = api.profile.session_context.clone();
        for kind in [
            EventKind::SessionStart,
            EventKind::SessionBeforeSwitch,
            EventKind::SessionBeforeCompact,
            EventKind::SessionCompact,
            EventKind::SessionShutdown,
            EventKind::ModelSelect,
            EventKind::AgentEnd,
        ] {
            let component = component.clone();
            let shutdown_context = shutdown_context.clone();
            api.on(
                kind,
                Arc::new(move |event, context| {
                    let component = component.clone();
                    let shutdown_context = shutdown_context.clone();
                    Box::pin(async move {
                        component
                            .engine
                            .runtime
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .capture_from(context);
                        let session = context.session_manager.session_id();
                        match event {
                            ExtensionEvent::SessionStart(_) => {
                                component
                                    .transitions
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .resolve(Some(session))
                                    .map_err(|error| {
                                        maho_ext_api::ExtensionFailure::new(error.to_string())
                                    })?;
                                // Custody is restored BEFORE the reconcile reads records: a
                                // restored Stop must already be in place when it scans them.
                                let custody = component
                                    .pause_custody
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .clone();
                                if let Some(custody) = &custody
                                    && let Some(pause_seq) = custody.restore()
                                {
                                    maho_ext_api::diagnostic(&format!(
                                        "task session start restored a Stop at pause sequence {pause_seq}"
                                    ));
                                }
                                let (lifecycle, owned_session) =
                                    (component.engine.lifecycle.clone(), session.to_owned());
                                let reconciliation = tokio::task::spawn_blocking(move || {
                                    lifecycle.reconcile_on_session_start(Some(&owned_session))
                                })
                                .await
                                .map_err(|error| {
                                    maho_ext_api::ExtensionFailure::new(error.to_string())
                                })?
                                .map_err(|error| {
                                    maho_ext_api::ExtensionFailure::new(error.to_string())
                                })?;
                                let mut records = Vec::<senpi_task::state::TaskRecord>::new();
                                for outcome in reconciliation.outcomes {
                                    if let Some(record) =
                                        component.engine.manager.get(&outcome.task_id)
                                    {
                                        records.push(record);
                                    }
                                }
                                for entry in component
                                    .engine
                                    .manager
                                    .list(&ListScope::ParentSession(session.into()))
                                {
                                    if let Some(record) = records
                                        .iter_mut()
                                        .find(|record| record.task_id == entry.record.task_id)
                                    {
                                        *record = entry.record;
                                    } else {
                                        records.push(entry.record);
                                    }
                                }
                                for record in &records {
                                    component.notify_owned_terminal(record);
                                }
                                component
                                    .channels
                                    .emit_session_start()
                                    .await
                                    .map_err(maho_ext_api::ExtensionFailure::new)?;
                                let team = component
                                    .team
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .clone();
                                if let Some(team) = &team {
                                    team.service.reconcile_mailbox();
                                }
                                let parent_state = component
                                    .engine
                                    .runtime
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .parent_state();
                                let delivery = component
                                    .delivery
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner);
                                component
                                    .engine
                                    .notifier
                                    .reconcile_unnotified_notifications(
                                        ReconcileUnnotifiedNotificationsInput {
                                            session_id: session,
                                            parent_state,
                                        },
                                    )
                                    .map_err(|error| {
                                        maho_ext_api::ExtensionFailure::new(error.to_string())
                                    })?;
                                drop(delivery);
                                let lifecycle = component.engine.lifecycle.clone();
                                tokio::task::spawn_blocking(move || {
                                    lifecycle.cleanup_expired_records()
                                })
                                .await
                                .map_err(|error| {
                                    maho_ext_api::ExtensionFailure::new(error.to_string())
                                })?
                                .map_err(|error| {
                                    maho_ext_api::ExtensionFailure::new(error.to_string())
                                })?;
                                if let Some(team) = team
                                    && let Err(error) = team.pollers.tick()
                                {
                                    maho_ext_api::diagnostic(&format!(
                                        "task session-start lead poll failed: {error}"
                                    ));
                                }
                                // Upstream order: statusUi.scheduleSync() then taskRpc.attach().
                                component.status.schedule_sync();
                                if let Some(rpc) =
                                    &*component.rpc.lock().unwrap_or_else(PoisonError::into_inner)
                                {
                                    rpc.attach();
                                }
                                // Native-only tail: arm completion waiters for still-running tasks. Its terminal
                                // re-delivery cannot double-notify what reconcile_unnotified_notifications already
                                // queued: the notifier skips notified_epoch >= run_epoch and in-flight `pending`
                                // keys, and owned-member liveness is deduped by `terminal_epochs`.
                                component.sync();
                            }
                            ExtensionEvent::SessionBeforeSwitch { .. } => {
                                if let Some(rpc) =
                                    &*component.rpc.lock().unwrap_or_else(PoisonError::into_inner)
                                {
                                    rpc.detach();
                                }
                                component
                                    .transitions
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .mark(TransitionReason::SessionSwitching, Some(session));
                                component
                                    .engine
                                    .runtime
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .clear_ui();
                            }
                            ExtensionEvent::SessionBeforeCompact(_) => component
                                .transitions
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .mark(TransitionReason::Compacting, Some(session)),
                            ExtensionEvent::SessionCompact(_) => {
                                component
                                    .transitions
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .resolve(Some(session))
                                    .map_err(|error| {
                                        maho_ext_api::ExtensionFailure::new(error.to_string())
                                    })?;
                                component.sync();
                            }
                            ExtensionEvent::SessionShutdown(shutdown) => {
                                let manager = component.engine.manager.clone();
                                tokio::task::spawn_blocking(move || manager.settle_launches_for_shutdown(std::time::Duration::from_secs(30)))
                                    .await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                                let before_suspend = component
                                    .before_suspend
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .clone();
                                if let Some(before_suspend) = before_suspend {
                                    let owned_session = session.to_owned();
                                    tokio::task::spawn_blocking(move || {
                                        before_suspend(&owned_session)
                                    })
                                    .await
                                    .map_err(|error| {
                                        maho_ext_api::ExtensionFailure::new(error.to_string())
                                    })??;
                                }
                                component
                                    .transitions
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .mark(TransitionReason::SessionShutdown, Some(session));
                                // Upstream order (DAG pauseForShutdown ran first via before_suspend): taskRpc.dispose,
                                // clearUi, statusUi.dispose, leadPollers.shutdown, awaited emitShutdown, then suspend.
                                // `dispose()` runs rpc/status/pollers in that order, then joins the resumption worker,
                                // whose exit awaits the zero-count `wake_source_state` emission before returning.
                                component
                                    .engine
                                    .runtime
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .clear_ui();
                                let cleanup = component.clone();
                                tokio::task::spawn_blocking(move || cleanup.dispose())
                                    .await
                                    .map_err(|error| {
                                        maho_ext_api::ExtensionFailure::new(error.to_string())
                                    })?;
                                let lifecycle = component.engine.lifecycle.clone();
                                // A hosted child suspends under ITS OWN task id, and only when the
                                // canonical state dir proves the identity is ours.
                                let owner = crate::event_bridge::shutdown_owner(
                                    session,
                                    &shutdown_context,
                                    component.engine.store.state_dir(),
                                );
                                let input = senpi_task::lifecycle::SuspendInput {
                                    own_task_id: (owner != session).then_some(owner),
                                    parent_session_id: session.into(),
                                    reason: match shutdown.reason {
                                        maho_ext_api::SessionReason::Startup => "startup",
                                        maho_ext_api::SessionReason::Reload => "reload",
                                        maho_ext_api::SessionReason::New => "new",
                                        maho_ext_api::SessionReason::Resume => "resume",
                                        maho_ext_api::SessionReason::Fork => "fork",
                                        maho_ext_api::SessionReason::Quit => "quit",
                                    }
                                    .into(),
                                };
                                tokio::task::spawn_blocking(move || {
                                    lifecycle.suspend_on_session_shutdown(&input)
                                })
                                .await
                                .map_err(|error| {
                                    maho_ext_api::ExtensionFailure::new(error.to_string())
                                })?
                                .map_err(|error| {
                                    maho_ext_api::ExtensionFailure::new(error.to_string())
                                })?;
                            }
                            ExtensionEvent::AgentEnd { .. } => {
                                // Upstream `queueMicrotask(() => coordinator.flushOnIdle())`: drain completions, team
                                // mail and DAG summaries that arrived mid-turn on the idle edge, after this handler.
                                if let Some(coordinator) = context.idle_coordinator.clone() {
                                    match &context.defer_macrotask {
                                        Some(defer) => defer(Box::new(move || {
                                            coordinator.flush_on_idle();
                                        })),
                                        None => {
                                            coordinator.flush_on_idle();
                                        }
                                    }
                                }
                                let team = component
                                    .team
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .clone();
                                if let Some(team) = team {
                                    let runtime = component.engine.runtime.clone();
                                    team.liveness.acknowledge_persisted(Arc::new(move || {
                                        runtime
                                            .lock()
                                            .unwrap_or_else(PoisonError::into_inner)
                                            .session_file()
                                            .map(ToOwned::to_owned)
                                    }));
                                }
                                component.sync();
                            }
                            ExtensionEvent::ModelSelect(_) => component.status.schedule_sync(),
                            _ => component.sync(),
                        }
                        Ok(EventResult::None)
                    })
                }),
            );
        }
        // TS index.ts createDagRuntime: the task spawn policy governs agent-routed nodes, the session's
        // ancestry sets child depth, and node skills are materialized through the task skill loader.
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let materialize_store_cwd = component.engine.runtime.clone();
        let scheduling = crate::dag_engine::DagRuntimeSchedulingOptions {
            ancestry: crate::dag_engine::read_session_ancestry(&api.profile.session_context, &env),
            node_spawn_policy: Some(crate::dag_engine::dag_node_spawn_policy(dag_policy)),
            auto_mode: None,
            lease_watch: None,
            retention_sweep: None,
            logger: None,
        };
        let dag_store_for_skills: Arc<
            std::sync::OnceLock<Arc<senpi_task::dag::store::DagFileStore>>,
        > = Arc::new(std::sync::OnceLock::new());
        let materialize_store = dag_store_for_skills.clone();
        let materialize: senpi_task::dag::manager::DagMaterializeSkills = Arc::new(move |input| {
            let cwd = materialize_store_cwd
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .cwd()
                .to_string_lossy()
                .into_owned();
            let store = materialize_store.get().cloned().ok_or_else(|| {
                senpi_task::dag::manager::DagManagerError::new(
                    senpi_task::dag::manager::DagManagerErrorCode::InvalidDefinition,
                    "DAG store is not ready for skill materialization",
                )
            })?;
            senpi_task::dag::skills::create_dag_skill_materializer(
                senpi_task::dag::skills::DagSkillMaterializerOptions {
                    store,
                    cwd,
                    load_skills: None,
                    home_dir: None,
                    extra_dirs: vec![],
                },
            )(input)
        });
        let dag = crate::dag_engine::TaskDagEngine::compose_with_options(
            &component.engine,
            Some(materialize),
            scheduling,
        )
        .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
        let _ = dag_store_for_skills.set(dag.store.clone());
        dag.register_queries(api, &component)?;
        dag.register_rpc_with_timers(api, &component, dag_status_timers, rpc_timers);
        dag.register_tool(api, component.clone());
        *component.dag.lock().unwrap_or_else(PoisonError::into_inner) = Some(dag);
        Ok(Some(component))
    }
    pub fn sync(self: &Arc<Self>) {
        if self.disposed.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        self.status.schedule_sync();
        let rpc = self
            .rpc
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(rpc) = rpc {
            rpc.sync();
        }
        self.channels.emit_if_changed();
        let entries = self.engine.manager.list(&ListScope::All);
        {
            let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
            let stale: Vec<_> = waiters
                .keys()
                .filter(|id| !entries.iter().any(|entry| &entry.record.task_id == *id))
                .cloned()
                .collect();
            let retired = stale
                .into_iter()
                .filter_map(|id| waiters.remove(&id).map(|waiter| (id, waiter)))
                .collect();
            drop(waiters);
            retire_completion_waiters(retired);
        }
        for entry in entries {
            let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
            if self.disposed.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let finished: Vec<_> = waiters
                .iter()
                .filter(|(_, waiter)| waiter.thread.is_finished())
                .map(|(id, _)| id.clone())
                .collect();
            for id in finished {
                if let Some(waiter) = waiters.remove(&id)
                    && waiter.thread.join().is_err()
                {
                    maho_ext_api::diagnostic("task completion waiter panicked");
                }
            }
            if entry.record.status.is_terminal() {
                // A task settled by another process never wakes this process's in-memory waiter, so
                // Wake it here, retaining its handle until the next sync or shutdown joins it.
                // Terminal delivery below is safe even if its waiter races us: the notifier dedupes epochs.
                if let Some(waiter) = waiters.get(&entry.record.task_id) {
                    waiter.signal.abort();
                }
                drop(waiters);
                self.notify_owned_terminal(&entry.record);
                // Already-notified terminals are a no-op in `notify_terminal` (AlreadyNotified) after a store load; skip that per-sync.
                if entry.record.notification.notified_epoch >= entry.record.notification.run_epoch {
                    continue;
                }
                let parent_state = self
                    .engine
                    .runtime
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .parent_state();
                let delivery = self.delivery.lock().unwrap_or_else(PoisonError::into_inner);
                if let Err(error) = self.engine.notifier.notify_terminal(&CompletionRequest {
                    run_in_background: self.engine.manager.was_background(&entry.record.task_id),
                    record: entry.record,
                    parent_state,
                    tokens: None,
                }) {
                    maho_ext_api::diagnostic(&format!("task terminal delivery failed: {error}"));
                }
                drop(delivery);
                continue;
            }
            if waiters.contains_key(&entry.record.task_id) {
                continue;
            }
            let id = entry.record.task_id;
            let manager = self.engine.manager.clone();
            let signal = AbortSignal::default();
            let abort = signal.clone();
            let weak = Arc::downgrade(self);
            let task = id.clone();
            let thread = std::thread::spawn(move || {
                if let Ok(record) = manager.wait_for(&task, Some(&abort), None)
                    && let Some(component) = weak.upgrade()
                {
                    if component.disposed.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    component.notify_owned_terminal(&record);
                    let parent_state = component
                        .engine
                        .runtime
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .parent_state();
                    let delivery = component
                        .delivery
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    if let Err(error) =
                        component
                            .engine
                            .notifier
                            .notify_terminal(&CompletionRequest {
                                record,
                                parent_state,
                                run_in_background: manager.was_background(&task),
                                tokens: None,
                            })
                    {
                        maho_ext_api::diagnostic(&format!(
                            "task terminal delivery failed: {error}"
                        ));
                    }
                    drop(delivery);
                    component.channels.emit_if_changed();
                    component.status.schedule_sync();
                    let rpc = component
                        .rpc
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    if let Some(rpc) = rpc {
                        rpc.sync();
                    }
                }
            });
            waiters.insert(id, CompletionWaiter { signal, thread });
        }
    }
    pub fn set_before_suspend(&self, callback: BeforeSuspendCallback) {
        *self
            .before_suspend
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(callback);
    }
    pub fn dag_engine(&self) -> Option<Arc<crate::dag_engine::TaskDagEngine>> {
        self.dag
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    fn notify_owned_terminal(&self, record: &senpi_task::state::TaskRecord) {
        if crate::member_liveness::liveness_details(record).is_none() {
            return;
        }
        let terminal = self
            .terminal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(terminal) = terminal {
            let key = (record.task_id.clone(), record.notification.run_epoch);
            let fresh = self
                .terminal_epochs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key);
            if fresh {
                terminal(record);
            }
        }
    }
    pub fn register_team_runtime(
        self: &Arc<Self>,
        api: &mut ExtensionApi,
        service: Arc<crate::team_service::TeamService>,
        pollers: Arc<crate::lead_poller_lifecycle::LeadPollerLifecycle>,
        liveness: Arc<crate::member_liveness::TeamMemberLivenessNotifier>,
        ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps,
    ) {
        crate::tools::register_lead_team_tools(api, service.clone());
        let resolver = pollers.clone();
        self.set_team_routing(Some(senpi_task::tools::control::send_shutdown::TaskSendTeamRouting {
            service: service.clone(), from: senpi_task::team::normalize::TEAM_LEAD_SENTINEL.into(), team_run_id: None,
            resolve_default_team_run_id: Some(Arc::new(move || {
                use crate::lead_poller_lifecycle::DefaultTeamRunIdResolution as Owned;
                use senpi_task::tools::control::send_shutdown::DefaultTeamRunIdResolution as Send;
                match resolver.resolve_default_team_run_id() {
                    Ok(Owned::Resolved(team_run_id)) => Send::Resolved { team_run_id },
                    Ok(Owned::None) => Send::None,
                    Ok(Owned::Ambiguous(reason)) => Send::Ambiguous { reason },
                    Err(reason) => Send::Failed { reason },
                }
            })),
        }));
        let runtime = self.engine.runtime.clone();
        *self.terminal.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(
            crate::owned_member_liveness::create_owned_member_liveness_notifier(
                ownership,
                Arc::new(move || {
                    runtime
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .session_id()
                        .map(str::to_owned)
                }),
                liveness.clone(),
            ),
        ));
        *self.team.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(TeamRuntime {
            service,
            pollers,
            liveness,
        }));
    }
    pub fn set_team_routing(
        &self,
        routing: Option<senpi_task::tools::control::send_shutdown::TaskSendTeamRouting>,
    ) {
        *self
            .team_routing
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = routing;
    }
    pub fn dispose(&self) {
        if self
            .disposed
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        self.set_team_routing(None);
        self.engine.store.set_record_listener(None);
        self.engine.store.set_mutation_listener(None);
        self.engine.store.set_write_listener(None);
        if let Some(observer) = self
            .eval_handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            observer.close();
        }
        if let Some(handle) = self
            .mutation_sync
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            self.status.timers.clear(handle);
        }
        let waiters =
            std::mem::take(&mut *self.waiters.lock().unwrap_or_else(PoisonError::into_inner));
        retire_completion_waiters(waiters);
        if let Some(rpc) = self
            .rpc
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            rpc.dispose();
        }
        self.status.dispose();
        if let Some(team) = self
            .team
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            team.pollers.shutdown();
        }
        self.channels.dispose();
    }
}

#[cfg(test)]
mod waiter_tests {
    use super::*;
    #[test]
    fn retirement_aborts_every_waiter_without_joining_the_calling_thread() {
        let (release, blocked) = std::sync::mpsc::channel();
        let (settle, settled) = std::sync::mpsc::channel();
        let signal = AbortSignal::default();
        let observed = signal.clone();
        let thread = std::thread::spawn(move || {
            blocked
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            settle.send(observed.aborted()).unwrap();
        });
        let waiters = BTreeMap::from([(
            "fixture".to_owned(),
            CompletionWaiter {
                signal: signal.clone(),
                thread,
            },
        )]);
        retire_completion_waiters(waiters);
        assert!(signal.aborted());
        release.send(()).unwrap();
        assert!(
            settled
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
        );
    }
}
