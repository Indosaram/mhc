use senpi_task::{
    host::HostError,
    runners::in_process::child_handle::{ChildSession, ChildSessionListener},
};
use std::sync::Arc;

pub struct TeamManager(pub Arc<senpi_task::manager::TaskManager>);
impl senpi_task::team::runtime_types::TeamMemberReadPort for TeamManager {
    fn get(&self, task_id: &str) -> Option<senpi_task::team::runtime_types::TeamMemberTaskRecord> {
        let record = self.0.get(task_id)?;
        let execution_mode =
            senpi_task::manager::execution_mode::ExecutionMode::parse(&record.execution_mode)?;
        Some(senpi_task::team::runtime_types::TeamMemberTaskRecord {
            task_id: record.task_id,
            status: record.status,
            residency_state: record.residency_state,
            created_at: record.created_at,
            updated_at: record.updated_at,
            parent_session_id: record.parent_session_id,
            root_session_id: record.root_session_id,
            depth: record.depth,
            execution_mode,
            model: record.model,
            child_session_id: record.child_session_id,
            resolved_model: record.resolved_model,
            name: record.name,
            category: record.category,
            agent_type: record.agent_type,
        })
    }
}
impl senpi_task::team::runtime_types::TeamMemberCancelPort for TeamManager {
    fn cancel_task(
        &self,
        id: &str,
        reason: Option<&str>,
    ) -> senpi_task::team::runtime_types::TeamCancelOutcome {
        use senpi_task::{steering::CancelOutcome, team::runtime_types::TeamCancelOutcome};
        match self.0.cancel_task(id, reason, Default::default()) {
            Ok(CancelOutcome::Cancelled {
                task_id,
                previous_status,
            }) => TeamCancelOutcome::Cancelled {
                task_id,
                previous_status,
            },
            Ok(CancelOutcome::Noop {
                task_id,
                status,
                reason,
            }) => TeamCancelOutcome::Noop {
                task_id,
                status,
                reason,
            },
            Ok(CancelOutcome::Stale {
                task_id,
                status,
                reason,
                ..
            }) => TeamCancelOutcome::Noop {
                task_id,
                status,
                reason,
            },
            Ok(CancelOutcome::CancelPending {
                task_id,
                previous_status,
                reason,
                ..
            }) => TeamCancelOutcome::Noop {
                task_id,
                status: previous_status,
                reason,
            },
            Ok(CancelOutcome::NotFound { reason }) => TeamCancelOutcome::NotFound { reason },
            Err(error) => {
                maho_core::output_guard::maho_write_stderr(&format!(
                    "Team member cancellation failed: {id}: {error}\n"
                ));
                TeamCancelOutcome::NotFound {
                    reason: error.to_string(),
                }
            }
        }
    }
}
impl senpi_task::team::runtime_types::TeamRuntimeManagerPort for TeamManager {
    fn start(
        &self,
        spec: &senpi_task::team::runtime_types::TeamMemberStartSpec,
    ) -> Result<senpi_task::team::runtime_types::TeamStartResult, String> {
        use senpi_task::{
            manager::types::{ManagerStartSpec, StartResult},
            team::runtime_types::{TeamStartResult, TeamStartedMember},
        };
        Ok(match self.0.start(&ManagerStartSpec::from(spec)) {
            StartResult::Started(task) => TeamStartResult::Started(TeamStartedMember {
                name: task.name,
                task_id: task.task_id,
                status: task.status,
                resolved_model: task.resolved_model,
            }),
            StartResult::Held(_) => TeamStartResult::Rejected {
                kind: "held".into(),
                reason: "Task admission is held until the run resumes.".into(),
            },
            StartResult::DepthDenied { reason, .. } => TeamStartResult::Rejected {
                kind: "depth_denied".into(),
                reason,
            },
            StartResult::PlanUnresolved(error) => TeamStartResult::Rejected {
                kind: "plan_unresolved".into(),
                reason: format!("{error:?}"),
            },
            StartResult::StartFailed(error) => TeamStartResult::Rejected {
                kind: "start_failed".into(),
                reason: error.error_message,
            },
            StartResult::ResidencyDenied { reason, .. } => TeamStartResult::Rejected {
                kind: "residency_denied".into(),
                reason,
            },
        })
    }
    fn get_resident_handle(
        &self,
        task_id: &str,
    ) -> Option<senpi_task::team::member_projection::ResidentSessionRef> {
        self.0.get_resident_handle(task_id).map(|handle| {
            senpi_task::team::member_projection::ResidentSessionRef {
                session_id: handle.session_id(),
            }
        })
    }
}

pub fn configured_team_bounds(
    config: &serde_json::Value,
) -> Result<senpi_task::team::runtime_config::TeamTaskBounds, maho_ext_api::ExtensionFailure> {
    let team = &config["task"]["team"];
    let value = |key: &str, default| match team.get(key) {
        None => Ok(default),
        Some(value) => value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
            maho_ext_api::ExtensionFailure::new(format!(
                "task.team.{key} must be a positive integer"
            ))
        }),
    };
    let bounds = senpi_task::team::runtime_config::TeamTaskBounds {
        max_members: value("max_members", 8)?,
        max_parallel_members: value("max_parallel_members", 4)?,
        max_wall_clock_minutes: value("max_wall_clock_minutes", 120)?,
    };
    senpi_task::team::runtime_config::to_team_core_config(&bounds, "")
        .map_err(maho_ext_api::ExtensionFailure::new)?;
    Ok(bounds)
}

