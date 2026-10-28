//! Hosted child handle (`rpc-host/handle.ts`).
use super::{
    handle_listeners::*, handle_port::*, handle_session_identity::*, handle_teardown::*,
    open_session::OpenedHostSession,
};
use crate::{
    host::HostError,
    manager::child_handle::{ManagedChildHandle, ManagedChildListener, Unsubscribe},
    runners::RunnerOutcome,
    state::TaskStatus,
};
use std::sync::{Arc, Mutex};
pub struct HostSessionHandle {
    task_id: String,
    port: Arc<Mutex<Arc<dyn HostSessionPort>>>,
    pub identity: HandleSessionIdentity,
    pub listeners: HandleListeners,
    teardown: Arc<HandleTeardown>,
    native_pause: CurrentPortPause,
    recovering: std::sync::atomic::AtomicBool,
    stopped: std::sync::atomic::AtomicBool,
    settled: std::sync::atomic::AtomicBool,
    event_subscription: Mutex<Option<Unsubscribe>>,
    outcome: Arc<dyn Fn() -> RunnerOutcome + Send + Sync>,
}
pub fn create_host_session_handle(
    task_id: String,
    port: Arc<dyn HostSessionPort>,
    opened: OpenedHostSession,
    outcome: Arc<dyn Fn() -> RunnerOutcome + Send + Sync>,
) -> Arc<HostSessionHandle> {
    let listeners = create_handle_listeners();
    let learned = listeners.clone();
    let identity = create_handle_session_identity(
        opened.session,
        opened.durable_session_id,
        Arc::new(move |id| learned.emit_durable_session_id(id)),
    );
    let port = Arc::new(Mutex::new(port));
    let teardown = create_handle_teardown(port.clone(), opened.attached);
    let handle = Arc::new(HostSessionHandle {
        native_pause: CurrentPortPause(port.clone()),
        task_id,
        port,
        identity,
        listeners,
        teardown: Arc::new(teardown),
        recovering: std::sync::atomic::AtomicBool::new(false),
        stopped: std::sync::atomic::AtomicBool::new(false),
        settled: std::sync::atomic::AtomicBool::new(opened.attached),
        event_subscription: Mutex::new(None),
        outcome,
    });
    handle.bind_events();
    handle
}
impl HostSessionHandle {
    pub fn stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn transport_lost(&self, message: String) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.current_port().transport_lost(message);
    }
    fn current_port(&self) -> Arc<dyn HostSessionPort> {
        self.port.lock().expect("hosted child state lock poisoned").clone()
    }
    fn bind_events(self: &Arc<Self>) {
        if let Some(unsubscribe) = self.event_subscription.lock().expect("hosted child state lock poisoned").take() {
            unsubscribe();
        }
        let weak = Arc::downgrade(self);
        let port = self.port.lock().expect("hosted child state lock poisoned").clone();
        let observed_port = port.clone();
        let subscription = port.on_event(Arc::new(move |event| {
            let Some(handle) = weak.upgrade() else {
                return;
            };
            if event.event_type == "agent_start" {
                let status_handle = Arc::downgrade(&handle);
                std::thread::spawn(move || {
                    if let Some(handle) = status_handle.upgrade()
                        && let Err(error) = handle.teardown.publish_running() {
                            eprintln!("host running publication failed: {error}");
                        }
                });
                let was_settled = handle
                    .settled
                    .swap(false, std::sync::atomic::Ordering::AcqRel);
                let claimed = observed_port.consume_turn_claim();
                if was_settled && !claimed {
                    handle.listeners.emit_self_resumed();
                }
            }
            handle.listeners.emit_event(event);
        }));
        *self.event_subscription.lock().expect("hosted child state lock poisoned") = Some(subscription);
    }
    pub fn begin_turn(&self, text: &str) -> Result<(), HostError> {
        self.settled
            .store(false, std::sync::atomic::Ordering::Release);
        self.teardown.publish_running()?;
        self.current_port()
            .send(serde_json::json!({"type":"prompt","message":text}))
    }
    pub fn recovery_ended(
        self: &Arc<Self>,
        port: Arc<dyn HostSessionPort>,
        identity: HostSessionIdentity,
    ) -> Result<(), HostError> {
        *self.port.lock().expect("hosted child state lock poisoned") = port;
        self.identity.adopt(identity);
        self.bind_events();
        self.teardown.republish_status()
    }
    pub fn recover(
        self: &Arc<Self>,
        input: Arc<super::reattach_port::ReattachPortInput>,
        socket: String,
        budget: std::time::Duration,
    ) -> Result<bool, HostError> {
        self.recovering
            .store(true, std::sync::atomic::Ordering::Release);
        let path = self.identity.current().session_path;
        let recovery = super::handle_recovery::HandleRecovery::default();
        let result = recovery.bounded_reattach(budget, move |abandoned| {
            super::reattach_port::create_reattach_port(&input, &socket, &path, abandoned.as_ref())
        });
        self.recovering
            .store(false, std::sync::atomic::Ordering::Release);
        if self.stopped() {
            if let Some(Ok(Some((port, _)))) = result {
                port.detach()?;
            }
            return Ok(false);
        }
        match result {
            Some(Ok(Some((port, identity)))) => {
                self.recovery_ended(port, identity)?;
                Ok(true)
            }
            Some(Err(message)) => Err(HostError { message }),
            Some(Ok(None)) | None => Ok(false),
        }
    }
    pub fn heartbeat(&self) -> Result<(), HostError> {
        let state = self.current_port().get_state()?;
        self.identity.observe_durable_id(&state["sessionId"]);
        Ok(())
    }
    pub fn start_initial_prompt(&self, text: &str) -> Result<(), HostError> {
        self.begin_turn(text)
    }
}
impl ManagedChildHandle for HostSessionHandle {
    fn transport_recovering(&self) -> bool {
        self.recovering.load(std::sync::atomic::Ordering::Acquire)
    }
    fn mark_stopping(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
    }
    fn on_self_resumed(&self, listener: Arc<dyn Fn() + Send + Sync>) -> Option<Unsubscribe> {
        Some(self.listeners.on_self_resumed(listener))
    }
    fn run_pause(&self) -> Option<&dyn crate::pause::native_child_pause::NativeRunPause> {
        Some(&self.native_pause)
    }
    fn on_durable_session_id(
        &self,
        listener: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Option<Unsubscribe> {
        Some(self.listeners.on_durable_session_id(listener))
    }
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn session_id(&self) -> Option<String> {
        self.identity.durable_id()
    }
    fn pid(&self) -> Option<i64> {
        None
    }
    fn steer(&self, text: &str) -> Result<(), HostError> {
        self.current_port()
            .send(serde_json::json!({"type":"steer","message":text}))
    }
    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        self.teardown.publish_running()?;
        self.current_port()
            .send(serde_json::json!({"type":"followUp","message":text}))
    }
    fn abort(&self) -> Result<(), HostError> {
        self.current_port()
            .send(serde_json::json!({"type":"abort"}))
    }
    fn subscribe(&self, listener: ManagedChildListener) -> Unsubscribe {
        self.listeners.on_event(listener)
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        let outcome = self
            .current_port()
            .wait_for_outcome()
            .unwrap_or_else(|| (self.outcome)());
        self.settled
            .store(true, std::sync::atomic::Ordering::Release);
        outcome
    }
    fn last_assistant_text(&self) -> Option<String> {
        self.current_port()
            .get_state()
            .ok()
            .and_then(|state| state["lastAssistantText"].as_str().map(str::to_string))
    }
    fn dispose(&self) -> Result<(), HostError> {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.teardown.detach()
    }
    fn terminate(&self) -> Result<(), HostError> {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.teardown.terminate()
    }
    fn has_terminate(&self) -> bool {
        true
    }
    fn is_host_session(&self) -> bool {
        true
    }
    fn publish_task_status(&self, status: TaskStatus) -> Result<(), HostError> {
        self.teardown.publish_task_status(status)
    }
}

