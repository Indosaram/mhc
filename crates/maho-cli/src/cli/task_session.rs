use std::sync::Arc;
use senpi_task::{host::HostError, runners::in_process::child_handle::{ChildSession, ChildSessionListener}};

pub struct TeamManager(pub Arc<senpi_task::manager::TaskManager>);
impl senpi_task::team::runtime_types::TeamMemberReadPort for TeamManager {
    fn get(&self, task_id: &str) -> Option<senpi_task::team::runtime_types::TeamMemberTaskRecord> {
        let record = self.0.get(task_id)?;
        let execution_mode = senpi_task::manager::execution_mode::ExecutionMode::parse(&record.execution_mode)?;
        Some(senpi_task::team::runtime_types::TeamMemberTaskRecord {
            task_id: record.task_id, status: record.status, residency_state: record.residency_state,
            created_at: record.created_at, updated_at: record.updated_at, parent_session_id: record.parent_session_id,
            root_session_id: record.root_session_id, depth: record.depth, execution_mode, model: record.model,
            child_session_id: record.child_session_id, resolved_model: record.resolved_model,
            name: record.name, category: record.category, agent_type: record.agent_type,
        })
    }
}
impl senpi_task::team::runtime_types::TeamMemberCancelPort for TeamManager {
    fn cancel_task(&self, id: &str, reason: Option<&str>) -> senpi_task::team::runtime_types::TeamCancelOutcome {
        use senpi_task::{steering::CancelOutcome, team::runtime_types::TeamCancelOutcome};
        match self.0.cancel_task(id, reason, Default::default()) {
            Ok(CancelOutcome::Cancelled { task_id, previous_status }) => TeamCancelOutcome::Cancelled { task_id, previous_status },
            Ok(CancelOutcome::Noop { task_id, status, reason }) => TeamCancelOutcome::Noop { task_id, status, reason },
            Ok(CancelOutcome::Stale { task_id, status, reason, .. }) => TeamCancelOutcome::Noop { task_id, status, reason },
            Ok(CancelOutcome::NotFound { reason }) => TeamCancelOutcome::NotFound { reason },
            Err(error) => {
                maho_core::output_guard::maho_write_stderr(&format!("Team member cancellation failed: {id}: {error}\n"));
                TeamCancelOutcome::NotFound { reason: error.to_string() }
            }
        }
    }
}
impl senpi_task::team::runtime_types::TeamRuntimeManagerPort for TeamManager {
    fn start(&self, spec: &senpi_task::team::runtime_types::TeamMemberStartSpec) -> Result<senpi_task::team::runtime_types::TeamStartResult, String> {
        use senpi_task::{manager::types::{ManagerStartSpec, StartResult}, team::runtime_types::{TeamStartResult, TeamStartedMember}};
        Ok(match self.0.start(&ManagerStartSpec::from(spec)) {
            StartResult::Started(task) => TeamStartResult::Started(TeamStartedMember {
                name: task.name, task_id: task.task_id, status: task.status, resolved_model: task.resolved_model,
            }),
            StartResult::DepthDenied { reason, .. } => TeamStartResult::Rejected { kind: "depth_denied".into(), reason },
            StartResult::PlanUnresolved(error) => TeamStartResult::Rejected { kind: "plan_unresolved".into(), reason: format!("{error:?}") },
            StartResult::StartFailed(error) => TeamStartResult::Rejected { kind: "start_failed".into(), reason: error.error_message },
            StartResult::ResidencyDenied { reason } => TeamStartResult::Rejected { kind: "residency_denied".into(), reason },
        })
    }
    fn get_resident_handle(&self, task_id: &str) -> Option<senpi_task::team::member_projection::ResidentSessionRef> {
        self.0.get_resident_handle(task_id).map(|handle| senpi_task::team::member_projection::ResidentSessionRef { session_id: handle.session_id() })
    }
}

pub fn configured_team_bounds(config: &serde_json::Value) -> Result<senpi_task::team::runtime_config::TeamTaskBounds, maho_ext_api::ExtensionFailure> {
    let team = &config["task"]["team"];
    let value = |key: &str, default| match team.get(key) {
        None => Ok(default),
        Some(value) => value.as_u64().filter(|value| *value > 0).ok_or_else(|| maho_ext_api::ExtensionFailure::new(format!("task.team.{key} must be a positive integer"))),
    };
    let bounds = senpi_task::team::runtime_config::TeamTaskBounds {
        max_members: value("max_members", 8)?, max_parallel_members: value("max_parallel_members", 4)?,
        max_wall_clock_minutes: value("max_wall_clock_minutes", 120)?,
    };
    senpi_task::team::runtime_config::to_team_core_config(&bounds, "").map_err(maho_ext_api::ExtensionFailure::new)?;
    Ok(bounds)
}