pub fn mount_team_runtime(
    api: &mut maho_ext_api::ExtensionApi,
    component: &Arc<maho_omo_task::component::TaskComponent>,
    ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps,
    actions: Arc<dyn maho_ext_api::ExtensionActions>,
    coordinator: Arc<maho_omo::task_coordinator::TaskCoordinator>,
) -> Result<(), maho_ext_api::ExtensionFailure> {
    use maho_omo_task::{
        lead_poller_lifecycle::{
            LeadMessageSink, LeadPollerLifecycleDeps, create_lead_poller_lifecycle,
        },
        member_liveness::{TeamMemberLivenessDeps, create_team_member_liveness_notifier},
        team_service::{TeamServiceDeps, create_team_service},
    };
    use senpi_task::tools::team::types::TeamToolsService;
    let runtime = component.engine.runtime.clone();
    let session: Arc<dyn Fn() -> Option<String> + Send + Sync> = Arc::new(move || {
        runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session_id()
            .map(str::to_owned)
    });
    let service = Arc::new(
        create_team_service(TeamServiceDeps {
            manager: component.engine.manager.clone(),
            member_manager: Arc::new(TeamManager(component.engine.manager.clone())),
            destruction: Arc::new(maho_omo_task::lifecycle_adapters::TaskLifecycleDestruction(
                (*component.engine.lifecycle).clone(),
            )),
            session_id: session.clone(),
            state_dir: ownership.state_dir.clone(),
            bounds: ownership.team_bounds,
            omo_config: component.engine.config.clone(),
            agent_names: component.engine.agents.keys().cloned().collect(),
            member_extension: senpi_task::team::runtime_types::TeamMemberExtensionConfig {
                entry_path: maho_omo_task::engine::MEMBER_EXTENSION_ENTRY.into(),
                inherited_extensions: Some(
                    senpi_task::runners::rpc::parent_extensions::parse_extension_entries(
                        &std::env::args().collect::<Vec<_>>(),
                    ),
                ),
            },
            append_task_event: None,
            now: None,
            new_message_id: None,
        })
        .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?,
    );
    let config = senpi_task::team::runtime_config::to_team_core_config(
        &ownership.team_bounds,
        &senpi_task::team::storage::team_storage_base_dir(&ownership.state_dir).to_string_lossy(),
    )
    .map_err(maho_ext_api::ExtensionFailure::new)?;
    let runtime = component.engine.runtime.clone();
    let file = Arc::new(move || {
        runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session_file()
            .map(ToOwned::to_owned)
    });
    let runtime = component.engine.runtime.clone();
    let parent_state: Arc<dyn Fn() -> senpi_task::completion::ParentState + Send + Sync> =
        Arc::new(move || {
            runtime
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .parent_state()
        });
    let listing = service.clone();
    let dirs = ownership.state_dir.clone();
    let store = component.engine.store.clone();
    let timers = Arc::new(maho_omo_task::timers::HostTimers::default());
    let sink = Arc::new(LeadMessageSink {
        actions: actions.clone(),
        coordinator: Some(coordinator.clone()),
        parent_state: parent_state.clone(),
        on_error: Arc::new(|error| {
            maho_core::output_guard::maho_write_stderr(&format!(
                "Team message delivery failed: {error}\n"
            ));
        }),
    });
    let pollers = create_lead_poller_lifecycle(LeadPollerLifecycleDeps {
        list_teams: Arc::new(move || listing.list_teams().map_err(|error| error.to_string())),
        session_id: session,
        session_file: file,
        parent_state: parent_state.clone(),
        config,
        runtime_dir: Arc::new(move |id| {
            senpi_task::team::storage::team_storage_base_dir(&dirs)
                .join("runtime")
                .join(id)
        }),
        delivery_journal: Some(Arc::new(
            senpi_task::team::messaging::delivery_journal::create_lead_delivery_journal(
                Default::default(),
            ),
        )),
        append_event: Arc::new(move |id, event| {
            if let Err(error) = store.append_event(
                id,
                &senpi_task::store::PersistedTaskEvent {
                    event_type: event.event_type,
                    payload: event.payload,
                },
            ) {
                maho_core::output_guard::maho_write_stderr(&format!(
                    "Team event persistence failed: {error}\n"
                ));
            }
        }),
        sink,
        factory: None,
        timers: timers.clone(),
        on_error: Arc::new(|error| {
            maho_core::output_guard::maho_write_stderr(&format!("Team poll failed: {error}\n"));
        }),
    });
    let read_store = component.engine.store.clone();
    let write_store = read_store.clone();
    let liveness = create_team_member_liveness_notifier(TeamMemberLivenessDeps {
        deliver: Arc::new(move |key, message, callbacks| {
            use maho_omo_task::parent_notifier::CompletionCoordinator;
            coordinator
                .enqueue_liveness(key, message, callbacks)
                .map_err(|error| error.to_string())?;
            // Same edge policy as the lead sink: streaming parents coalesce on the batch window,
            // idle parents flush on the next macrotask.
            if parent_state() == senpi_task::completion::ParentState::Streaming {
                coordinator.schedule_flush();
            } else {
                coordinator.flush_soon();
            }
            Ok(())
        }),
        was_delivered: Arc::new(move |record| match read_store.load(&record.task_id) {
            Ok(Some(fresh)) => {
                fresh.notification.liveness_notified_epoch.unwrap_or(-1)
                    >= record.notification.run_epoch
            }
            Ok(None) => false,
            Err(error) => {
                maho_core::output_guard::maho_write_stderr(&format!(
                    "Team liveness read failed: {error}\n"
                ));
                false
            }
        }),
        mark_delivered: Arc::new(move |record| {
            if let Err(error) = write_store.mutate(&record.task_id, |fresh| {
                let mut updated = fresh.clone();
                if fresh.status == record.status
                    && fresh.notification.run_epoch == record.notification.run_epoch
                {
                    updated.notification.liveness_notified_epoch =
                        Some(record.notification.run_epoch);
                }
                updated
            }) {
                maho_core::output_guard::maho_write_stderr(&format!(
                    "Team liveness persistence failed: {error}\n"
                ));
            }
        }),
        on_error: Arc::new(|error| {
            maho_core::output_guard::maho_write_stderr(&format!(
                "Team liveness delivery failed: {error}\n"
            ));
        }),
        timers,
        max_delivery_retries: None,
        max_persistence_retries: None,
    });
    component.register_team_runtime(api, service, pollers, liveness, ownership);
    Ok(())
}

