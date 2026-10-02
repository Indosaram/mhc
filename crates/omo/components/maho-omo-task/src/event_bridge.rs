use std::sync::{Arc,Mutex,PoisonError};
use maho_ext_api::{ExtensionApi,ExtensionContext,ExtensionEvent,ExtensionFailure,EventKind,EventResult,SessionReason};
use senpi_task::{manager::{TaskManager,types::ListScope},lifecycle::{TaskLifecycle,SuspendInput},completion::{CompletionNotifier,ReconcileUnnotifiedNotificationsInput,TransitionReason},state::TaskRecord};
use crate::{runtime_context::TaskRuntimeContext,session_transition_bridge::SessionTransitionBridge,status_ui::TaskStatusUi,task_rpc_bridge::TaskRpcBridge,lead_poller_lifecycle::LeadPollerLifecycle};
pub type RecoveryAction=Arc<dyn Fn()->Result<(),String>+Send+Sync>;
pub type ObserveLiveness=Arc<dyn Fn(&TaskRecord)+Send+Sync>;
pub const TASK_USAGE_HINT_FLAG:&str="omo-task-usage-hint";
pub fn wire_task_usage_guidance(api:&mut ExtensionApi,enabled:Arc<dyn Fn()->bool+Send+Sync>) {
    let guard=Arc::new(Mutex::new(crate::usage_guidance::OncePerSessionGuard::default()));
    api.on(EventKind::BeforeAgentStart,Arc::new(move |_,context| {
        let first=enabled() && guard.lock().unwrap_or_else(PoisonError::into_inner).first_delivery(context.session_manager.session_id());
        Box::pin(async move { Ok(if first { EventResult::BeforeAgentStart(maho_ext_api::BeforeAgentStartEventResult { message:Some(maho_ext_api::CustomMessage { custom_type:"senpi-task.usage".into(),content:vec![maho_ext_api::ToolContent::text(crate::usage_guidance::TASK_USAGE_GUIDANCE)],display:false,details:Some(serde_json::json!({})) }),system_prompt:None }) } else { EventResult::None }) })
    }));
}
pub struct EventBridgeDeps {
    pub runtime:Arc<Mutex<TaskRuntimeContext>>,pub manager:Arc<TaskManager>,pub lifecycle:Arc<TaskLifecycle>,pub notifier:CompletionNotifier,pub transitions:Mutex<SessionTransitionBridge>,pub status_ui:Arc<TaskStatusUi>,pub task_rpc:Arc<TaskRpcBridge>,pub lead_pollers:Arc<LeadPollerLifecycle>,pub notify_liveness:ObserveLiveness,pub reconcile_mailbox:RecoveryAction,pub resumption_start:RecoveryAction,pub resumption_shutdown:RecoveryAction,pub acknowledge_liveness:RecoveryAction,pub on_warning:Arc<dyn Fn(String)+Send+Sync>,pub unsubscribe_snapshots:Mutex<Option<Box<dyn FnOnce()+Send>>>,
}
fn failure(error:impl std::fmt::Display)->ExtensionFailure { ExtensionFailure::new(error.to_string()) }
pub fn wire_event_bridge(api:&mut ExtensionApi,deps:Arc<EventBridgeDeps>) {
    crate::reload_guard::wire_reload_guard(api,deps.manager.clone());
    for kind in [EventKind::SessionStart,EventKind::SessionBeforeSwitch,EventKind::SessionBeforeCompact,EventKind::SessionCompact,EventKind::SessionShutdown,EventKind::ModelSelect,EventKind::AgentEnd] {
        let deps=deps.clone(); api.on(kind,Arc::new(move |event,context| { let deps=deps.clone(); Box::pin(async move { deps.handle(event,context)?; Ok(EventResult::None) }) }));
    }
}
impl EventBridgeDeps {
    pub fn handle(&self,event:&ExtensionEvent,context:&ExtensionContext)->Result<(),ExtensionFailure> {
        if matches!(event,ExtensionEvent::SessionBeforeSwitch { .. }) { self.task_rpc.detach(); }
        if matches!(event,ExtensionEvent::SessionShutdown(_)) {
            if let Some(unsubscribe)=self.unsubscribe_snapshots.lock().unwrap_or_else(PoisonError::into_inner).take() { unsubscribe(); }
            self.task_rpc.dispose();
        }
        self.runtime.lock().unwrap_or_else(PoisonError::into_inner).capture_from(context);
        let session=self.runtime.lock().unwrap_or_else(PoisonError::into_inner).session_id().map(str::to_owned);
        match event {
            ExtensionEvent::SessionStart(_) => {
                self.transitions.lock().unwrap_or_else(PoisonError::into_inner).resolve(session.as_deref()).map_err(failure)?;
                let reconciliation=self.lifecycle.reconcile_on_session_start(session.as_deref()).map_err(failure)?;
                let mut records:Vec<TaskRecord>=Vec::new();
                for outcome in reconciliation.outcomes { if let Some(record)=self.manager.get(&outcome.task_id) { records.push(record); } }
                if let Some(session)=&session { for entry in self.manager.list(&ListScope::ParentSession(session.clone())) { if let Some(record)=records.iter_mut().find(|record| record.task_id==entry.record.task_id) { *record=entry.record; } else { records.push(entry.record); } } }
                for record in &records { (self.notify_liveness)(record); }
                (self.resumption_start)().map_err(failure)?;
                if let Err(error)=(self.reconcile_mailbox)() { (self.on_warning)(format!("omo-senpi task session-start team mailbox reclaim failed: {error}")); }
                if let Some(session)=&session { let parent_state=self.runtime.lock().unwrap_or_else(PoisonError::into_inner).parent_state(); self.notifier.reconcile_unnotified_notifications(ReconcileUnnotifiedNotificationsInput { session_id:session,parent_state }).map_err(failure)?; }
                self.lifecycle.cleanup_expired_records().map_err(failure)?;
                if let Err(error)=self.lead_pollers.tick() { (self.on_warning)(format!("omo-senpi task session-start lead poll failed: {error}")); }
                self.status_ui.schedule_sync(); self.task_rpc.attach();
            },
            ExtensionEvent::SessionBeforeSwitch { .. } => { self.transitions.lock().unwrap_or_else(PoisonError::into_inner).mark(TransitionReason::SessionSwitching,session.as_deref()); self.runtime.lock().unwrap_or_else(PoisonError::into_inner).clear_ui(); },
            ExtensionEvent::SessionBeforeCompact(_) => self.transitions.lock().unwrap_or_else(PoisonError::into_inner).mark(TransitionReason::Compacting,session.as_deref()),
            ExtensionEvent::SessionCompact(_) => { self.transitions.lock().unwrap_or_else(PoisonError::into_inner).resolve(session.as_deref()).map_err(failure)?; self.status_ui.schedule_sync(); },
            ExtensionEvent::SessionShutdown(shutdown) => {
                self.transitions.lock().unwrap_or_else(PoisonError::into_inner).mark(TransitionReason::SessionShutdown,session.as_deref()); self.runtime.lock().unwrap_or_else(PoisonError::into_inner).clear_ui(); self.status_ui.dispose(); self.lead_pollers.shutdown(); (self.resumption_shutdown)().map_err(failure)?;
                if let Some(parent_session_id)=session { let reason=match shutdown.reason { SessionReason::Startup=>"startup",SessionReason::Reload=>"reload",SessionReason::New=>"new",SessionReason::Resume=>"resume",SessionReason::Fork=>"fork",SessionReason::Quit=>"quit" }; self.lifecycle.suspend_on_session_shutdown(&SuspendInput { parent_session_id,reason:reason.into() }).map_err(failure)?; }
            },
            ExtensionEvent::ModelSelect(_) => self.status_ui.schedule_sync(),
            ExtensionEvent::AgentEnd { .. } => (self.acknowledge_liveness)().map_err(failure)?,
            _ => {},
        }
        Ok(())
    }
}
