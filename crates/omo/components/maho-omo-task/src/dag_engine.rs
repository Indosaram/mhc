use std::{collections::BTreeMap, sync::{Arc, Mutex, PoisonError}};
use senpi_task::dag::{manager::{DagManager, DagManagerOptions, create_dag_manager}, scheduler::{DagSchedulerOptions, DagSchedulerContext, create_dag_scheduler}, store::DagFileStore};
use crate::engine::TaskEngine;
pub struct TaskDagEngine {
    pub manager: DagManager,
    pub store: Arc<DagFileStore>,
    schedulers: Arc<Mutex<BTreeMap<String, Arc<DagSchedulerContext>>>>,
    rpc: Mutex<Option<Arc<crate::dag_rpc_bridge::DagRpcBridge>>>,
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
        Ok(Self { manager:create_dag_manager(options), store, schedulers, rpc:Mutex::new(None) })
    }
    pub fn scheduler(&self, engine: &TaskEngine, run: &str, session: &str) -> Result<Arc<DagSchedulerContext>, senpi_task::dag::manager::DagManagerError> {
        let mut schedulers = self.schedulers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(scheduler) = schedulers.get(run) { return Ok(scheduler.clone()); }
        let initial_record = self.manager.record(&run.to_owned(), session)?;
        let scheduler = create_dag_scheduler(DagSchedulerOptions { store:self.store.clone(), task_manager:engine.manager.clone(), initial_record, execution_mode_agents:Some(Arc::new(engine.agents.clone())), execution_mode_config:engine.config["task"]["default_execution_mode"].as_str().and_then(senpi_task::manager::execution_mode::ExecutionMode::parse), ancestry_depth:None, subscriber_ring:None, now:None }).map_err(senpi_task::dag::manager::DagManagerError::from)?;
        schedulers.insert(run.into(), scheduler.clone()); Ok(scheduler)
    }
    pub fn register_rpc(self: &Arc<Self>, api: &mut maho_ext_api::ExtensionApi, component: &crate::component::TaskComponent) {
        let runtime = component.engine.runtime.clone();
        let session: Arc<dyn Fn() -> Option<String> + Send + Sync> = Arc::new(move || runtime.lock().unwrap_or_else(PoisonError::into_inner).session_id().map(str::to_owned));
        let live = Arc::downgrade(self); let live_session = session.clone();
        let snapshots = Arc::downgrade(self); let snapshot_session = session.clone();
        let events = api.events.clone();
        let bridge = crate::dag_rpc_bridge::create_dag_rpc_bridge(crate::dag_rpc_bridge_contract::DagRpcBridgeDeps {
            live_runs: Arc::new(move || {
                let (Some(dag), Some(session)) = (live.upgrade(), live_session()) else { return vec![]; };
                let runs = match dag.manager.list(&session, None) { Ok(runs) => runs, Err(error) => { eprintln!("DAG live run list failed: {error}"); return vec![]; } };
                runs.into_iter().map(|run| {
                    let store = dag.store.clone(); let id = run.run_id.clone();
                    crate::dag_rpc_bridge_contract::DagBridgeRun { run_id:run.run_id, status:run.status.as_str().into(), subscribe:Arc::new(move |listener| {
                        senpi_task::dag::journal::subscribe_dag_journal(&store, &id, Arc::new(move |event| match serde_json::to_value(event) { Ok(event) => listener(&event), Err(error) => eprintln!("DAG event serialization failed: {error}") }))
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
            parent_session_id: session,
            emit: Arc::new(move |name, data| events.emit("senpi:extension-rpc-event", &serde_json::json!({"name":name,"data":data}))),
            timers: Arc::new(crate::timers::HostTimers::default()), now: Arc::new(|| chrono::Utc::now().timestamp_millis()), heartbeat_ms:None, activity_coalesce_ms:None, snapshot_debounce_ms:None,
        });
        *self.rpc.lock().unwrap_or_else(PoisonError::into_inner) = Some(bridge.clone());
        for kind in [maho_ext_api::EventKind::SessionStart, maho_ext_api::EventKind::SessionBeforeSwitch, maho_ext_api::EventKind::SessionShutdown] {
            let bridge = bridge.clone();
            api.on(kind, Arc::new(move |event, _| { let bridge = bridge.clone(); Box::pin(async move {
                match event { maho_ext_api::ExtensionEvent::SessionStart(_) => bridge.attach(), maho_ext_api::ExtensionEvent::SessionBeforeSwitch { .. } => bridge.detach(), maho_ext_api::ExtensionEvent::SessionShutdown(_) => bridge.dispose(), _ => {} }
                Ok(maho_ext_api::EventResult::None)
            }) }));
        }
        bridge.attach();
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
                        let cancellation = scheduler.clone();
                        let run_id = run.to_owned();
                        let (sender, receiver) = tokio::sync::oneshot::channel();
                        let worker = std::thread::spawn(move || { let _ = sender.send(scheduler.run()); });
                        tokio::pin!(receiver);
                        let record = tokio::select! {
                            result = &mut receiver => result,
                            () = signal.cancelled() => {
                                cancellation.cancel(&run_id, Some("tool aborted")).map_err(maho_ext_api::ToolError::Message)?;
                                receiver.await
                            }
                        }.map_err(|error| maho_ext_api::ToolError::Message(error.to_string()))?;
                        worker.join().map_err(|_| maho_ext_api::ToolError::Message("DAG scheduler panicked".into()))?;
                        let record = record.map_err(|error| maho_ext_api::ToolError::Message(error.to_string()))?;
                        component.sync();
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