type ReopenChildSession =
    Arc<dyn Fn() -> Result<Arc<maho_core::agent_session::AgentSession>, HostError> + Send + Sync>;

struct NativeChildState {
    session: Option<Arc<maho_core::agent_session::AgentSession>>,
    subscription: Option<maho_core::agent_session::AgentSessionSubscription>,
    last_assistant_text: Option<String>,
    workpool_identity:
        Option<senpi_task::runners::in_process::child_options::WorkpoolChildIdentity>,
    disposed: bool,
}

pub struct NativeChild {
    session_id: String,
    cwd: String,
    state: std::sync::Mutex<NativeChildState>,
    listeners: Arc<std::sync::Mutex<std::collections::BTreeMap<u64, ChildSessionListener>>>,
    next_listener_id: std::sync::atomic::AtomicU64,
    reopen: Option<ReopenChildSession>,
    run_control: std::sync::Mutex<()>,
    pub executor: tokio::runtime::Handle,
    workpool_events: Option<super::task_runners::WorkpoolEvents>,
}

fn attach_child_forwarding(
    session: &maho_core::agent_session::AgentSession,
    listeners: &Arc<std::sync::Mutex<std::collections::BTreeMap<u64, ChildSessionListener>>>,
) -> maho_core::agent_session::AgentSessionSubscription {
    let listeners = Arc::clone(listeners);
    session.subscribe(Arc::new(move |event| {
        let record = maho_rpc::session_binding::session_event_record(event)
            .unwrap_or(serde_json::Value::Null);
        let active: Vec<ChildSessionListener> = listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for listener in active {
            listener(&record);
        }
    }))
}

impl NativeChild {
    pub fn new(
        session: Arc<maho_core::agent_session::AgentSession>,
        executor: tokio::runtime::Handle,
    ) -> Self {
        Self::with_reopen(session, executor, None, None)
    }

    fn with_reopen(
        session: Arc<maho_core::agent_session::AgentSession>,
        executor: tokio::runtime::Handle,
        workpool_events: Option<super::task_runners::WorkpoolEvents>,
        reopen: Option<ReopenChildSession>,
    ) -> Self {
        let listeners = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
        let subscription = attach_child_forwarding(&session, &listeners);
        let session_id = session.session_id();
        let cwd = session.cwd();
        let last_assistant_text = session.get_last_assistant_text();
        Self {
            session_id,
            cwd,
            state: std::sync::Mutex::new(NativeChildState {
                session: Some(session),
                subscription: Some(subscription),
                last_assistant_text,
                workpool_identity: None,
                disposed: false,
            }),
            listeners,
            next_listener_id: std::sync::atomic::AtomicU64::new(0),
            reopen,
            run_control: std::sync::Mutex::new(()),
            executor,
            workpool_events,
        }
    }

    fn ensure_session(&self) -> Result<Arc<maho_core::agent_session::AgentSession>, HostError> {
        {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.disposed {
                return Err(HostError {
                    message: "child session is disposed".to_owned(),
                });
            }
            if let Some(session) = &state.session {
                return Ok(Arc::clone(session));
            }
        }
        let reopen = self.reopen.as_ref().ok_or_else(|| HostError {
            message: "child session cannot be reopened".to_owned(),
        })?;
        let session = reopen()?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.disposed {
            return Err(HostError {
                message: "child session is disposed".to_owned(),
            });
        }
        if let Some(existing) = &state.session {
            return Ok(Arc::clone(existing));
        }
        if let Some(identity) = &state.workpool_identity {
            session
                .replace_custom_tool(super::task_runners::workpool_yield_tool_with_events(
                    identity,
                    &self.cwd,
                    self.workpool_events.clone(),
                ))
                .map_err(|message| HostError { message })?;
        }
        let subscription = attach_child_forwarding(&session, &self.listeners);
        state.subscription = Some(subscription);
        state.session = Some(Arc::clone(&session));
        Ok(session)
    }
}

fn block_on_sync<F, T>(executor: tokio::runtime::Handle, future: F) -> Result<T, HostError>
where
    F: std::future::Future<Output = Result<T, String>> + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(move || executor.block_on(future))
        .join()
        .map_err(|_| HostError {
            message: "child session creation thread panicked".to_owned(),
        })?
        .map_err(|message| HostError { message })
}

impl ChildSession for NativeChild {
    fn priority_pairing_registry(
        &self,
    ) -> Option<&dyn senpi_task::runners::pinned_model_equivalence::PriorityPairingRegistry> {
        Some(self)
    }
    fn model(&self) -> Option<senpi_task::runners::pinned_model_equivalence::ModelIdentity> {
        let session = self.ensure_session().ok()?;
        let model = session.model();
        Some(
            senpi_task::runners::pinned_model_equivalence::ModelIdentity::new(
                model.provider,
                model.id,
            ),
        )
    }
    fn run_pause(&self) -> Option<&dyn senpi_task::pause::native_child_pause::NativeRunPause> {
        Some(self)
    }