pub fn mount_team_runtime(api: &mut maho_ext_api::ExtensionApi, component: &Arc<maho_omo_task::component::TaskComponent>, ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps, actions: Arc<dyn maho_ext_api::ExtensionActions>, coordinator: Arc<maho_omo::task_coordinator::TaskCoordinator>) -> Result<(), maho_ext_api::ExtensionFailure> {
    use maho_omo_task::{team_service::{create_team_service, TeamServiceDeps}, lead_poller_lifecycle::{create_lead_poller_lifecycle, LeadPollerLifecycleDeps, LeadMessageSink}, member_liveness::{create_team_member_liveness_notifier, TeamMemberLivenessDeps}};
    use senpi_task::tools::team::types::TeamToolsService;
    let runtime = component.engine.runtime.clone();
    let session: Arc<dyn Fn() -> Option<String> + Send + Sync> = Arc::new(move || runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner).session_id().map(str::to_owned));
    let service = Arc::new(create_team_service(TeamServiceDeps {
        manager: component.engine.manager.clone(), member_manager: Arc::new(TeamManager(component.engine.manager.clone())),
        destruction: Arc::new(maho_omo_task::lifecycle_adapters::TaskLifecycleDestruction((*component.engine.lifecycle).clone())),
        session_id: session.clone(), state_dir: ownership.state_dir.clone(), bounds: ownership.team_bounds,
        omo_config: component.engine.config.clone(), agent_names: component.engine.agents.keys().cloned().collect(),
        member_extension: senpi_task::team::runtime_types::TeamMemberExtensionConfig {
            entry_path: maho_omo_task::engine::MEMBER_EXTENSION_ENTRY.into(), inherited_extensions: Some(senpi_task::runners::rpc::parent_extensions::parse_extension_entries(&std::env::args().collect::<Vec<_>>())),
        },
        append_task_event: None, now: None, new_message_id: None,
    }).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?);
    let config = senpi_task::team::runtime_config::to_team_core_config(&ownership.team_bounds,
        &senpi_task::team::storage::team_storage_base_dir(&ownership.state_dir).to_string_lossy()).map_err(maho_ext_api::ExtensionFailure::new)?;
    let runtime = component.engine.runtime.clone();
    let file = Arc::new(move || runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner).session_file().map(ToOwned::to_owned));
    let runtime = component.engine.runtime.clone();
    let parent_state: Arc<dyn Fn() -> senpi_task::completion::ParentState + Send + Sync> = Arc::new(move || runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner).parent_state());
    let listing = service.clone(); let dirs = ownership.state_dir.clone(); let store = component.engine.store.clone();
    let timers = Arc::new(maho_omo_task::timers::HostTimers::default());
    let sink = Arc::new(LeadMessageSink { actions: actions.clone(), coordinator: Some(coordinator.clone()), parent_state: parent_state.clone(), on_error: Arc::new(|error| { maho_core::output_guard::maho_write_stderr(&format!("Team message delivery failed: {error}\n")); }) });
    let pollers = create_lead_poller_lifecycle(LeadPollerLifecycleDeps {
        list_teams: Arc::new(move || listing.list_teams().map_err(|error| error.to_string())), session_id: session, session_file: file,
        parent_state: parent_state.clone(), config, runtime_dir: Arc::new(move |id| senpi_task::team::storage::team_storage_base_dir(&dirs).join("runtime").join(id)),
        delivery_journal: Some(Arc::new(senpi_task::team::messaging::delivery_journal::create_lead_delivery_journal(Default::default()))), append_event: Arc::new(move |id, event| {
            if let Err(error) = store.append_event(id, &senpi_task::store::PersistedTaskEvent { event_type: event.event_type, payload: event.payload }) { maho_core::output_guard::maho_write_stderr(&format!("Team event persistence failed: {error}\n")); }
        }), sink, factory: None, timers: timers.clone(), on_error: Arc::new(|error| { maho_core::output_guard::maho_write_stderr(&format!("Team poll failed: {error}\n")); }),
    });
    let read_store = component.engine.store.clone(); let write_store = read_store.clone();
    let liveness = create_team_member_liveness_notifier(TeamMemberLivenessDeps {
        deliver: Arc::new(move |key, message, callbacks| {
            use maho_omo_task::parent_notifier::CompletionCoordinator;
            coordinator.enqueue_liveness(key, message, callbacks).map_err(|error| error.to_string())?;
            // Same edge policy as the lead sink: streaming parents coalesce on the batch window,
            // idle parents flush on the next macrotask.
            if parent_state() == senpi_task::completion::ParentState::Streaming { coordinator.schedule_flush(); }
            else { coordinator.flush_soon(); }
            Ok(())
        }),
        was_delivered: Arc::new(move |record| match read_store.load(&record.task_id) {
            Ok(Some(fresh)) => fresh.notification.liveness_notified_epoch.unwrap_or(-1) >= record.notification.run_epoch,
            Ok(None) => false, Err(error) => { maho_core::output_guard::maho_write_stderr(&format!("Team liveness read failed: {error}\n")); false },
        }),
        mark_delivered: Arc::new(move |record| {
            if let Err(error) = write_store.mutate(&record.task_id, |fresh| {
                let mut updated = fresh.clone();
                if fresh.status == record.status && fresh.notification.run_epoch == record.notification.run_epoch { updated.notification.liveness_notified_epoch = Some(record.notification.run_epoch); }
                updated
            }) { maho_core::output_guard::maho_write_stderr(&format!("Team liveness persistence failed: {error}\n")); }
        }), on_error: Arc::new(|error| { maho_core::output_guard::maho_write_stderr(&format!("Team liveness delivery failed: {error}\n")); }), timers, max_delivery_retries: None, max_persistence_retries: None,
    });
    component.register_team_runtime(api, service, pollers, liveness, ownership);
    Ok(())
}