impl Drop for HostSessionHandle {
    fn drop(&mut self) {
        if let Some(unsubscribe) = self.event_subscription.get_mut().expect("validated recovery state is present").take() {
            unsubscribe();
        }
    }
}

struct CurrentPortPause(Arc<Mutex<Arc<dyn HostSessionPort>>>);
impl CurrentPortPause {
    fn current(
        &self,
    ) -> Result<
        Arc<dyn crate::pause::native_child_pause::NativeRunPause>,
        crate::pause::native_child_pause::NativePauseError,
    > {
        self.0.lock().expect("hosted child state lock poisoned").native_pause().ok_or_else(|| {
            crate::pause::native_child_pause::NativePauseError::Engine(
                "native_pause_unavailable".into(),
            )
        })
    }
}
impl crate::pause::native_child_pause::NativeRunPause for CurrentPortPause {
    fn pause_for_run(
        &self,
        owner: crate::pause::ports::RunPauseOwner,
    ) -> Result<
        crate::pause::ports::EngineInheritReceipt,
        crate::pause::native_child_pause::NativePauseError,
    > {
        self.current()?.pause_for_run(owner)
    }
    fn resume_for_run(
        &self,
        request: crate::pause::ports::ResumeTurnRequest,
    ) -> Result<
        crate::pause::native_child_pause::NativeResumeResult,
        crate::pause::native_child_pause::NativePauseError,
    > {
        self.current()?.resume_for_run(request)
    }
    fn hold_input(
        &self,
        parent: &str,
        input: crate::pause::ports::HoldInput,
    ) -> Result<
        crate::pause::ports::EngineHoldInputResult,
        crate::pause::native_child_pause::NativePauseError,
    > {
        self.current()?.hold_input(parent, input)
    }
    fn observe_hold(
        &self,
        parent: &str,
    ) -> Result<
        Option<crate::pause::ports::ObservedHold>,
        crate::pause::native_child_pause::NativePauseError,
    > {
        self.current()?.observe_hold(parent)
    }
}