    fn rebind_workpool_identity(
        &self,
        identity: senpi_task::runners::in_process::child_handle::WorkpoolChildIdentity,
    ) -> Option<Result<(), HostError>> {
        let identity = senpi_task::runners::in_process::child_options::WorkpoolChildIdentity {
            task_id: identity.task_id,
            state_dir: identity.state_dir,
            run_epoch: identity.run_epoch,
        };
        let session = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.workpool_identity = Some(identity.clone());
            state.session.clone()
        };
        let Some(session) = session else {
            return Some(Ok(()));
        };
        Some(
            session
                .replace_custom_tool(super::task_runners::workpool_yield_tool_with_events(
                    &identity,
                    &self.cwd,
                    self.workpool_events.clone(),
                ))
                .map_err(|message| HostError { message }),
        )
    }
    fn session_id(&self) -> String {
        self.session_id.clone()
    }
    fn prompt(&self, text: &str) -> Result<(), HostError> {
        let session = self.ensure_session()?;
        let result = self
            .executor
            .block_on(async {
                session.prompt(text, Default::default()).await?;
                session.wait_for_idle().await;
                Ok::<(), String>(())
            })
            .map_err(|message| HostError { message });
        if let Some(last) = session.get_last_assistant_text() {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .last_assistant_text = Some(last);
        }
        result
    }
    fn steer(&self, text: &str) -> Result<(), HostError> {
        let session = self.ensure_session()?;
        if tokio::runtime::Handle::try_current().is_ok() {
            let text = text.to_owned();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| HostError {
                        message: error.to_string(),
                    })?;
                runtime
                    .block_on(session.steer(&text, None, Default::default()))
                    .map_err(|message| HostError { message })
            })
            .join()
            .map_err(|_| HostError {
                message: "steer thread panicked".to_owned(),
            })?
        } else {
            self.executor
                .block_on(session.steer(text, None, Default::default()))
                .map_err(|message| HostError { message })
        }
    }
    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        let session = self.ensure_session()?;
        if tokio::runtime::Handle::try_current().is_ok() {
            let text = text.to_owned();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| HostError {
                        message: error.to_string(),
                    })?;
                runtime
                    .block_on(session.follow_up(&text, None, Default::default()))
                    .map_err(|message| HostError { message })
            })
            .join()
            .map_err(|_| HostError {
                message: "follow_up thread panicked".to_owned(),
            })?
        } else {
            self.executor
                .block_on(session.follow_up(text, None, Default::default()))
                .map_err(|message| HostError { message })
        }
    }
    fn abort(&self) -> Result<(), HostError> {
        let session = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session
            .clone();
        if let Some(session) = session {
            self.executor.spawn(async move {
                session.abort().await;
            });
        }
        Ok(())
    }
    fn get_last_assistant_text(&self) -> Option<String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(session) = &state.session {
            let session = Arc::clone(session);
            let cached = state.last_assistant_text.clone();
            drop(state);
            return session.get_last_assistant_text().or(cached);
        }
        state.last_assistant_text.clone()
    }
    fn park_idle_memory(&self) {
        if self.reopen.is_none() {
            let session = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .session
                .clone();
            if let Some(session) = session {
                session.park_idle_child_memory();
            }
            return;
        }
        let (session, subscription) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.disposed {
                return;
            }
            state.last_assistant_text = None;
            (state.session.take(), state.subscription.take())
        };
        drop(subscription);
        let Some(session) = session else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            self.executor.spawn(async move {
                session.dispose().await;
                drop(session);
                maho_tool_child::allocator::reclaim_unused_heap();
            });
        } else {
            self.executor.block_on(async move {
                session.dispose().await;
            });
            maho_tool_child::allocator::reclaim_unused_heap();
        }
    }
    fn subscribe(
        &self,
        listener: ChildSessionListener,
    ) -> senpi_task::manager::child_handle::Unsubscribe {
        let id = self
            .next_listener_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        self.listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, listener);
        let listeners = Arc::clone(&self.listeners);
        Box::new(move || {
            listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&id);
        })
    }
    fn dispose(&self) {
        let (session, subscription) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.disposed = true;
            (state.session.take(), state.subscription.take())
        };
        drop(subscription);
        self.listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        let Some(session) = session else {
            maho_tool_child::allocator::reclaim_unused_heap();
            return;
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            self.executor.spawn(async move {
                session
                    .emit_session_shutdown(maho_ext_api::SessionReason::Quit)
                    .await;
                session.dispose().await;
            });
        } else {
            self.executor.block_on(async move {
                session
                    .emit_session_shutdown(maho_ext_api::SessionReason::Quit)
                    .await;
                session.dispose().await;
            });
        }
    }
}

pub fn factory(
    executor: tokio::runtime::Handle,
    parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
) -> senpi_task::runners::in_process::runner::CreateChildSession {
    factory_with_workpool_events(executor, parent, None)
}