pub struct NativeChild { pub session: Arc<maho_core::agent_session::AgentSession>, pub executor: tokio::runtime::Handle, workpool_events: Option<super::task_runners::WorkpoolEvents> }
impl NativeChild {
    pub fn new(session: Arc<maho_core::agent_session::AgentSession>, executor: tokio::runtime::Handle) -> Self { Self { session, executor, workpool_events: None } }
}

fn block_on_sync<F, T>(executor: tokio::runtime::Handle, future: F) -> Result<T, HostError>
where
    F: std::future::Future<Output = Result<T, String>> + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(move || executor.block_on(future))
        .join()
        .map_err(|_| HostError { message: "child session creation thread panicked".to_owned() })?
        .map_err(|message| HostError { message })
}

impl ChildSession for NativeChild {
    fn rebind_workpool_identity(&self, identity: senpi_task::runners::in_process::child_handle::WorkpoolChildIdentity) -> Option<Result<(), HostError>> {
        let identity = senpi_task::runners::in_process::child_options::WorkpoolChildIdentity { task_id: identity.task_id, state_dir: identity.state_dir, run_epoch: identity.run_epoch };
        Some(self.session.replace_custom_tool(super::task_runners::workpool_yield_tool_with_events(&identity, &self.session.cwd(), self.workpool_events.clone()))
            .map_err(|message| HostError { message }))
    }
    fn session_id(&self) -> String { self.session.session_id() }
    fn prompt(&self, text: &str) -> Result<(), HostError> {
        self.executor.block_on(async {
            self.session.prompt(text, Default::default()).await?;
            self.session.wait_for_idle().await;
            Ok::<(), String>(())
        }).map_err(|message| HostError { message })
    }
    fn steer(&self, text: &str) -> Result<(), HostError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            let session = self.session.clone();
            let text = text.to_owned();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| HostError { message: error.to_string() })?;
                runtime.block_on(session.steer(&text, None, Default::default())).map_err(|message| HostError { message })
            }).join().map_err(|_| HostError { message: "steer thread panicked".to_owned() })?
        } else {
            self.executor.block_on(self.session.steer(text, None, Default::default())).map_err(|message| HostError { message })
        }
    }
    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            let session = self.session.clone();
            let text = text.to_owned();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| HostError { message: error.to_string() })?;
                runtime.block_on(session.follow_up(&text, None, Default::default())).map_err(|message| HostError { message })
            }).join().map_err(|_| HostError { message: "follow_up thread panicked".to_owned() })?
        } else {
            self.executor.block_on(self.session.follow_up(text, None, Default::default())).map_err(|message| HostError { message })
        }
    }
    fn abort(&self) -> Result<(), HostError> { let session = self.session.clone(); self.executor.spawn(async move { session.abort().await; }); Ok(()) }
    fn get_last_assistant_text(&self) -> Option<String> { self.session.get_last_assistant_text() }
    fn subscribe(&self, listener: ChildSessionListener) -> senpi_task::manager::child_handle::Unsubscribe {
        let subscription = self.session.subscribe(Arc::new(move |event| { listener(&maho_rpc::session_binding::session_event_record(event).unwrap_or(serde_json::Value::Null)); }));
        let session = self.session.clone();
        Box::new(move || { drop(subscription); drop(session); })
    }
    fn dispose(&self) {
        let session = self.session.clone();
        if tokio::runtime::Handle::try_current().is_ok() {
            self.executor.spawn(async move {
                session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await;
                session.dispose().await;
            });
        } else {
            self.executor.block_on(async move {
                session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await;
                session.dispose().await;
            });
        }
    }
}

