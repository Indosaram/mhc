use std::{collections::BTreeMap, sync::{Arc, Mutex, PoisonError}};
use senpi_task::dag::{manager::{DagManager, DagManagerOptions, create_dag_manager}, scheduler::{DagSchedulerOptions, DagSchedulerContext, create_dag_scheduler}, store::DagFileStore};
use crate::engine::TaskEngine;
pub struct TaskDagEngine {
    pub manager: DagManager,
    pub store: Arc<DagFileStore>,
    recovery: Arc<senpi_task::dag::recovery::DagRecovery>,
    settings: senpi_task::dag::types::DagSettings,
    schedulers: Arc<Mutex<BTreeMap<String, Arc<DagSchedulerContext>>>>,
    rpc: Mutex<Option<Arc<crate::dag_rpc_bridge::DagRpcBridge>>>,
    surfaces: Mutex<Option<Arc<DagSurfaces>>>,
}
struct DagSurfaces {
    status: Arc<crate::dag_status_ui::DagStatusUi>,
    wake: crate::dag_wake_source::DagWakeSource,
    activity: Mutex<BTreeMap<String, (String, String, senpi_task::manager::Unsubscribe)>>,
}
impl DagSurfaces {
    fn clear_activity(&self) {
        let subscriptions = std::mem::take(&mut *self.activity.lock().unwrap_or_else(PoisonError::into_inner));
        for (_, _, unsubscribe) in subscriptions.into_values() { unsubscribe(); }
    }
    fn bind_activity(self: &Arc<Self>, run_id: &str, node_id: &str, task_id: &str, tasks: &senpi_task::manager::TaskManager, bridge: &Arc<crate::dag_rpc_bridge::DagRpcBridge>) {
                if self.activity.lock().unwrap_or_else(PoisonError::into_inner).contains_key(task_id) { return; }
                let Some(record) = tasks.get(task_id) else { return; };
                let started_at = chrono::DateTime::parse_from_rfc3339(&record.created_at).map_or(0, |date| date.timestamp_millis().max(0) as u64);
                let progress = Mutex::new(senpi_task::progress::create_child_progress(task_id, senpi_task::progress::ChildProgressTarget {
                    name:record.name, task_summary:record.task_summary, description:record.description, category:record.category,
                    agent_type:record.agent_type, resolved_model:record.resolved_model, model:Some(record.model),
                }, started_at, || chrono::Utc::now().timestamp_millis().max(0) as u64));
                let status = Arc::downgrade(&self.status); let bridge = Arc::downgrade(bridge);
                let run = run_id.to_owned(); let node = node_id.to_owned(); let task = task_id.to_owned();
                let unsubscribe = tasks.subscribe_child(task_id, Arc::new(move |event| {
                    let details = {
                        let mut progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
                        if !progress.accept(event) { return; }
                        progress.details()
                    };
                    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                    if let Some(bridge) = bridge.upgrade() { bridge.publish_activity(crate::dag_runtime::dag_activity_payload(&run, &node, &task, &at, &details)); }
                    if let Some(status) = status.upgrade() { status.on_activity(&run, &node, details.current_tool.as_deref().or(details.last_assistant_line.as_deref()).unwrap_or(&details.progress.activity)); }
                }));
                let duplicate = {
                    let mut activity = self.activity.lock().unwrap_or_else(PoisonError::into_inner);
                    match activity.entry(task_id.to_owned()) {
                        std::collections::btree_map::Entry::Vacant(entry) => { entry.insert((run_id.to_owned(), node_id.to_owned(), unsubscribe)); None }
                        std::collections::btree_map::Entry::Occupied(_) => Some(unsubscribe),
                    }
                };
                if let Some(unsubscribe) = duplicate { unsubscribe(); }
    }
    fn reconcile_activity(self: &Arc<Self>, manager: &DagManager, session: Option<&str>, tasks: &senpi_task::manager::TaskManager, bridge: &Arc<crate::dag_rpc_bridge::DagRpcBridge>) -> Result<(), senpi_task::dag::manager::DagManagerError> {
        let mut wanted = BTreeMap::new();
        if let Some(session) = session {
            for run in manager.list(session, None)? {
                if run.status.is_terminal() { continue; }
                for node in manager.record(&run.run_id, session)?.nodes {
                    if matches!(node.state, senpi_task::dag::types::DagNodeState::Completed | senpi_task::dag::types::DagNodeState::Failed | senpi_task::dag::types::DagNodeState::Cancelled | senpi_task::dag::types::DagNodeState::Skipped) { continue; }
                    if let Some(task) = node.task_id { wanted.insert(task, (run.run_id.clone(), node.id)); }
                }
            }
        }
        let stale = {
            let mut activity = self.activity.lock().unwrap_or_else(PoisonError::into_inner);
            let stale: Vec<_> = activity.iter().filter(|(task, (run, node, _))| wanted.get(*task).is_none_or(|owner| &owner.0 != run || &owner.1 != node)).map(|(task, _)| task.clone()).collect();
            stale.into_iter().filter_map(|task| activity.remove(&task)).collect::<Vec<_>>()
        };
        for (_, _, unsubscribe) in stale { unsubscribe(); }
        for (task, (run, node)) in wanted { self.bind_activity(&run, &node, &task, tasks, bridge); }
        Ok(())
    }
    fn on_event(self: &Arc<Self>, event: &senpi_task::dag::types::DagRunEvent, tasks: &senpi_task::manager::TaskManager, bridge: &Arc<crate::dag_rpc_bridge::DagRpcBridge>) {
        use senpi_task::dag::types::DagRunEventPayload;
        match &event.payload {
            DagRunEventPayload::NodeTaskAttached { node_id, task_id, .. } => self.bind_activity(&event.run_id, node_id, task_id, tasks, bridge),
            DagRunEventPayload::NodeTransitioned { node_id, to: senpi_task::dag::types::DagNodeState::Completed | senpi_task::dag::types::DagNodeState::Failed | senpi_task::dag::types::DagNodeState::Cancelled | senpi_task::dag::types::DagNodeState::Skipped, .. } => {
                let stale = {
                    let mut subscriptions = self.activity.lock().unwrap_or_else(PoisonError::into_inner);
                    let ids: Vec<_> = subscriptions.iter().filter(|(_, (run, node, _))| run == &event.run_id && node == node_id).map(|(task, _)| task.clone()).collect();
                    ids.into_iter().filter_map(|task| subscriptions.remove(&task)).collect::<Vec<_>>()
                };
                for (_, _, unsubscribe) in stale { unsubscribe(); }
            }
            DagRunEventPayload::RunCompleted { .. } | DagRunEventPayload::RunFailed { .. } | DagRunEventPayload::RunCancelled { .. } => {
                let stale = {
                    let mut subscriptions = self.activity.lock().unwrap_or_else(PoisonError::into_inner);
                    let ids: Vec<_> = subscriptions.iter().filter(|(_, (run, _, _))| run == &event.run_id).map(|(task, _)| task.clone()).collect();
                    ids.into_iter().filter_map(|task| subscriptions.remove(&task)).collect::<Vec<_>>()
                };
                for (_, _, unsubscribe) in stale { unsubscribe(); }
                self.wake.publish_live();
            }
            _ => {}
        }
        self.status.schedule_sync();
    }
}
impl crate::dag_status_ui::DagStatusUiManager for DagManager {
    fn list(&self, session: &str) -> Vec<senpi_task::dag::manager::DagRunSummary> {
        match self.list(session, None) { Ok(runs) => runs, Err(error) => { eprintln!("DAG status list failed: {error}"); vec![] } }
    }
    fn snapshot(&self, run: &str, session: &str) -> Option<senpi_task::dag::types::DagRunSnapshot> {
        match self.snapshot(&run.to_owned(), session) { Ok(snapshot) => Some(snapshot), Err(error) => { eprintln!("DAG status snapshot failed: {error}"); None } }
    }
}
impl TaskDagEngine {
    pub fn register_queries(&self, api: &mut maho_ext_api::ExtensionApi, component: &crate::component::TaskComponent) -> Result<(), maho_ext_api::ExtensionFailure> {
        let runtime = component.engine.runtime.clone();
        crate::dag_commands::register_dag_commands(api, Arc::new(crate::dag_commands::DagCommandAdapter { manager:self.manager.clone(), tasks:(*component.engine.manager).clone() }));
        crate::dag_rpc_handlers::register_dag_rpc_handlers(api, self.manager.clone(), Arc::new(move || runtime.lock().unwrap_or_else(PoisonError::into_inner).session_id().map(str::to_owned)))
    }
    pub fn compose(engine: &TaskEngine, materialize: Option<senpi_task::dag::manager::DagMaterializeSkills>) -> Result<Self, senpi_task::dag::store::DagStoreError> {
        let mut settings = senpi_task::dag::types::DagSettings::default();
        let config = &engine.config["task"]["dag"];
        macro_rules! resolved { ($($field:ident),*) => { $(if let Some(value) = config[stringify!($field)].as_u64() { settings.$field = value.try_into().map_err(|_| senpi_task::dag::store::DagStoreError::Message(format!("invalid dag setting {}", stringify!($field))))?; })* }; }
        resolved!(max_nodes_per_run, max_runs_per_session, subscriber_ring, heartbeat_ms, history_default_limit, history_max_limit, retention_days, max_prompt_bytes);
        let store = Arc::new(senpi_task::dag::store::create_dag_file_store(&senpi_task::dag::store::DagStoreConfig { project_dir:engine.runtime.lock().unwrap_or_else(PoisonError::into_inner).cwd().into(), task:Some(senpi_task::dag::store::DagStoreTaskConfig { state_dir:Some(engine.store.state_dir().into()), dag:Some(senpi_task::dag::store::DagSettingsOverrides { max_nodes_per_run:Some(settings.max_nodes_per_run), max_runs_per_session:Some(settings.max_runs_per_session), subscriber_ring:Some(settings.subscriber_ring), heartbeat_ms:Some(settings.heartbeat_ms), history_default_limit:Some(settings.history_default_limit), history_max_limit:Some(settings.history_max_limit), retention_days:Some(settings.retention_days), max_prompt_bytes:Some(settings.max_prompt_bytes) }) }) }, Default::default())?);
        let schedulers = Arc::new(Mutex::new(BTreeMap::<String, Arc<DagSchedulerContext>>::new()));
        let options = DagManagerOptions { store:store.clone(), new_run_id:None, now:None, materialize_skills:materialize, settings:Some(settings) };
        let stopped = schedulers.clone();
        let recovery = Arc::new(senpi_task::dag::recovery::create_dag_recovery(senpi_task::dag::recovery::DagRecoveryOptions {
            store:store.clone(), task_manager:engine.manager.clone(), host_pid:None, is_process_alive:None, now:None,
            subscriber_ring:Some(settings.subscriber_ring),
            stop_admission:Some(Arc::new(move |run| {
                let scheduler = stopped.lock().unwrap_or_else(PoisonError::into_inner).get(run).cloned();
                if let Some(scheduler) = scheduler { scheduler.stop_admission(); }
            })), reattach:None,
        }));
        Ok(Self { manager:create_dag_manager(options), store, recovery, settings, schedulers, rpc:Mutex::new(None), surfaces:Mutex::new(None) })
    }
    pub fn scheduler(&self, engine: &TaskEngine, run: &str, session: &str) -> Result<Arc<DagSchedulerContext>, senpi_task::dag::manager::DagManagerError> {
        let mut schedulers = self.schedulers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(scheduler) = schedulers.get(run) { return Ok(scheduler.clone()); }
        let initial_record = self.manager.record(&run.to_owned(), session)?;
        let scheduler = create_dag_scheduler(DagSchedulerOptions { store:self.store.clone(), task_manager:engine.manager.clone(), initial_record, execution_mode_agents:Some(Arc::new(engine.agents.clone())), execution_mode_config:engine.config["task"]["default_execution_mode"].as_str().and_then(senpi_task::manager::execution_mode::ExecutionMode::parse), ancestry_depth:None, subscriber_ring:Some(self.settings.subscriber_ring), now:None }).map_err(senpi_task::dag::manager::DagManagerError::from)?;
        schedulers.insert(run.into(), scheduler.clone()); Ok(scheduler)
    }
    pub fn register_rpc(self: &Arc<Self>, api: &mut maho_ext_api::ExtensionApi, component: &crate::component::TaskComponent) {
        self.register_rpc_with_status_timers(api, component, Arc::new(crate::timers::HostTimers::default()));
    }
    pub fn register_rpc_with_status_timers(self: &Arc<Self>, api: &mut maho_ext_api::ExtensionApi, component: &crate::component::TaskComponent, timers: Arc<dyn crate::status_ui::StatusUiTimers>) {
        self.register_rpc_with_timers(api, component, timers, Arc::new(crate::timers::HostTimers::default()));
    }
    pub fn register_rpc_with_timers(self: &Arc<Self>, api: &mut maho_ext_api::ExtensionApi, component: &crate::component::TaskComponent, timers: Arc<dyn crate::status_ui::StatusUiTimers>, rpc_timers: Arc<dyn crate::status_ui::StatusUiTimers>) {
        let recovery = self.recovery.clone();
        component.set_before_suspend(Arc::new(move |session| recovery.try_pause_runs_for_shutdown(session).map(|_| ()).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))));
        let runtime = component.engine.runtime.clone();
        let status = crate::dag_status_ui::DagStatusUi::new(Arc::new(self.manager.clone()), runtime.clone(), timers);
        let session: Arc<dyn Fn() -> Option<String> + Send + Sync> = Arc::new(move || runtime.lock().unwrap_or_else(PoisonError::into_inner).session_id().map(str::to_owned));
        let surfaces = Arc::new(DagSurfaces { status, wake:crate::dag_wake_source::DagWakeSource { events:api.events.clone(), manager:Arc::new(self.manager.clone()), session_id:session.clone() }, activity:Mutex::new(BTreeMap::new()) });
        *self.surfaces.lock().unwrap_or_else(PoisonError::into_inner) = Some(surfaces.clone());
        let live = Arc::downgrade(self); let live_session = session.clone();
        let snapshots = Arc::downgrade(self); let snapshot_session = session.clone();
        let events = api.events.clone();
        let tasks = component.engine.manager.clone();
        let lifecycle_tasks = tasks.clone();
        let bridge = crate::dag_rpc_bridge::create_dag_rpc_bridge(crate::dag_rpc_bridge_contract::DagRpcBridgeDeps {
            live_runs: Arc::new(move || {
                let (Some(dag), Some(session)) = (live.upgrade(), live_session()) else { return vec![]; };
                let runs = match dag.manager.list(&session, None) { Ok(runs) => runs, Err(error) => { eprintln!("DAG live run list failed: {error}"); return vec![]; } };
                runs.into_iter().map(|run| {
                    let store = dag.store.clone(); let id = run.run_id.clone();
                    let owner = Arc::downgrade(&dag); let tasks = tasks.clone();
                    crate::dag_rpc_bridge_contract::DagBridgeRun { run_id:run.run_id, status:run.status.as_str().into(), subscribe:Arc::new(move |listener| {
                        let owner = owner.clone(); let tasks = tasks.clone();
                        senpi_task::dag::journal::subscribe_dag_journal(&store, &id, Arc::new(move |event| {
                            if let Some(dag) = owner.upgrade() {
                                let surfaces = dag.surfaces.lock().unwrap_or_else(PoisonError::into_inner).clone();
                                let bridge = dag.rpc.lock().unwrap_or_else(PoisonError::into_inner).clone();
                                if let (Some(surfaces), Some(bridge)) = (surfaces, bridge) { surfaces.on_event(event, &tasks, &bridge); }
                            }
                            match serde_json::to_value(event) { Ok(event) => listener(&event), Err(error) => eprintln!("DAG event serialization failed: {error}") }
                        }))
                    }) }
                }).collect()
            }),
            run_snapshots: Some(Arc::new(move || {
                let (Some(dag), Some(session)) = (snapshots.upgrade(), snapshot_session()) else { return vec![]; };
                match dag.manager.list(&session, None) {
                    Ok(runs) => runs.into_iter().filter_map(|run| match dag.manager.snapshot(&run.run_id, &session) { Ok(snapshot) => Some((snapshot, run.status.as_str().into())), Err(error) => { eprintln!("DAG snapshot failed: {error}"); None } }).collect(),
                    Err(error) => { eprintln!("DAG snapshot list failed: {error}"); vec![] }
                }
            })),
            parent_session_id: session.clone(),
            emit: Arc::new(move |name, data| events.emit("senpi:extension-rpc-event", &serde_json::json!({"name":name,"data":data}))),
            timers: rpc_timers, now: Arc::new(|| chrono::Utc::now().timestamp_millis()), heartbeat_ms:Some(self.settings.heartbeat_ms), activity_coalesce_ms:None, snapshot_debounce_ms:None,
        });
        *self.rpc.lock().unwrap_or_else(PoisonError::into_inner) = Some(bridge.clone());
        let owner = Arc::downgrade(self);
        self.store.set_checkpoint_listener(Some(Arc::new(move || {
            if let Some(dag) = owner.upgrade() {
                if let Some(bridge) = dag.rpc.lock().unwrap_or_else(PoisonError::into_inner).clone() { bridge.notify_store_mutation(); }
                if let Some(surfaces) = dag.surfaces.lock().unwrap_or_else(PoisonError::into_inner).clone() { surfaces.status.schedule_sync(); }
            }
        })));
        for kind in [maho_ext_api::EventKind::SessionStart, maho_ext_api::EventKind::SessionBeforeSwitch, maho_ext_api::EventKind::SessionShutdown] {
            let bridge = bridge.clone();
            let surfaces = surfaces.clone();
            let manager = self.manager.clone(); let tasks = lifecycle_tasks.clone(); let session = session.clone();
            let recovery = self.recovery.clone();
            let schedulers = self.schedulers.clone();
            let store = self.store.clone();
            api.on(kind, Arc::new(move |event, _| { let bridge = bridge.clone(); let surfaces = surfaces.clone(); let manager = manager.clone(); let tasks = tasks.clone(); let session = session.clone(); let recovery = recovery.clone(); let schedulers = schedulers.clone(); let store = store.clone(); Box::pin(async move {
                match event {
                    maho_ext_api::ExtensionEvent::SessionStart(_) => {
                        bridge.attach();
                        if let Some(session_id) = session() {
                            schedulers.lock().unwrap_or_else(PoisonError::into_inner).retain(|_, scheduler| !scheduler.admission_is_stopped());
                            tokio::task::spawn_blocking(move || recovery.resume_paused_runs(&session_id)).await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                        }
                        surfaces.reconcile_activity(&manager, session().as_deref(), &tasks, &bridge).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?; surfaces.status.sync_now();
                    },
                    maho_ext_api::ExtensionEvent::SessionBeforeSwitch { .. } => { bridge.detach(); surfaces.clear_activity(); surfaces.status.dispose(); },
                    maho_ext_api::ExtensionEvent::SessionShutdown(_) => {
                        if let Some(session) = session() { recovery.try_pause_runs_for_shutdown(&session).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?; }
                        store.set_checkpoint_listener(None);
                        bridge.dispose(); surfaces.clear_activity(); surfaces.status.dispose(); surfaces.wake.emit_shutdown();
                    },
                    _ => {}
                }
                Ok(maho_ext_api::EventResult::None)
            }) }));
        }
        bridge.attach();
        if let Err(error) = surfaces.reconcile_activity(&self.manager, session().as_deref(), &lifecycle_tasks, &bridge) { eprintln!("DAG activity reconciliation failed: {error}"); }
        surfaces.status.sync_now();
    }
    pub fn register_tool(self: &Arc<Self>, api: &mut maho_ext_api::ExtensionApi, component: Arc<crate::component::TaskComponent>) {
        let runtime = component.engine.runtime.clone();
        let parent_session_id: Arc<dyn Fn() -> String + Send + Sync> = Arc::new(move || runtime.lock().unwrap_or_else(PoisonError::into_inner).session_id().unwrap_or_default().to_owned());
        let wait_dag = self.clone(); let wait_component = component.clone();
        let wait = Arc::new(move |run: &str, session: &str| {
            let scheduler = wait_dag.scheduler(&wait_component.engine, run, session).map_err(|error| maho_ext_api::ToolError::Message(error.to_string()))?;
            scheduler.when_idle();
            serde_json::to_value(scheduler.snapshot()).map_err(maho_ext_api::ToolError::from)
        });
        let cancel_dag = self.clone();
        let cancel = Arc::new(move |run: &str, reason: Option<&str>| {
            let scheduler = cancel_dag.schedulers.lock().unwrap_or_else(PoisonError::into_inner).get(run).cloned().ok_or_else(|| maho_ext_api::ToolError::Message("DAG scheduler not attached".into()))?;
            scheduler.cancel(&run.to_owned(), reason).map_err(maho_ext_api::ToolError::Message)
        });
        let mut tool = crate::dag_tool::create_dag_tool(crate::dag_tool::DagToolDeps { manager:self.manager.clone(), parent_session_id:parent_session_id.clone(), root_session_id:parent_session_id, wait:Some(wait), cancel:Some(cancel) });
        let execute = tool.execute.clone(); let dag = self.clone();
        tool.execute = Arc::new(move |call| {
            let start = call.params["action"] == "start";
            let signal = call.signal.clone();
            let future = execute(call); let dag = dag.clone(); let component = component.clone();
            Box::pin(async move {
                let result = future.await?;
                if start && let Some(run) = result.details.as_ref().and_then(|details| details["run_id"].as_str()) {
                    let session = component.engine.runtime.lock().unwrap_or_else(PoisonError::into_inner).session_id().map(str::to_owned);
                    if let Some(session) = session {
                        let scheduler = dag.scheduler(&component.engine, run, &session).map_err(|error| maho_ext_api::ToolError::Message(error.to_string()))?;
                        if let Some(bridge) = dag.rpc.lock().unwrap_or_else(PoisonError::into_inner).clone() { bridge.sync(); }
                        if let Some(surfaces) = dag.surfaces.lock().unwrap_or_else(PoisonError::into_inner).clone() { surfaces.status.sync_now(); surfaces.wake.publish_live(); }
                        let cancellation = scheduler.clone();
                        let run_id = run.to_owned();
                        let (sender, receiver) = tokio::sync::oneshot::channel();
                        let worker = std::thread::spawn(move || { let _ = sender.send(scheduler.run()); });
                        let record = crate::worker::settle(receiver, worker, &signal, || {
                            cancellation.cancel(&run_id, Some("tool aborted")).map_err(maho_ext_api::ToolError::Message)
                        }, "DAG scheduler panicked").await?;
                        let record = record.map_err(|error| maho_ext_api::ToolError::Message(error.to_string()))?;
                        component.sync();
                        if let Some(surfaces) = dag.surfaces.lock().unwrap_or_else(PoisonError::into_inner).clone() { surfaces.status.sync_now(); surfaces.wake.publish_live(); }
                        if let Some(bridge) = dag.rpc.lock().unwrap_or_else(PoisonError::into_inner).clone() { bridge.sync(); }
                        let snapshot = dag.manager.snapshot(&record.run_id, &session).map_err(|error| maho_ext_api::ToolError::Message(error.to_string()))?;
                        return Ok(crate::tools::tool_text(format!("Completed dag run {}.", record.run_id), serde_json::json!({"kind":"started","run_id":record.run_id,"snapshot":snapshot})));
                    }
                }
                Ok(result)
            })
        });
        api.register_tool(tool);
    }
}