fn create_native_child_session(
    options: &senpi_task::runners::in_process::child_options::ChildSessionOptions,
    executor: &tokio::runtime::Handle,
    parent: &Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
    workpool_events: &Option<super::task_runners::WorkpoolEvents>,
) -> Result<Arc<maho_core::agent_session::AgentSession>, HostError> {
    let model = options
        .model
        .as_ref()
        .and_then(|model| model.downcast_ref::<maho_ai::model::Model>())
        .cloned();
    let registry = options
        .model_registry
        .as_ref()
        .and_then(|registry| {
            registry.downcast_ref::<super::task_runners::NativeChildModelRegistry>()
        })
        .map(|registry| registry.0.clone());
    let models = options
        .model_runtime
        .as_ref()
        .and_then(|runtime| runtime.downcast_ref::<maho_core::model_runtime::ModelRuntime>())
        .cloned();
    let mut child_builtins = None;
    let custom = options
        .custom_tools
        .iter()
        .map(|tool| {
            let name = tool.name().to_owned();
            let builtins = child_builtins.get_or_insert_with(|| {
                let shell_options = maho_tools::bash::BashToolOptions {
                    shell_env: Some(Arc::new(maho_core::utils::shell::get_shell_env)),
                    // Child sessions follow the parent's `bashEnvScrub`/`bashEnvPassthrough`; scrub by default.
                    env_passthrough: parent()
                        .map(|session| session.with_settings_manager(|manager| manager.get_bash_env_scrub_passthrough()))
                        .unwrap_or_else(|| Some(Vec::new())),
                    // Child sessions follow the parent's `bashSandbox` (default off).
                    sandbox: Some(
                        parent()
                            .map(|session| session.with_settings_manager(|manager| manager.get_bash_sandbox()))
                            .unwrap_or_default(),
                    ),
                    sandbox_agent_dir: Some(std::path::PathBuf::from(
                        parent()
                            .map(|session| session.with_settings_manager(|manager| manager.agent_dir()))
                            .unwrap_or_else(maho_core::config::get_agent_dir),
                    )),
                    ..Default::default()
                };
                maho_tools::index::create_all_tool_definitions(
                    std::path::Path::new(&options.cwd),
                    maho_tools::index::ToolsOptions {
                        bash: shell_options.clone(),
                        powershell: shell_options,
                        ..Default::default()
                    },
                )
            });
            if let Some(definition) = builtins.get(&name) {
                return Ok(definition.clone());
            }
            let parent_definition = parent().and_then(|parent| parent.get_tool_definition(&name));
            let tool = tool.clone();
            let description = tool.description().to_owned();
            let Some(parameters) = tool
                .parameters()
                .or_else(|| parent_definition.map(|tool| tool.parameters))
            else {
                return Err(HostError {
                    message: format!("Shared parent tool {name} has no native schema"),
                });
            };
            Ok(maho_ext_api::ToolDefinition::new(
                &name,
                &description,
                parameters,
                Arc::new(move |call| {
                    let tool = tool.clone();
                    let id = call.id.to_owned();
                    let params = call.params;
                    let signal = call.signal;
                    Box::pin(async move {
                        let controller = maho_ai::utils::abort::AbortController::new();
                        if signal.is_aborted() {
                            controller.abort(None);
                        }
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
                        serde_json::from_value(result)
                            .map_err(|error| maho_tools::ToolError::Message(error.to_string()))
                    })
                }),
            ))
        })
        .collect::<Result<Vec<_>, HostError>>()?;
    let mut sdk_options = super::task_runners::native_child_sdk_options(options, custom)?;
    if let Some(identity) = &options.workpool_identity {
        sdk_options
            .custom_tools
            .retain(|tool| tool.name != "workpool");
        sdk_options
            .custom_tools
            .push(super::task_runners::workpool_yield_tool_with_events(
                identity,
                &options.cwd,
                workpool_events.clone(),
            ));
    }
    sdk_options.model = model;
    sdk_options.model_runtime = models;
    sdk_options.model_registry = registry;
    let create_session = maho_core::sdk::create_agent_session(sdk_options);
    let session = block_on_sync(executor.clone(), create_session)?;
    Ok(Arc::new(session.session))
}

pub fn factory_with_workpool_events(
    executor: tokio::runtime::Handle,
    parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
    workpool_events: Option<super::task_runners::WorkpoolEvents>,
) -> senpi_task::runners::in_process::runner::CreateChildSession {
    Arc::new(move |options| {
        let session = create_native_child_session(&options, &executor, &parent, &workpool_events)?;
        let options = Arc::new(options);
        let reopen_executor = executor.clone();
        let reopen_parent = parent.clone();
        let reopen_workpool = workpool_events.clone();
        let reopen: ReopenChildSession = Arc::new(move || {
            create_native_child_session(
                &options,
                &reopen_executor,
                &reopen_parent,
                &reopen_workpool,
            )
        });
        Ok(Arc::new(NativeChild::with_reopen(
            session,
            executor.clone(),
            workpool_events.clone(),
            Some(reopen),
        )) as Arc<dyn ChildSession>)
    })
}

impl senpi_task::runners::pinned_model_equivalence::PriorityPairingRegistry for NativeChild {
    fn get_service_tier(
        &self,
        identity: &senpi_task::runners::pinned_model_equivalence::ModelIdentity,
    ) -> Option<String> {
        let session = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session
            .clone()?;
        let registry = session.model_registry();
        let model = registry.find(&identity.provider, &identity.id)?;
        registry
            .get_service_tier(&model)
            .and_then(|tier| serde_json::to_value(tier).ok())
            .and_then(|value| value.as_str().map(str::to_string))
    }
    fn get_upstream_model_id(
        &self,
        identity: &senpi_task::runners::pinned_model_equivalence::ModelIdentity,
    ) -> Option<String> {
        let session = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session
            .clone()?;
        let registry = session.model_registry();
        let model = registry.find(&identity.provider, &identity.id)?;
        registry.get_upstream_model_id(&model)
    }
}