pub fn factory(executor: tokio::runtime::Handle, parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>) -> senpi_task::runners::in_process::runner::CreateChildSession {
    factory_with_workpool_events(executor, parent, None)
}

pub fn factory_with_workpool_events(executor: tokio::runtime::Handle, parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>, workpool_events: Option<super::task_runners::WorkpoolEvents>) -> senpi_task::runners::in_process::runner::CreateChildSession {
    Arc::new(move |options| {
        let model = options.model.as_ref().and_then(|model| model.downcast_ref::<maho_ai::model::Model>()).cloned();
        let registry = options.model_registry.as_ref().and_then(|registry| registry.downcast_ref::<super::task_runners::NativeChildModelRegistry>()).map(|registry| registry.0.clone());
        let models = options.model_runtime.as_ref().and_then(|runtime| runtime.downcast_ref::<maho_core::model_runtime::ModelRuntime>()).cloned();
        let manager = super::task_runners::native_child_session_manager(&options)?;
        let shell_options = maho_tools::bash::BashToolOptions { shell_env: Some(Arc::new(maho_core::utils::shell::get_shell_env)), ..Default::default() };
        let child_builtins = maho_tools::index::create_all_tool_definitions(std::path::Path::new(&options.cwd),
            maho_tools::index::ToolsOptions { bash: shell_options.clone(), powershell: shell_options, ..Default::default() });
        let custom = options.custom_tools.iter().map(|tool| {
            let name = tool.name().to_owned();
            if let Some(definition) = child_builtins.get(&name) { return Ok(definition.clone()); }
            let parent_definition = parent().and_then(|parent| parent.get_tool_definition(&name));
            let tool = tool.clone();
            let description = tool.description().to_owned();
            let Some(parameters) = tool.parameters().or_else(|| parent_definition.map(|tool| tool.parameters)) else {
                return Err(HostError { message: format!("Shared parent tool {name} has no native schema") });
            };
            Ok(maho_ext_api::ToolDefinition::new(&name, &description, parameters,
                Arc::new(move |call| { let tool = tool.clone(); let id = call.id.to_owned(); let params = call.params; let signal = call.signal; Box::pin(async move {
                    let controller = maho_ai::utils::abort::AbortController::new();
                    if signal.is_aborted() { controller.abort(None); }
                    let bridge_controller = controller.clone();
                    let cancellation = async move {
                        signal.cancelled().await;
                        bridge_controller.abort(None);
                        std::future::pending::<()>().await;
                    };
                    let result = tokio::select! {
                        biased;
                        _ = cancellation => unreachable!("cancellation bridge remains pending"),
                        result = tool.execute_async(&id, &params, Some(controller.signal())) => result,
                    }
                        .map_err(|error| maho_tools::ToolError::Message(error.message))?;
                    serde_json::from_value(result).map_err(|error| maho_tools::ToolError::Message(error.to_string()))
                }) })))
        }).collect::<Result<Vec<_>, HostError>>()?;
        let mut sdk_options = super::task_runners::native_child_sdk_options(&options, custom)?;
        if let Some(identity) = &options.workpool_identity {
            sdk_options.custom_tools.retain(|tool| tool.name != "workpool");
            sdk_options.custom_tools.push(super::task_runners::workpool_yield_tool_with_events(identity, &options.cwd, workpool_events.clone()));
        }
        sdk_options.model = model;
        sdk_options.model_runtime = models;
        sdk_options.model_registry = registry;
        sdk_options.session_manager = Some(manager);
        let create_session = maho_core::sdk::create_agent_session(sdk_options);
        let session = block_on_sync(executor.clone(), create_session)?;
        Ok(Arc::new(NativeChild { session: Arc::new(session.session), executor: executor.clone(), workpool_events: workpool_events.clone() }) as Arc<dyn ChildSession>)
    })
}

