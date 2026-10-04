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
            Ok(CancelOutcome::NotFound { reason }) => TeamCancelOutcome::NotFound { reason },
            Err(error) => {
                eprintln!("Team member cancellation failed: {id}: {error}");
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

pub fn mount_team_runtime(api: &mut maho_ext_api::ExtensionApi, component: &Arc<maho_omo_task::component::TaskComponent>, ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps, actions: Arc<dyn maho_ext_api::ExtensionActions>) -> Result<(), maho_ext_api::ExtensionFailure> {
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
            entry_path: "builtin:task".into(), inherited_extensions: Some(senpi_task::runners::rpc::parent_extensions::parse_extension_entries(&std::env::args().collect::<Vec<_>>())),
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
    let sink = Arc::new(LeadMessageSink { actions: actions.clone(), coordinator: None, parent_state: parent_state.clone(), on_error: Arc::new(|error| eprintln!("Team message delivery failed: {error}")) });
    let pollers = create_lead_poller_lifecycle(LeadPollerLifecycleDeps {
        list_teams: Arc::new(move || listing.list_teams().map_err(|error| error.to_string())), session_id: session, session_file: file,
        parent_state, config, runtime_dir: Arc::new(move |id| senpi_task::team::storage::team_storage_base_dir(&dirs).join("runtime").join(id)),
        delivery_journal: Some(Arc::new(senpi_task::team::messaging::delivery_journal::create_lead_delivery_journal(Default::default()))), append_event: Arc::new(move |id, event| {
            if let Err(error) = store.append_event(id, &senpi_task::store::PersistedTaskEvent { event_type: event.event_type, payload: event.payload }) { eprintln!("Team event persistence failed: {error}"); }
        }), sink, factory: None, timers: timers.clone(), on_error: Arc::new(|error| eprintln!("Team poll failed: {error}")),
    });
    let read_store = component.engine.store.clone(); let write_store = read_store.clone();
    let liveness = create_team_member_liveness_notifier(TeamMemberLivenessDeps {
        deliver: Arc::new(move |_, message| actions.send_message(message, maho_ext_api::SendMessageOptions { trigger_turn: true, deliver_as: Some(maho_ext_api::DeliverAs::Steer) }).map_err(|error| error.to_string())),
        was_delivered: Arc::new(move |record| match read_store.load(&record.task_id) {
            Ok(Some(fresh)) => fresh.notification.liveness_notified_epoch.unwrap_or(-1) >= record.notification.run_epoch,
            Ok(None) => false, Err(error) => { eprintln!("Team liveness read failed: {error}"); false },
        }),
        mark_delivered: Arc::new(move |record| {
            if let Err(error) = write_store.mutate(&record.task_id, |fresh| {
                let mut updated = fresh.clone();
                if fresh.status == record.status && fresh.notification.run_epoch == record.notification.run_epoch { updated.notification.liveness_notified_epoch = Some(record.notification.run_epoch); }
                updated
            }) { eprintln!("Team liveness persistence failed: {error}"); }
        }), on_error: Arc::new(|error| eprintln!("Team liveness delivery failed: {error}")), timers, max_delivery_retries: None, max_persistence_retries: None,
    });
    component.register_team_runtime(api, service, pollers, liveness, ownership);
    Ok(())
}

struct NativeChild { session: Arc<maho_core::agent_session::AgentSession>, executor: tokio::runtime::Handle }
impl ChildSession for NativeChild {
    fn session_id(&self) -> String { self.session.session_id() }
    fn prompt(&self, text: &str) -> Result<(), HostError> {
        self.executor.block_on(async {
            self.session.prompt(text, Default::default()).await?;
            self.session.wait_for_idle().await;
            Ok::<(), String>(())
        }).map_err(|message| HostError { message })
    }
    fn steer(&self, text: &str) -> Result<(), HostError> { self.executor.block_on(self.session.steer(text, None, Default::default())).map_err(|message| HostError { message }) }
    fn follow_up(&self, text: &str) -> Result<(), HostError> { self.executor.block_on(self.session.follow_up(text, None, Default::default())).map_err(|message| HostError { message }) }
    fn abort(&self) { let session = self.session.clone(); self.executor.spawn(async move { session.abort().await; }); }
    fn subscribe(&self, listener: ChildSessionListener) -> senpi_task::manager::child_handle::Unsubscribe {
        let subscription = self.session.subscribe(Arc::new(move |event| { listener(&serde_json::to_value(event).expect("agent event serialization")); }));
        let session = self.session.clone();
        Box::new(move || { drop(subscription); drop(session); })
    }
    fn dispose(&self) { let session = self.session.clone(); self.executor.block_on(async move { session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await; session.dispose().await; }); }
}

pub fn factory(executor: tokio::runtime::Handle, parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>) -> senpi_task::runners::in_process::runner::CreateChildSession {
    Arc::new(move |options| {
        let model = options.model.as_ref().and_then(|model| model.downcast_ref::<maho_ai::model::Model>()).cloned();
        let registry = options.model_registry.as_ref().and_then(|registry| registry.downcast_ref::<super::task_runners::NativeChildModelRegistry>()).map(|registry| registry.0.clone());
        let models = options.model_runtime.as_ref().and_then(|runtime| runtime.downcast_ref::<maho_core::model_runtime::ModelRuntime>()).cloned();
        let manager = super::task_runners::native_child_session_manager(&options);
        let custom = options.custom_tools.iter().map(|tool| {
            let tool = tool.clone();
            let name = tool.name().to_owned(); let description = tool.description().to_owned();
            let parameters = parent().and_then(|parent| parent.get_tool_definition(&name)).map(|tool| tool.parameters)
                .ok_or_else(|| HostError { message: format!("Shared parent tool {name} has no native schema") })?;
            Ok(maho_ext_api::ToolDefinition::new(&name, &description, parameters,
                Arc::new(move |call| { let tool = tool.clone(); let id = call.id.to_owned(); let params = call.params; Box::pin(async move {
                    let result = tokio::task::spawn_blocking(move || tool.execute(&id, &params)).await
                        .map_err(|error| maho_tools::ToolError::Message(error.to_string()))?
                        .map_err(|error| maho_tools::ToolError::Message(error.message))?;
                    serde_json::from_value(result).map_err(|error| maho_tools::ToolError::Message(error.to_string()))
                }) })))
        }).collect::<Result<Vec<_>, HostError>>()?;
        let settings = super::task_runners::native_child_settings(&options.settings);
        let session = executor.block_on(maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
            cwd: Some(options.cwd), model, model_runtime: models, model_registry: registry,
            agent_dir: options.agent_dir,
            auth_storage: options.auth_storage.and_then(|auth| auth.downcast::<maho_core::auth_storage::AuthStorage>().ok()),
            session_manager: Some(manager), settings_manager: Some(settings), tools: options.tools, exclude_tools: options.exclude_tools,
            custom_tools: custom, minimal_resources: true,
            thinking_level: options.thinking_level.as_deref().and_then(maho_ai::types::ThinkingLevel::parse),
            ..Default::default()
        })).map_err(|message| HostError { message })?;
        Ok(Arc::new(NativeChild { session: Arc::new(session.session), executor: executor.clone() }) as Arc<dyn ChildSession>)
    })
}