impl senpi_task::pause::native_child_pause::NativeRunPause for NativeChild {
    fn pause_for_run(
        &self,
        owner: senpi_task::pause::ports::RunPauseOwner,
    ) -> Result<
        senpi_task::pause::ports::EngineInheritReceipt,
        senpi_task::pause::native_child_pause::NativePauseError,
    > {
        use senpi_task::pause::{native_child_pause::NativePauseError, ports::*};
        let _control = self
            .run_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = self
            .ensure_session()
            .map_err(|error| NativePauseError::Engine(error.to_string()))?;
        let kind = match owner.run_ref.kind {
            RunRefKind::Task => "task",
            RunRefKind::Dag => "dag",
            RunRefKind::Team => "team",
            RunRefKind::Workpool => "workpool",
        };
        block_on_sync(self.executor.clone(), async move {
            session
                .pause_for_run(
                    &owner.parent_session_id,
                    owner.parent_pause_seq,
                    maho_core::run_holds::RunRef {
                        kind: kind.into(),
                        id: owner.run_ref.id,
                    },
                )
                .await
        })
        .map(|receipt| match receipt {
            maho_core::run_holds::InheritResult::Inherited { child_seq } => {
                EngineInheritReceipt::Inherited { child_seq }
            }
            maho_core::run_holds::InheritResult::WrongParent => EngineInheritReceipt::WrongParent,
        })
        .map_err(|error| NativePauseError::Engine(error.to_string()))
    }
    fn resume_for_run(
        &self,
        request: senpi_task::pause::ports::ResumeTurnRequest,
    ) -> Result<
        senpi_task::pause::native_child_pause::NativeResumeResult,
        senpi_task::pause::native_child_pause::NativePauseError,
    > {
        use senpi_task::pause::{
            native_child_pause::{NativePauseError, NativeResumeResult},
            ports::*,
        };
        let _control = self
            .run_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = self
            .ensure_session()
            .map_err(|error| NativePauseError::Engine(error.to_string()))?;
        let turn_id = request.resume_id.clone();
        let (settled, outcome) = std::sync::mpsc::sync_channel(1);
        let prompt_failed = settled.clone();
        let turn_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let subscription = session.subscribe(Arc::new(move |event| {
            let Ok(record) = maho_rpc::session_binding::session_event_record(event) else {
                return;
            };
            if record["type"] == "session_paused" {
                let _ = settled.try_send(senpi_task::runners::RunnerOutcome::Cancelled);
                return;
            }
            if record["type"] == "agent_start" {
                turn_started.store(true, std::sync::atomic::Ordering::Release);
            }
            if record["type"] != "agent_end"
                || record["willRetry"] == true
                || !turn_started.load(std::sync::atomic::Ordering::Acquire)
            {
                return;
            }
            let message = record["messages"].as_array().and_then(|messages| {
                messages
                    .iter()
                    .rev()
                    .find(|message| message["role"] == "assistant")
            });
            let result = match message {
                Some(message) if message["stopReason"] == "aborted" => {
                    senpi_task::runners::RunnerOutcome::Cancelled
                }
                Some(message) if message["stopReason"] == "error" => {
                    senpi_task::runners::RunnerOutcome::error(
                        senpi_task::runners::RunnerFailureKind::ChildTurnFailed,
                        message["errorMessage"]
                            .as_str()
                            .unwrap_or("child turn failed"),
                    )
                }
                Some(message) => senpi_task::runners::RunnerOutcome::completed(
                    message["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|block| block["type"] == "text")
                        .filter_map(|block| block["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                None => senpi_task::runners::RunnerOutcome::completed(""),
            };
            let _ = settled.try_send(result);
        }));
        let result = session
            .release_run_hold(
                &request.owner.parent_session_id,
                request.owner.parent_pause_seq,
                request.child_seq_cutoff,
            )
            .map_err(NativePauseError::Engine)?;
        Ok(match result {
            maho_core::run_holds::ReleaseInheritedResult::WrongParent => {
                NativeResumeResult::Refused(ResumeRefusal::WrongParent)
            }
            maho_core::run_holds::ReleaseInheritedResult::NotHeld => {
                NativeResumeResult::Refused(ResumeRefusal::NotHeld)
            }
            maho_core::run_holds::ReleaseInheritedResult::Stale => {
                NativeResumeResult::Refused(ResumeRefusal::Stale)
            }
            maho_core::run_holds::ReleaseInheritedResult::Released {
                other_hold_live: true,
            } => NativeResumeResult::OtherHoldLive,
            maho_core::run_holds::ReleaseInheritedResult::Released {
                other_hold_live: false,
            } => {
                {
                    let (admitted, admission) = std::sync::mpsc::sync_channel(1);
                    self.executor.spawn(async move {
                        let signal = admitted.clone();
                        let result = session
                            .prompt(
                                maho_core::manual_continue::MANUAL_CONTINUE_DIRECTIVE,
                                maho_core::agent_session::PromptOptions {
                                    prompt_admitted: Some(Arc::new(move |_| {
                                        let _ = signal.try_send(());
                                    })),
                                    ..Default::default()
                                },
                            )
                            .await;
                        match result {
                            Err(error) => {
                                let _ = prompt_failed.try_send(
                                    senpi_task::runners::RunnerOutcome::error(
                                        senpi_task::runners::RunnerFailureKind::ChildTurnFailed,
                                        error,
                                    ),
                                );
                            }
                            Ok(maho_core::agent_session::PromptDisposition::Handled) => {
                                let _ = prompt_failed
                                    .try_send(senpi_task::runners::RunnerOutcome::completed(""));
                            }
                            _ => {}
                        }
                        let _ = admitted.try_send(());
                    });
                    admission
                        .recv()
                        .map_err(|error| NativePauseError::Engine(error.to_string()))?;
                }
                NativeResumeResult::Turn(ResumedChildTurn {
                    turn_id,
                    outcome: Box::new(move || {
                        let _subscription = subscription;
                        outcome.recv().unwrap_or_else(|error| {
                            senpi_task::runners::RunnerOutcome::error(
                                senpi_task::runners::RunnerFailureKind::ChildTurnFailed,
                                error.to_string(),
                            )
                        })
                    }),
                })
            }
        })
    }
    fn hold_input(
        &self,
        parent: &str,
        input: senpi_task::pause::ports::HoldInput,
    ) -> Result<
        senpi_task::pause::ports::EngineHoldInputResult,
        senpi_task::pause::native_child_pause::NativePauseError,
    > {
        use senpi_task::pause::{
            native_child_pause::NativePauseError, ports::EngineHoldInputResult,
        };
        let session = self
            .ensure_session()
            .map_err(|error| NativePauseError::Engine(error.to_string()))?;
        if session
            .with_run_holds(|holds| holds.inherited_owner())
            .is_none_or(|owner| owner.parent_session_id != parent)
        {
            return Err(NativePauseError::Engine("wrong_parent".into()));
        }
        let receipt = session.hold_input(serde_json::json!({"inputId":input.input_id,"revision":input.revision,"text":input.text,"origin":"human"})).map_err(NativePauseError::Engine)?;
        if let Some(revision) = receipt["heldRevision"].as_u64() {
            return Ok(EngineHoldInputResult::InputIdConflict {
                held_revision: revision,
            });
        }
        if let Some(revision) = receipt["revision"]
            .as_u64()
            .filter(|_| receipt["kind"] == "already_delivered")
        {
            return Ok(EngineHoldInputResult::AlreadyDelivered { revision });
        }
        Ok(EngineHoldInputResult::Held {
            entry_id: receipt["entryId"].as_str().unwrap_or_default().into(),
            duplicate: receipt["duplicate"] == true,
        })
    }
    fn observe_hold(
        &self,
        parent: &str,
    ) -> Result<
        Option<senpi_task::pause::ports::ObservedHold>,
        senpi_task::pause::native_child_pause::NativePauseError,
    > {
        let session = self.ensure_session().map_err(|error| {
            senpi_task::pause::native_child_pause::NativePauseError::Engine(error.to_string())
        })?;
        Ok(session.with_run_holds(|holds| {
            holds
                .inherited_owner()
                .filter(|owner| owner.parent_session_id == parent)
                .map(|_| senpi_task::pause::ports::ObservedHold {
                    held: true,
                    child_seq: holds.pause_seq(),
                })
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{block_on_sync, factory};
    use std::sync::Arc;

    #[test]
    fn synchronous_child_creation_can_run_inside_async_runtime() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let executor = runtime.handle().clone();

        runtime.block_on(async move {
            let value =
                block_on_sync(executor, async { Ok::<_, String>(42) }).expect("child creation");
            assert_eq!(value, 42);
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_crossing_real_native_staged_creation_holds_first_prompt_without_running_it() {
        use senpi_task::manager::types::*;
        struct CrossingRunner {
            barrier: senpi_task::pause::admission_barrier::AdmissionBarrier,
            child: Arc<senpi_task::runners::in_process::child_handle::InProcessChildHandle>,
        }
        impl ManagedRunner for CrossingRunner {
            fn start(&self, _: &ManagedStartSpec) -> ManagedRunnerResult {
                panic!("must stage")
            }
            fn start_staged(
                &self,
                _: &ManagedStartSpec,
            ) -> Option<Result<StagedManagedChild, ManagedRunnerError>> {
                self.barrier.close("parent", 8);
                Some(Ok(StagedManagedChild {
                    child: self.child.clone(),
                    first_prompt: "held first prompt".into(),
                    begin: Box::new(|| panic!("Stop crossed creation; first turn must not begin")),
                }))
            }
        }
        let root = tempfile::tempdir().unwrap();
        let session = Arc::new(
            maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
                cwd: Some(root.path().to_string_lossy().into_owned()),
                ..Default::default()
            })
            .await
            .unwrap()
            .session,
        );
        let native = Arc::new(super::NativeChild::new(
            session.clone(),
            tokio::runtime::Handle::current(),
        ));
        let barrier = senpi_task::pause::admission_barrier::AdmissionBarrier::new();
        let child = senpi_task::runners::in_process::child_handle::InProcessChildHandle::restored(
            "staged", native,
        );
        let runner = Arc::new(CrossingRunner {
            barrier: barrier.clone(),
            child,
        });
        let store = senpi_task::store::TaskRecordStore::new(&senpi_task::store::StateDirConfig {
            project_dir: root.path().into(),
            task_state_dir: None,
        });
        let mut options = TaskManagerOptions::new(
            store.clone(),
            ManagedRunners {
                in_process: runner.clone(),
                process: runner,
            },
            Arc::new(|_| {
                Ok(ResolvedChildPlan {
                    model: "p/m".into(),
                    ..Default::default()
                })
            }),
            root.path().to_string_lossy(),
        );
        options.admission_barrier = Some(barrier);
        let manager = senpi_task::manager::create_task_manager(options);
        tokio::task::spawn_blocking(move || {
            let started = manager.start(&ManagerStartSpec {
                parent_session_id: "parent".into(),
                prompt: "held first prompt".into(),
                ..Default::default()
            });
            let StartResult::Started(task) = started else {
                panic!("start refused: {started:?}")
            };
            assert_eq!(
                store.load(&task.task_id).unwrap().unwrap().status,
                senpi_task::state::TaskStatus::Paused
            );
            assert_eq!(session.held_inputs()[0]["text"], "held first prompt");
            assert!(session.paused());
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_pause_receipts_are_real_and_idempotent() {
        use senpi_task::pause::{native_child_pause::NativeRunPause, ports::*};
        let root = tempfile::tempdir().unwrap();
        let session =
            maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
                cwd: Some(root.path().to_string_lossy().into_owned()),
                ..Default::default()
            })
            .await
            .unwrap()
            .session;
        let child = super::NativeChild::new(Arc::new(session), tokio::runtime::Handle::current());
        tokio::task::spawn_blocking(move || {
            let owner = RunPauseOwner {
                parent_session_id: "parent".into(),
                parent_pause_seq: 2,
                run_ref: RunRef::task("task"),
            };
            assert!(matches!(
                child.pause_for_run(owner).unwrap(),
                EngineInheritReceipt::Inherited { .. }
            ));
            let input = HoldInput {
                input_id: "input".into(),
                revision: 1,
                text: "guidance".into(),
            };
            let first = child.hold_input("parent", input.clone()).unwrap();
            assert!(matches!(
                first,
                EngineHoldInputResult::Held {
                    duplicate: false,
                    ..
                }
            ));
            assert!(matches!(
                child.hold_input("parent", input).unwrap(),
                EngineHoldInputResult::Held {
                    duplicate: true,
                    ..
                }
            ));
            assert!(
                child
                    .hold_input(
                        "other",
                        HoldInput {
                            input_id: "other".into(),
                            revision: 1,
                            text: "wrong".into()
                        }
                    )
                    .is_err()
            );
            assert!(child.observe_hold("parent").unwrap().unwrap().held);
            assert!(child.observe_hold("other").unwrap().is_none());
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_child_shared_write_stays_in_child_cwd() {
        let root = tempfile::tempdir().expect("temporary workspace");
        let parent_cwd = root.path().join("parent");
        let child_cwd = root.path().join("child");
        std::fs::create_dir_all(&parent_cwd).expect("parent cwd");
        std::fs::create_dir_all(&child_cwd).expect("child cwd");
        use maho_ai::providers::faux::{
            FauxAssistantMessageOptions, faux_assistant_message, faux_provider, faux_tool_call,
        };
        let provider = faux_provider(Default::default()).expect("valid faux provider");
        provider.set_responses(vec![
            faux_assistant_message(
                faux_tool_call(
                    "write",
                    serde_json::json!({"path":"child-only.txt","content":"child"})
                        .as_object()
                        .expect("arguments")
                        .clone(),
                    Some("child-write"),
                ),
                FauxAssistantMessageOptions {
                    stop_reason: Some(maho_ai::types::StopReason::ToolUse),
                    ..Default::default()
                },
            )
            .into(),
            faux_assistant_message("done", Default::default()).into(),
        ]);
        let model = provider.get_model(Some("faux-1")).expect("faux model");
        let mut model_runtime = maho_core::model_runtime::ModelRuntime::create_sync(
            maho_core::model_runtime::CreateModelRuntimeOptions {
                models_path: Some(root.path().join("models.json")),
                providers: Some(Vec::new()),
                ..Default::default()
            },
        )
        .expect("valid model runtime");
        model_runtime.register_native_provider(provider.provider.clone());
        let registry = maho_core::model_registry::ModelRegistry::new(model_runtime.clone());
        registry
            .auth_storage
            .set(
                "faux",
                Some(serde_json::json!({"type":"api_key","key":"child-fixture"})),
            )
            .expect("faux auth");
        let parent = Arc::new(
            maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
                cwd: Some(parent_cwd.to_string_lossy().into_owned()),
                model: Some(model.clone()),
                model_runtime: Some(model_runtime.clone()),
                tools: Some(vec!["write".into()]),
                ..Default::default()
            })
            .await
            .expect("parent session")
            .session,
        );
        let parent_accessor: Arc<
            dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync,
        > = Arc::new({
            let parent = Arc::downgrade(&parent);
            move || parent.upgrade().map(|session| (*session).clone())
        });
        let shared_write = super::super::task_runners::live_parent_tools(
            parent_accessor.clone(),
            tokio::runtime::Handle::current(),
        )()
        .into_iter()
        .find(|tool| tool.name() == "write")
        .expect("parent write tool");
        let child_cwd = child_cwd.to_string_lossy().into_owned();
        let options = senpi_task::runners::in_process::child_options::ChildSessionOptions {
            workpool_identity: None,
            cwd: child_cwd.clone(),
            session_manager: Arc::new(
                senpi_task::runners::in_process::session_manager::ChildSessionManager::create(
                    &child_cwd, &child_cwd,
                )
                .expect("child session manager"),
            ),
            resource_loader:
                senpi_task::runners::in_process::child_options::ChildResourceLoader::Minimal,
            system_prompt: None,
            custom_tools: vec![shared_write],
            agent_dir: None,
            auth_storage: Some(registry.auth_storage.clone()),
            model_registry: Some(Arc::new(
                super::super::task_runners::NativeChildModelRegistry(registry),
            )),
            model_runtime: Some(Arc::new(model_runtime)),
            model: Some(Arc::new(model)),
            thinking_level: None,
            settings: Default::default(),
            tools: Some(vec!["write".into()]),
            exclude_tools: None,
        };
        let child = factory(tokio::runtime::Handle::current(), parent_accessor)(options)
            .expect("native child");
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = events.clone();
        let unsubscribe = child.subscribe(Arc::new(move |event| {
            observed.lock().expect("child events").push(event.clone())
        }));
        tokio::task::spawn_blocking(move || {
            child.prompt("write the child file").expect("child prompt");
            child.dispose();
        })
        .await
        .expect("child worker");
        unsubscribe();
        parent.dispose().await;

        assert_eq!(
            std::fs::read_to_string(child_cwd + "/child-only.txt").unwrap_or_else(|error| panic!(
                "child output: {error}; events: {:?}",
                events.lock().expect("child events")
            )),
            "child"
        );
        assert!(!parent_cwd.join("child-only.txt").exists());
    }
}