#[cfg(test)]
mod tests {
    use super::{block_on_sync, factory};
    use std::sync::Arc;

    #[test]
    fn synchronous_child_creation_can_run_inside_async_runtime() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let executor = runtime.handle().clone();

        runtime.block_on(async move {
            let value = block_on_sync(executor, async { Ok::<_, String>(42) }).expect("child creation");
            assert_eq!(value, 42);
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_child_shared_write_stays_in_child_cwd() {
        let root = tempfile::tempdir().expect("temporary workspace");
        let parent_cwd = root.path().join("parent");
        let child_cwd = root.path().join("child");
        std::fs::create_dir_all(&parent_cwd).expect("parent cwd");
        std::fs::create_dir_all(&child_cwd).expect("child cwd");
        use maho_ai::providers::faux::{faux_provider,faux_assistant_message,faux_tool_call,FauxAssistantMessageOptions};
        let provider = faux_provider(Default::default());
        provider.set_responses(vec![
            faux_assistant_message(faux_tool_call("write", serde_json::json!({"path":"child-only.txt","content":"child"}).as_object().expect("arguments").clone(), Some("child-write")), FauxAssistantMessageOptions {stop_reason:Some(maho_ai::types::StopReason::ToolUse),..Default::default()}).into(),
            faux_assistant_message("done", Default::default()).into(),
        ]);
        let model = provider.get_model(Some("faux-1")).expect("faux model");
        let mut model_runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(root.path().join("models.json")), providers: Some(Vec::new()), ..Default::default()
        });
        model_runtime.register_native_provider(provider.provider.clone());
        let registry = maho_core::model_registry::ModelRegistry::new(model_runtime.clone());
        registry.auth_storage.set("faux", Some(serde_json::json!({"type":"api_key","key":"child-fixture"}))).expect("faux auth");
        let parent = Arc::new(maho_core::sdk::create_agent_session(
            maho_core::sdk::CreateAgentSessionOptions {
                cwd: Some(parent_cwd.to_string_lossy().into_owned()), model: Some(model.clone()), model_runtime: Some(model_runtime.clone()),
                tools: Some(vec!["write".into()]), ..Default::default()
            },
        ).await.expect("parent session").session);
        let parent_accessor: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync> =
            Arc::new({ let parent = Arc::downgrade(&parent); move || parent.upgrade().map(|session| (*session).clone()) });
        let shared_write = super::super::task_runners::live_parent_tools(parent_accessor.clone(), tokio::runtime::Handle::current())()
            .into_iter().find(|tool| tool.name() == "write").expect("parent write tool");
        let child_cwd = child_cwd.to_string_lossy().into_owned();
        let options = senpi_task::runners::in_process::child_options::ChildSessionOptions {
            workpool_identity: None,
            cwd: child_cwd.clone(),
            session_manager: Arc::new(senpi_task::runners::in_process::session_manager::ChildSessionManager::create(
                &child_cwd, &child_cwd,
            ).expect("child session manager")),
            resource_loader: senpi_task::runners::in_process::child_options::ChildResourceLoader::Minimal,
            system_prompt: None, custom_tools: vec![shared_write], agent_dir: None,
            auth_storage: Some(registry.auth_storage.clone()), model_registry: Some(Arc::new(super::super::task_runners::NativeChildModelRegistry(registry))), model_runtime: Some(Arc::new(model_runtime)), model: Some(Arc::new(model)),
            thinking_level: None, settings: Default::default(), tools: Some(vec!["write".into()]), exclude_tools: None,
        };
        let child = factory(tokio::runtime::Handle::current(), parent_accessor)(options).expect("native child");
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = events.clone();
        let unsubscribe = child.subscribe(Arc::new(move |event| observed.lock().expect("child events").push(event.clone())));
        tokio::task::spawn_blocking(move || { child.prompt("write the child file").expect("child prompt"); child.dispose(); }).await.expect("child worker");
        unsubscribe();
        parent.dispose().await;

        assert_eq!(std::fs::read_to_string(child_cwd + "/child-only.txt").unwrap_or_else(|error| panic!("child output: {error}; events: {:?}", events.lock().expect("child events"))), "child");
        assert!(!parent_cwd.join("child-only.txt").exists());
    }

}
