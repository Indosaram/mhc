use std::{collections::BTreeMap, sync::{Arc, Mutex, PoisonError}, thread::JoinHandle};
use maho_ext_api::{ExtensionApi, ExtensionEvent, EventKind, EventResult, FlagValue};
use senpi_task::{completion::{CompletionRequest, ReconcileUnnotifiedNotificationsInput, TransitionReason}, manager::{AbortSignal, types::ListScope}};
use crate::{engine::TaskEngine, resumption_channel_emitter::{ResumptionChannelEmitter, TaskResumptionChannelManager}, session_transition_bridge::SessionTransitionBridge, status_ui::TaskStatusUi, timers::HostTimers, tools::TaskToolsDeps};

struct CompletionWaiter { signal: AbortSignal, thread: JoinHandle<()> }
pub struct TaskComponent {
    pub engine: TaskEngine,
    pub status: Arc<TaskStatusUi>,
    channels: Mutex<ResumptionChannelEmitter>,
    transitions: Mutex<SessionTransitionBridge>,
    waiters: Mutex<BTreeMap<String, CompletionWaiter>>,
    rpc: Mutex<Option<Arc<crate::task_rpc_bridge::TaskRpcBridge>>>,
    terminal: Mutex<Option<crate::completion_bridge::TerminalObserver>>,
    delivery: Mutex<()>,
    terminal_epochs: Mutex<std::collections::BTreeSet<(String, i64)>>,
}
impl TaskComponent {
    pub fn register_with_process_sweep(api: &mut ExtensionApi, engine: TaskEngine, spawn: senpi_task::tools::task::execute_spec::TaskToolDeps, ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps, member_process: bool, sweep: crate::process_sweep::SessionStartProcessSweepOptions) -> Result<Option<Arc<Self>>, maho_ext_api::ExtensionFailure> {
        if member_process { return Ok(None); }
        crate::process_sweep::wire_session_start_process_sweep(api, sweep);
        Self::register(api, engine, spawn, ownership, false)
    }
    pub fn register(api: &mut ExtensionApi, engine: TaskEngine, spawn: senpi_task::tools::task::execute_spec::TaskToolDeps, ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps, member_process: bool) -> Result<Option<Arc<Self>>, maho_ext_api::ExtensionFailure> {
        if member_process { return Ok(None); }
        crate::registration::register_task_flags(api);
        if api.get_flag("omo-task") == Some(FlagValue::Boolean(false)) { return Ok(None); }
        let state = engine.runtime.clone();
        let session = Arc::new(move || state.lock().unwrap_or_else(PoisonError::into_inner).session_id().map(str::to_owned));
        let channels = ResumptionChannelEmitter::new(api.events.clone(), Arc::new(TaskResumptionChannelManager { manager: engine.manager.clone(), ownership }), session);
        let status = TaskStatusUi::new(engine.manager.clone(), engine.runtime.clone(), Arc::new(HostTimers::default()), Arc::new(|| chrono::Utc::now().timestamp_millis()), Arc::new(|| None));
        let transitions = SessionTransitionBridge::new(engine.runtime.clone(), engine.notifier.clone());
        let component = Arc::new(Self { engine, status, channels: Mutex::new(channels), transitions: Mutex::new(transitions), waiters: Mutex::new(BTreeMap::new()), rpc: Mutex::new(None), terminal: Mutex::new(None), delivery: Mutex::new(()), terminal_epochs: Mutex::new(std::collections::BTreeSet::new()) });
        let state = component.engine.runtime.clone();
        let events = api.events.clone();
        let rpc = crate::task_rpc_bridge::wire_task_rpc_bridge(api, component.engine.manager.clone(), Arc::new(move || state.lock().unwrap_or_else(PoisonError::into_inner).session_id().map(str::to_owned)), component.engine.store.state_dir().to_string_lossy().into_owned(), Arc::new(move |name, value| events.emit("senpi:extension-rpc-event", &serde_json::json!({"name":name,"data":value}))))?;
        *component.rpc.lock().unwrap_or_else(PoisonError::into_inner) = Some(rpc);
        let tracker = crate::skill_invocation_tracker::SkillInvocationTracker::new().map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
        tracker.register(api);
        crate::renderers::register_task_message_renderers(api);
        crate::registration::register_removed_team_wait_hint(api);
        let weak = Arc::downgrade(&component);
        crate::commands::register_task_commands_with_sync(api, component.engine.manager.clone(), Arc::new(move || { if let Some(component) = weak.upgrade() { component.sync(); } }));
        let weak = Arc::downgrade(&component);
        crate::tools::register_task_tools_with_sync(api, TaskToolsDeps { manager: component.engine.manager.clone(), state_dir: component.engine.store.state_dir().to_string_lossy().into_owned(), omo_config: component.engine.config.clone(), agents: component.engine.agents.clone(), spawn, policy: tracker, team_routing: None }, Arc::new(move || { if let Some(component) = weak.upgrade() { component.sync(); } }));
        crate::reload_guard::wire_reload_guard(api, component.engine.manager.clone());
        let usage = api.get_flag("omo-task-usage-hint") != Some(FlagValue::Boolean(false));
        crate::event_bridge::wire_task_usage_guidance(api, Arc::new(move || usage));
        for kind in [EventKind::SessionStart, EventKind::SessionBeforeSwitch, EventKind::SessionBeforeCompact, EventKind::SessionCompact, EventKind::SessionShutdown, EventKind::AgentEnd, EventKind::ToolResult] {
            let component = component.clone();
            api.on(kind, Arc::new(move |event, context| { let component = component.clone(); Box::pin(async move {
                component.engine.runtime.lock().unwrap_or_else(PoisonError::into_inner).capture_from(context);
                let session = context.session_manager.session_id();
                match event {
                    ExtensionEvent::SessionStart(_) => {
                        component.transitions.lock().unwrap_or_else(PoisonError::into_inner).resolve(Some(session)).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                        component.engine.lifecycle.reconcile_on_session_start(Some(session)).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                        component.engine.lifecycle.cleanup_expired_records().map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                        let parent_state = component.engine.runtime.lock().unwrap_or_else(PoisonError::into_inner).parent_state();
                        let delivery = component.delivery.lock().unwrap_or_else(PoisonError::into_inner);
                        component.engine.notifier.reconcile_unnotified_notifications(ReconcileUnnotifiedNotificationsInput { session_id: session, parent_state }).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                        drop(delivery);
                        component.channels.lock().unwrap_or_else(PoisonError::into_inner).emit_session_start();
                        if let Some(rpc) = &*component.rpc.lock().unwrap_or_else(PoisonError::into_inner) { rpc.attach(); }
                        component.sync();
                    }
                    ExtensionEvent::SessionBeforeSwitch { .. } => { if let Some(rpc) = &*component.rpc.lock().unwrap_or_else(PoisonError::into_inner) { rpc.detach(); } component.transitions.lock().unwrap_or_else(PoisonError::into_inner).mark(TransitionReason::SessionSwitching, Some(session)); component.channels.lock().unwrap_or_else(PoisonError::into_inner).emit_shutdown(); component.engine.runtime.lock().unwrap_or_else(PoisonError::into_inner).clear_ui(); }
                    ExtensionEvent::SessionBeforeCompact(_) => component.transitions.lock().unwrap_or_else(PoisonError::into_inner).mark(TransitionReason::Compacting, Some(session)),
                    ExtensionEvent::SessionCompact(_) => { component.transitions.lock().unwrap_or_else(PoisonError::into_inner).resolve(Some(session)).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?; component.sync(); }
                    ExtensionEvent::SessionShutdown(shutdown) => {
                        component.transitions.lock().unwrap_or_else(PoisonError::into_inner).mark(TransitionReason::SessionShutdown, Some(session));
                        component.dispose();
                        component.engine.lifecycle.suspend_on_session_shutdown(&senpi_task::lifecycle::SuspendInput { parent_session_id: session.into(), reason: match shutdown.reason { maho_ext_api::SessionReason::Startup => "startup", maho_ext_api::SessionReason::Reload => "reload", maho_ext_api::SessionReason::New => "new", maho_ext_api::SessionReason::Resume => "resume", maho_ext_api::SessionReason::Fork => "fork", maho_ext_api::SessionReason::Quit => "quit" }.into() }).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                        component.engine.runtime.lock().unwrap_or_else(PoisonError::into_inner).clear_ui();
                    }
                    _ => component.sync(),
                }
                Ok(EventResult::None)
            }) }));
        }
        Ok(Some(component))
    }
    pub fn sync(self: &Arc<Self>) {
        self.status.schedule_sync();
        let rpc = self.rpc.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(rpc) = rpc { rpc.sync(); }
        self.channels.lock().unwrap_or_else(PoisonError::into_inner).emit_if_changed();
        let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        let finished: Vec<_> = waiters.iter().filter(|(_, waiter)| waiter.thread.is_finished()).map(|(id, _)| id.clone()).collect();
        for id in finished { if let Some(waiter) = waiters.remove(&id) && waiter.thread.join().is_err() { eprintln!("task completion waiter panicked"); } }
        for entry in self.engine.manager.list(&ListScope::All) {
            if entry.record.status.is_terminal() {
                self.notify_owned_terminal(&entry.record);
                let parent_state = self.engine.runtime.lock().unwrap_or_else(PoisonError::into_inner).parent_state();
                let delivery = self.delivery.lock().unwrap_or_else(PoisonError::into_inner);
                if !waiters.contains_key(&entry.record.task_id) && let Err(error) = self.engine.notifier.notify_terminal(&CompletionRequest { run_in_background: self.engine.manager.was_background(&entry.record.task_id), record: entry.record, parent_state, tokens: None }) { eprintln!("task terminal delivery failed: {error}"); }
                drop(delivery);
                continue;
            }
            if waiters.contains_key(&entry.record.task_id) { continue; }
            let id = entry.record.task_id;
            let manager = self.engine.manager.clone();
            let signal = AbortSignal::default();
            let abort = signal.clone();
            let weak = Arc::downgrade(self);
            let task = id.clone();
            let thread = std::thread::spawn(move || {
                if let Ok(record) = manager.wait_for(&task, Some(&abort), None) && let Some(component) = weak.upgrade() {
                    component.notify_owned_terminal(&record);
                    let parent_state = component.engine.runtime.lock().unwrap_or_else(PoisonError::into_inner).parent_state();
                    let delivery = component.delivery.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Err(error) = component.engine.notifier.notify_terminal(&CompletionRequest { record, parent_state, run_in_background: manager.was_background(&task), tokens: None }) { eprintln!("task terminal delivery failed: {error}"); }
                    drop(delivery);
                    component.channels.lock().unwrap_or_else(PoisonError::into_inner).emit_if_changed();
                    component.status.schedule_sync();
                    let rpc = component.rpc.lock().unwrap_or_else(PoisonError::into_inner).clone();
                    if let Some(rpc) = rpc { rpc.sync(); }
                }
            });
            waiters.insert(id, CompletionWaiter { signal, thread });
        }
    }
    fn notify_owned_terminal(&self, record: &senpi_task::state::TaskRecord) {
        if crate::member_liveness::liveness_details(record).is_none() { return; }
        let terminal = self.terminal.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(terminal) = terminal {
            let key = (record.task_id.clone(), record.notification.run_epoch);
            let fresh = self.terminal_epochs.lock().unwrap_or_else(PoisonError::into_inner).insert(key);
            if fresh { terminal(record); }
        }
    }
    pub fn register_team_runtime(self: &Arc<Self>, api: &mut ExtensionApi, service: Arc<crate::team_service::TeamService>, pollers: Arc<crate::lead_poller_lifecycle::LeadPollerLifecycle>, liveness: Arc<crate::member_liveness::TeamMemberLivenessNotifier>, ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps) {
        crate::tools::register_lead_team_tools(api, service.clone());
        let runtime = self.engine.runtime.clone();
        *self.terminal.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(crate::owned_member_liveness::create_owned_member_liveness_notifier(ownership, Arc::new(move || runtime.lock().unwrap_or_else(PoisonError::into_inner).session_id().map(str::to_owned)), liveness.clone())));
        for kind in [EventKind::SessionStart, EventKind::AgentEnd, EventKind::SessionShutdown] {
            let service = service.clone(); let pollers = pollers.clone(); let liveness = liveness.clone(); let runtime = self.engine.runtime.clone();
            api.on(kind, Arc::new(move |event, _| {
                let service = service.clone(); let pollers = pollers.clone(); let liveness = liveness.clone(); let runtime = runtime.clone();
                Box::pin(async move {
                    match event {
                        ExtensionEvent::SessionStart(_) => { service.reconcile_mailbox(); pollers.tick().map_err(maho_ext_api::ExtensionFailure::new)?; }
                        ExtensionEvent::AgentEnd { .. } => liveness.acknowledge_persisted(Arc::new(move || runtime.lock().unwrap_or_else(PoisonError::into_inner).session_file().map(ToOwned::to_owned))),
                        ExtensionEvent::SessionShutdown(_) => pollers.shutdown(),
                        _ => {}
                    }
                    Ok(EventResult::None)
                })
            }));
        }
    }
    pub fn dispose(&self) {
        let waiters = std::mem::take(&mut *self.waiters.lock().unwrap_or_else(PoisonError::into_inner));
        for waiter in waiters.values() { waiter.signal.abort(); }
        for waiter in waiters.into_values() { if waiter.thread.join().is_err() { eprintln!("task completion waiter panicked"); } }
        self.status.dispose();
        if let Some(rpc) = self.rpc.lock().unwrap_or_else(PoisonError::into_inner).take() { rpc.dispose(); }
        self.channels.lock().unwrap_or_else(PoisonError::into_inner).emit_shutdown();
    }
}
