//! Hosted child handle (`rpc-host/handle.ts`).
use super::{
    handle_listeners::*, handle_port::*, handle_session_identity::*, handle_teardown::*,
    open_session::OpenedHostSession,
};
use crate::{
    host::HostError,
    manager::child_handle::{InputIdentity, ManagedChildHandle, ManagedChildListener, Unsubscribe},
    runners::RunnerOutcome,
    runners::input_admission_read::read_host_admissions,
    runners::input_effects::{
        IdentifiedInput, InputCommand, InputCommandType, InputEffects, InputEffectsOptions,
        InputWriteError, create_input_effects,
    },
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
    input_effects: InputEffects,
    /// `startHostHeartbeat`: this session's liveness (probe failure is never proof of death).
    liveness: Arc<super::handle_heartbeat::HostHeartbeat>,
    unresponsive_close: Arc<super::unresponsive_close::UnresponsiveClose>,
    pending_probe: Mutex<Option<std::sync::mpsc::Receiver<Result<serde_json::Value, HostError>>>>,
    pending_close: Mutex<Option<std::sync::mpsc::Receiver<Result<(), HostError>>>>,
    /// The outcome a liveness ending settles with (confirmed unresponsive close).
    forced_outcome: Mutex<Option<RunnerOutcome>>,
    outcome_changed: Arc<(Mutex<u64>, std::sync::Condvar)>,
}
/// The default probe cadence (`DEFAULT_HEARTBEAT_INTERVAL_MS`, `session-open.ts`).
pub const HOST_HEARTBEAT_INTERVAL_MS: i64 = 10_000;
/// The close grace of a hosted session (`DEFAULT_CLOSE_GRACE_MS`).
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
const UNRESPONSIVE_CLOSED: &str = "unresponsive, closed: the host confirmed this session is absent";
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
pub fn create_host_session_handle(
    task_id: String,
    port: Arc<dyn HostSessionPort>,
    opened: OpenedHostSession,
    outcome: Arc<dyn Fn() -> RunnerOutcome + Send + Sync>,
) -> Arc<HostSessionHandle> {
    create_host_session_handle_with_inputs(task_id, port, opened, outcome, None)
}
/// `createHostSessionHandle` with `inputStore`: identified inputs persist in the task store.
pub fn create_host_session_handle_with_inputs(
    task_id: String,
    port: Arc<dyn HostSessionPort>,
    opened: OpenedHostSession,
    outcome: Arc<dyn Fn() -> RunnerOutcome + Send + Sync>,
    input_store: Option<crate::store::TaskRecordStore>,
) -> Arc<HostSessionHandle> {
    let listeners = create_handle_listeners();
    let learned = listeners.clone();
    let identity = create_handle_session_identity(
        opened.session,
        opened.durable_session_id,
        Arc::new(move |id| learned.emit_durable_session_id(id)),
    );
    let port = Arc::new(Mutex::new(port));
    let send_port = port.clone();
    let read_port = port.clone();
    let input_effects = create_input_effects(InputEffectsOptions {
        task_id: task_id.clone(),
        store: input_store,
        send: Arc::new(move |command: &IdentifiedInput| {
            let current = send_port
                .lock()
                .expect("hosted child state lock poisoned")
                .clone();
            current
                .send(to_host_input(command))
                .map_err(|error| InputWriteError::from_message(None, error.message))
        }),
        read: Some(Arc::new(move |ids: &[String]| {
            let current = read_port
                .lock()
                .expect("hosted child state lock poisoned")
                .clone();
            read_host_admissions(current.as_ref(), ids)
        })),
    });
    let teardown = create_handle_teardown(port.clone(), opened.attached);
    let liveness = Arc::new(super::handle_heartbeat::HostHeartbeat::new(
        task_id.clone(),
        HOST_HEARTBEAT_INTERVAL_MS,
        now_ms(),
    ));
    let close_port = port.clone();
    let session_handle: Arc<std::sync::OnceLock<std::sync::Weak<HostSessionHandle>>> =
        Arc::new(std::sync::OnceLock::new());
    let session_lookup = Arc::clone(&session_handle);
    let unresponsive_close = Arc::new(super::unresponsive_close::create_unresponsive_close(
        super::unresponsive_close::UnresponsiveCloseInput {
            task_id: task_id.clone(),
            close_grace: CLOSE_GRACE,
            // SAFETY-DIVERGENCE(omo rpc-host/unresponsive-close.ts): upstream reattaches through
            // the handle's reattach seam before closing; the Rust reattach input lives in the
            // assembly (maho-cli task_host), so a disconnected close is refused and retried.
            reattach: None,
            port: Arc::new(move || {
                close_port
                    .lock()
                    .expect("hosted child state lock poisoned")
                    .clone()
            }),
            session: Arc::new(move || {
                session_lookup
                    .get()
                    .and_then(std::sync::Weak::upgrade)
                    .map(|handle| handle.identity.current())
                    .unwrap_or_else(|| HostSessionIdentity {
                        routing_id: String::new(),
                        session_path: String::new(),
                        instance_id: String::new(),
                    })
            }),
            stopped: Arc::new(|| false),
            adopt: Arc::new(|_, _| {}),
        },
    ));
    let handle = Arc::new(HostSessionHandle {
        liveness,
        unresponsive_close,
        pending_probe: Mutex::new(None),
        pending_close: Mutex::new(None),
        forced_outcome: Mutex::new(None),
        outcome_changed: Arc::new((Mutex::new(0), std::sync::Condvar::new())),
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
        input_effects,
    });
    let _ = session_handle.set(Arc::downgrade(&handle));
    handle.bind_events();
    handle
}
fn log_failure(message: &str, task_id: &str, error: &str) {
    utils::logger::log(
        message,
        Some(&serde_json::json!({ "taskId": task_id, "error": error })),
    );
}
impl HostSessionHandle {
    pub fn stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn transport_lost(&self, message: String) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.wake_outcome_waiters();
        self.current_port().transport_lost(message);
    }
    fn current_port(&self) -> Arc<dyn HostSessionPort> {
        self.port
            .lock()
            .expect("hosted child state lock poisoned")
            .clone()
    }
    fn bind_events(self: &Arc<Self>) {
        if let Some(unsubscribe) = self
            .event_subscription
            .lock()
            .expect("hosted child state lock poisoned")
            .take()
        {
            unsubscribe();
        }
        let weak = Arc::downgrade(self);
        let port = self
            .port
            .lock()
            .expect("hosted child state lock poisoned")
            .clone();
        let observed_port = port.clone();
        let subscription = port.on_event(Arc::new(move |event| {
            let Some(handle) = weak.upgrade() else {
                return;
            };
            if matches!(
                event.event_type.as_str(),
                "session_closed" | "session_parked"
            ) {
                handle.session_frame(event);
                return;
            }
            handle
                .liveness
                .step(|machine, changes| machine.activity(now_ms(), changes));
            if event.event_type == "agent_start" {
                let status_handle = Arc::downgrade(&handle);
                std::thread::spawn(move || {
                    if let Some(handle) = status_handle.upgrade()
                        && let Err(error) = handle.teardown.publish_running()
                    {
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
        *self
            .event_subscription
            .lock()
            .expect("hosted child state lock poisoned") = Some(subscription);
    }
    pub fn begin_turn(&self, text: &str) -> Result<(), HostError> {
        self.settled
            .store(false, std::sync::atomic::Ordering::Release);
        self.teardown.publish_running()?;
        self.send_input(InputCommandType::Prompt, text, None)
    }
    /// `inputEffects.send`: every input goes out under its durable id (minted or the caller's own).
    fn send_input(
        &self,
        kind: InputCommandType,
        text: &str,
        client_message_id: Option<&str>,
    ) -> Result<(), HostError> {
        self.input_effects
            .send(InputCommand {
                kind,
                message: text.to_string(),
                streaming_behavior: None,
                client_message_id: client_message_id.map(str::to_string),
            })
            .map_err(|error| HostError {
                message: error.to_string(),
            })
    }
    /// Reconciles inputs whose acknowledgement was lost (`reconcileInputs`).
    pub fn reconcile_inputs(&self) {
        self.input_effects.reconcile();
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
    /// One liveness step (`startHostHeartbeat`), driven by the assembly's recovery loop.
    /// `Err` only for a probe the running session refused at once (a lost connection, which the
    /// caller recovers); a missed deadline counts toward `unresponsive`, and a terminal-pending
    /// session is closed with host-confirmed absence instead of being declared crashed.
    pub fn heartbeat(&self) -> Result<(), HostError> {
        self.liveness_step_at(now_ms())
    }
    pub(crate) fn liveness_step_at(&self, now: i64) -> Result<(), HostError> {
        self.liveness_step_with_deadline(
            now,
            std::time::Duration::from_millis(
                crate::runners::rpc::heartbeat::HEARTBEAT_DEADLINE_MS as u64,
            ),
        )
    }
    pub(crate) fn liveness_step_with_deadline(
        &self,
        now: i64,
        deadline: std::time::Duration,
    ) -> Result<(), HostError> {
        use super::handle_heartbeat::HeartbeatStep;
        use super::session_wire::is_host_session_missing;
        if self.stopped() {
            return Ok(());
        }
        self.collect_close();
        let mut refused = None;
        let step = self
            .liveness
            .step(|machine, changes| machine.tick(now, changes));
        match step {
            HeartbeatStep::Probe => {
                let (answer_tx, answer_rx) = std::sync::mpsc::channel();
                let running = self.liveness.status().is_running();
                let port = self.current_port();
                let close = Arc::clone(&self.unresponsive_close);
                std::thread::spawn(move || {
                    let answer = if running {
                        port.get_state()
                    } else {
                        close.probe().and_then(|probe| match probe {
                            super::unresponsive_close::UnresponsiveProbe::State(state) => Ok(state),
                            super::unresponsive_close::UnresponsiveProbe::Missing => {
                                Err(HostError {
                                    message: "unknown_session".into(),
                                })
                            }
                        })
                    };
                    let _ = answer_tx.send(answer);
                });
                *self
                    .pending_probe
                    .lock()
                    .expect("hosted child state lock poisoned") = Some(answer_rx);
            }
            HeartbeatStep::Close => self.start_close(),
            HeartbeatStep::Gone => {
                self.session_gone();
                return Ok(());
            }
            HeartbeatStep::Idle => {}
        }
        let pending = self
            .pending_probe
            .lock()
            .expect("hosted child state lock poisoned")
            .take();
        if let Some(answer_rx) = pending {
            match answer_rx.recv_timeout(deadline) {
                Ok(Ok(state)) => {
                    self.identity.observe_durable_id(&state["sessionId"]);
                    self.liveness
                        .step(|machine, changes| machine.answered(now, changes));
                }
                Ok(Err(error)) => {
                    let running = self.liveness.status().is_running();
                    let missing = is_host_session_missing(None, &error.message);
                    let next = self
                        .liveness
                        .step(|machine, changes| machine.refused(now, missing, changes));
                    if next == HeartbeatStep::Gone {
                        self.session_gone();
                        return Ok(());
                    }
                    if running {
                        refused = Some(error);
                    }
                }
                // Still in flight: the machine counts the deadline; the probe stays single-flight.
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    *self
                        .pending_probe
                        .lock()
                        .expect("hosted child state lock poisoned") = Some(answer_rx);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
            }
        }
        refused.map_or(Ok(()), Err)
    }
    fn start_close(&self) {
        let mut pending = self
            .pending_close
            .lock()
            .expect("hosted child state lock poisoned");
        if pending.is_some() {
            return;
        }
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let close = Arc::clone(&self.unresponsive_close);
        std::thread::spawn(move || {
            let _ = done_tx.send(close.close());
        });
        *pending = Some(done_rx);
    }
    fn collect_close(&self) {
        let mut pending = self
            .pending_close
            .lock()
            .expect("hosted child state lock poisoned");
        let Some(done_rx) = pending.as_ref() else {
            return;
        };
        match done_rx.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => *pending = None,
            Ok(Err(error)) => {
                *pending = None;
                log_failure(
                    "senpi-task unresponsive session close pending",
                    &self.task_id,
                    &error.message,
                );
            }
            Ok(Ok(())) => {
                *pending = None;
                drop(pending);
                self.closed_unresponsive();
            }
        }
    }
    /// Blocks until the in-flight confirmed close answers, then applies it (tests only).
    #[cfg(test)]
    pub(crate) fn settle_close_for_tests(&self) {
        let pending = self
            .pending_close
            .lock()
            .expect("hosted child state lock poisoned")
            .take();
        let Some(done_rx) = pending else {
            return;
        };
        let answer = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("close answered");
        let (done_tx, replay) = std::sync::mpsc::channel();
        let _ = done_tx.send(answer);
        *self
            .pending_close
            .lock()
            .expect("hosted child state lock poisoned") = Some(replay);
        self.collect_close();
    }
    /// `onGone(true)`: the host confirmed this session is absent.
    fn closed_unresponsive(&self) {
        self.liveness.stop();
        self.unresponsive_close.stop();
        *self
            .forced_outcome
            .lock()
            .expect("hosted child state lock poisoned") = Some(RunnerOutcome::error(
            crate::runners::RunnerFailureKind::ChildUnresponsive,
            UNRESPONSIVE_CLOSED,
        ));
        let (generation, changed) = &*self.outcome_changed;
        *generation.lock().expect("outcome generation lock poisoned") += 1;
        changed.notify_all();
        self.transport_lost(UNRESPONSIVE_CLOSED.to_string());
    }
    /// `onGone(false)`: a live host answered `unknown_session`; the session ended and is detached.
    fn session_gone(&self) {
        self.session_ended("unknown_session");
    }
    /// `endSession({kind: "session_closed", reason})` plus `releaseEndedSession`: the session
    /// ended on the host, so this child exits and its connection is detached.
    fn session_ended(&self, reason: &str) {
        self.liveness.stop();
        self.unresponsive_close.stop();
        self.transport_lost(format!("host session closed: {reason}"));
        if let Err(error) = self.current_port().detach() {
            log_failure(
                "senpi-task absent session detach failed",
                &self.task_id,
                &error.message,
            );
        }
    }
    /// `clientBinding.onClosed` / `onParked`: a lifecycle frame is a control record, never
    /// activity. While the confirmed close runs (`closingUnresponsive`) it is a hint, not proof:
    /// only the host's post-close listing ends the session. Otherwise a `session_closed` frame
    /// ends this child's session (`endSession`).
    fn session_frame(&self, event: &crate::manager::child_handle::ManagedChildEvent) {
        if self.stopped()
            || matches!(
                self.liveness.status(),
                crate::state::host_liveness::HostLivenessStatus::TerminalPending { .. }
            )
        {
            return;
        }
        // SAFETY-DIVERGENCE(omo rpc-host/handle.ts park): a parked session is not an exit, but the
        // Rust manager has no parked-session listener yet, so a parking frame is ignored here and
        // the record is not parked; the session most likely ends later through the heartbeat's
        // unknown_session path.
        if event.event_type == "session_parked"
            || matches!(
                event.reason.as_deref(),
                Some("handoff_parked" | "idle_evicted")
            )
        {
            return;
        }
        self.session_ended(event.reason.as_deref().unwrap_or("session_closed"));
    }
    pub fn start_initial_prompt(&self, text: &str) -> Result<(), HostError> {
        self.begin_turn(text)
    }
    /// `HostSessionUnresponsiveError`: nothing is sent while the session is not running.
    fn require_running(&self) -> Result<(), HostError> {
        if self.liveness.status().is_running() {
            return Ok(());
        }
        Err(super::session_wire::host_session_unresponsive_error())
    }
    /// `stopHeartbeat`: the heartbeat, the close path and the input retries stop together.
    fn stop_liveness(&self) {
        self.liveness.stop();
        self.unresponsive_close.stop();
        self.input_effects.stop_retrying();
    }
}
impl ManagedChildHandle for HostSessionHandle {
    fn reconcile_inputs(&self) {
        HostSessionHandle::reconcile_inputs(self);
    }
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
        self.require_running()?;
        self.send_input(InputCommandType::Steer, text, None)
    }
    fn steer_with_identity(&self, text: &str, identity: &InputIdentity) -> Result<(), HostError> {
        self.require_running()?;
        self.send_input(
            InputCommandType::Steer,
            text,
            Some(&identity.client_message_id),
        )
    }
    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        self.require_running()?;
        self.teardown.publish_running()?;
        self.send_input(InputCommandType::FollowUp, text, None)
    }
    fn follow_up_with_identity(
        &self,
        text: &str,
        identity: &InputIdentity,
    ) -> Result<(), HostError> {
        self.require_running()?;
        self.teardown.publish_running()?;
        self.send_input(
            InputCommandType::FollowUp,
            text,
            Some(&identity.client_message_id),
        )
    }
    fn liveness(&self) -> Option<crate::state::host_liveness::HostLivenessStatus> {
        Some(self.liveness.status())
    }
    fn on_liveness(
        &self,
        listener: Arc<dyn Fn(crate::state::host_liveness::HostLivenessStatus) + Send + Sync>,
    ) -> Option<Unsubscribe> {
        Some(self.liveness.subscribe(listener))
    }
    fn abort(&self) -> Result<(), HostError> {
        self.current_port()
            .send(serde_json::json!({"type":"abort"}))
    }
    fn subscribe(&self, listener: ManagedChildListener) -> Unsubscribe {
        self.listeners.on_event(listener)
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        if self
            .forced_outcome
            .lock()
            .expect("hosted child state lock poisoned")
            .is_none()
            && matches!(
                self.liveness.status(),
                crate::state::host_liveness::HostLivenessStatus::TerminalPending { .. }
            )
        {
            let (generation, changed) = &*self.outcome_changed;
            let mut generation = generation.lock().expect("outcome generation lock poisoned");
            let observed = *generation;
            while observed == *generation
                && !self.stopped()
                && self
                    .forced_outcome
                    .lock()
                    .expect("hosted child state lock poisoned")
                    .is_none()
                && matches!(
                    self.liveness.status(),
                    crate::state::host_liveness::HostLivenessStatus::TerminalPending { .. }
                )
            {
                generation = changed
                    .wait(generation)
                    .expect("outcome generation lock poisoned");
            }
        }
        let forced = self
            .forced_outcome
            .lock()
            .expect("hosted child state lock poisoned")
            .clone();
        let outcome = forced.unwrap_or_else(|| {
            self.current_port()
                .wait_for_outcome()
                .unwrap_or_else(|| (self.outcome)())
        });
        let outcome = self
            .forced_outcome
            .lock()
            .expect("hosted child state lock poisoned")
            .clone()
            .unwrap_or(outcome);
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
        self.wake_outcome_waiters();
        self.stop_liveness();
        self.teardown.detach()
    }
    fn terminate(&self) -> Result<(), HostError> {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.wake_outcome_waiters();
        self.stop_liveness();
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

impl HostSessionHandle {
    fn wake_outcome_waiters(&self) {
        let (generation, changed) = &*self.outcome_changed;
        *generation.lock().expect("outcome generation lock poisoned") += 1;
        changed.notify_all();
    }
}

/// `toHostInput` (`rpc-host/input-command.ts`): the identified input as a host session command.
pub fn to_host_input(command: &IdentifiedInput) -> serde_json::Value {
    let mut wire = command.wire();
    if command.kind == InputCommandType::FollowUp {
        wire["type"] = serde_json::json!("followUp");
    }
    wire
}

impl Drop for HostSessionHandle {
    fn drop(&mut self) {
        if let Some(unsubscribe) = self
            .event_subscription
            .get_mut()
            .expect("validated recovery state is present")
            .take()
        {
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
        self.0
            .lock()
            .expect("hosted child state lock poisoned")
            .native_pause()
            .ok_or_else(|| {
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

#[cfg(test)]
#[path = "handle_liveness_tests.rs"]
mod liveness_tests;
