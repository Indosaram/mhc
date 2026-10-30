//! `runners/rpc/handle.ts`: the steerable handle over one RPC child, tracking each turn's outcome.

pub type ResumedListenersCallback = Mutex<Vec<(u64, Arc<dyn Fn() + Send + Sync>)>>;
use std::borrow::Cow;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::host::HostError;
use crate::manager::child_handle::{
    InputIdentity, ManagedChildEvent, ManagedChildHandle, ManagedChildListener, Unsubscribe,
};
use crate::runners::RunnerOutcome;
use crate::runners::input_admission_read::read_stdio_admissions;
use crate::runners::input_effects::{
    IdentifiedInput, InputCommand, InputCommandType, InputEffectError, InputEffects,
    InputEffectsOptions, InputWriteError, create_input_effects,
};
use crate::runners::rpc::errors::RpcCommandError;
use crate::runners::rpc::exit_mapping::classify_child_exit;
use crate::runners::rpc::heartbeat::{
    HEARTBEAT_DEADLINE_MS, HeartbeatMachine, MissVerdict, unresponsive_message,
};
use crate::runners::rpc::native_run_pause::RpcRunPause;
use crate::runners::rpc::protocol_client::{RpcClientError, RpcClientPort};
use crate::runners::rpc::turn_claim::{TurnClaim, create_turn_claim};
use crate::runners::rpc::turn_outcome::{
    agent_end_outcome, exit_turn_outcome, extract_assistant_text, prompt_failure_outcome,
};
use crate::runners::task_parent_control::{
    TaskParentControlError, TaskParentWire, create_task_parent_control,
};
use crate::runners::types::{
    ChildExitOutcome, RpcEntriesResult, RpcSpawnSpec, RpcSwitchSessionResult,
    RpcTerminalAssistantMessage, TerminateOptions,
};

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct RpcChildHandleOptions {
    pub client: Arc<dyn RpcClientPort>,
    pub task_id: String,
    pub heartbeat_interval_ms: u64,
    pub now: Clock,
    /// `inputStore`: where identified inputs and uncertain deliveries are persisted.
    pub input_store: Option<crate::store::TaskRecordStore>,
}

#[derive(Default)]
struct HandleState {
    reached_idle: bool,
    session_id: Option<String>,
    final_text: Option<String>,
    turn_baseline: Option<String>,
    turn_outcome: Option<RunnerOutcome>,
    turn_generation: u64,
    settled_turns: std::collections::HashMap<u64, RunnerOutcome>,
    terminal_assistant_message: Option<RpcTerminalAssistantMessage>,
    /// The raw terminal assistant `message_end` payload of the current turn.
    terminal_wire_message: Option<Value>,
    aborted_by_user: bool,
    last_seen_at: Option<i64>,
    exit: Option<ChildExitOutcome>,
    disposed: bool,
}

struct HandleShared {
    /// `startHeartbeat`'s miss accounting; events feed its `activity()`.
    heartbeat: Mutex<HeartbeatMachine>,
    clock: Clock,
    claims: TurnClaim,
    resumed_listeners: ResumedListenersCallback,
    next_listener: std::sync::atomic::AtomicU64,
    state: Mutex<HandleState>,
    changed: Condvar,
}

impl HandleShared {
    fn lock(&self) -> MutexGuard<'_, HandleState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn live(&self) -> bool {
        let state = self.lock();
        !state.disposed && state.exit.is_none()
    }

    fn settle_turn(&self, state: &mut HandleState, settled: RunnerOutcome) {
        if state.turn_outcome.is_some() {
            return;
        }
        let generation = state.turn_generation;
        state.settled_turns.insert(generation, settled.clone());
        state.turn_outcome = Some(settled);
        state.reached_idle = true;
        self.changed.notify_all();
    }

    fn on_event(&self, event: &Value) {
        self.heartbeat
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .activity((self.clock)());
        let mut state = self.lock();
        if event["type"] == "agent_start" && state.turn_outcome.is_some() && state.exit.is_none() {
            state.turn_generation += 1;
            state.reached_idle = false;
            state.turn_outcome = None;
            state.terminal_assistant_message = None;
            state.terminal_wire_message = None;
            state.aborted_by_user = false;
            state.turn_baseline = state.final_text.clone();
            let claimed = self.claims.consume();
            drop(state);
            if !claimed {
                let listeners: Vec<_> = self
                    .resumed_listeners
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .iter()
                    .map(|(_, listener)| Arc::clone(listener))
                    .collect();
                for listener in listeners {
                    listener();
                }
            }
            state = self.lock();
        }
        match event.get("type").and_then(Value::as_str) {
            Some("message_end") => {
                if let Some(terminal) = event.get("message").and_then(terminal_message) {
                    if terminal.text.is_some() {
                        state.final_text.clone_from(&terminal.text);
                    }
                    state.terminal_assistant_message = Some(terminal);
                    state.terminal_wire_message = event.get("message").cloned();
                }
            }
            Some("agent_end") if event.get("willRetry").and_then(Value::as_bool) == Some(false) => {
                let settled = if state.aborted_by_user {
                    RunnerOutcome::Cancelled
                } else {
                    agent_end_outcome(
                        &with_terminal_fallback(event, state.terminal_wire_message.as_ref()),
                        state.turn_baseline.as_deref(),
                        state.final_text.as_deref(),
                    )
                };
                self.settle_turn(&mut state, settled);
            }
            _ => {}
        }
    }

    fn on_exit(&self, built: ChildExitOutcome) {
        let mut state = self.lock();
        if state.exit.is_some() {
            return;
        }
        if state.turn_outcome.is_none() {
            let settled = exit_turn_outcome(&built, state.final_text.as_deref());
            self.settle_turn(&mut state, settled);
        }
        state.exit = Some(built);
        self.changed.notify_all();
    }
}

/// The RPC child handle. The runner attaches the spawn facts after a successful start.
pub struct RpcChildHandle {
    control: Arc<crate::runners::task_parent_control::TaskParentControl>,
    task_id: String,
    client: Arc<dyn RpcClientPort>,
    shared: Arc<HandleShared>,
    spawn_spec: Mutex<Option<RpcSpawnSpec>>,
    resume: Mutex<Option<(String, RpcSwitchSessionResult)>>,
    unsubscribe: Mutex<Vec<Unsubscribe>>,
    input_effects: InputEffects,
}

impl RpcChildHandle {
    pub fn new(options: RpcChildHandleOptions) -> Arc<Self> {
        let shared = Arc::new(HandleShared {
            heartbeat: Mutex::new(HeartbeatMachine::default()),
            clock: Arc::clone(&options.now),
            claims: create_turn_claim(),
            resumed_listeners: Mutex::new(Vec::new()),
            next_listener: std::sync::atomic::AtomicU64::new(0),
            state: Mutex::new(HandleState::default()),
            changed: Condvar::new(),
        });
        let client = options.client;
        let event_shared = Arc::downgrade(&shared);
        let exit_shared = Arc::downgrade(&shared);
        let unsubscribe_event = client.on_event(Arc::new(move |event| {
            if let Some(shared) = event_shared.upgrade() {
                shared.on_event(event);
            }
        }));
        let unsubscribe_exit = client.on_exit(Arc::new(move |exit| {
            if let Some(shared) = exit_shared.upgrade() {
                shared.on_exit(classify_child_exit(exit));
            }
        }));
        spawn_heartbeat(
            Arc::downgrade(&shared),
            Arc::clone(&client),
            options.task_id.clone(),
            Duration::from_millis(options.heartbeat_interval_ms),
            options.now,
        );
        let wire_client = Arc::clone(&client);
        let control = Arc::new(create_task_parent_control(TaskParentWire::new(
            80,
            Arc::new(move |command| {
                wire_client
                    .send_task_parent(command)
                    .map_err(|error| TaskParentControlError::new(&error.to_string()))
            }),
        )));
        let send_client = Arc::clone(&client);
        let read_client = Arc::clone(&client);
        let input_effects = create_input_effects(InputEffectsOptions {
            task_id: options.task_id.clone(),
            store: options.input_store,
            send: Arc::new(move |command: &IdentifiedInput| {
                write_command(send_client.as_ref(), command.wire(), command.kind.wire())
                    .map_err(input_write_error)
            }),
            read: Some(Arc::new(move |ids: &[String]| {
                read_stdio_admissions(read_client.as_ref(), ids)
            })),
        });
        Arc::new(Self {
            control,
            input_effects,
            task_id: options.task_id,
            client,
            shared,
            spawn_spec: Mutex::new(None),
            resume: Mutex::new(None),
            unsubscribe: Mutex::new(vec![unsubscribe_event, unsubscribe_exit]),
        })
    }

    pub fn run_pause(&self) -> RpcRunPause {
        let shared = Arc::clone(&self.shared);
        let expected_turn = {
            let state = shared.lock();
            state.turn_generation + u64::from(state.turn_outcome.is_some())
        };
        RpcRunPause {
            task_id: self.task_id.clone(),
            control: Arc::clone(&self.control),
            claims: self.shared.claims.clone(),
            current_outcome: Arc::new(move || {
                let mut state = shared.lock();
                loop {
                    if let Some(outcome) = state.settled_turns.get(&expected_turn) {
                        return outcome.clone();
                    }
                    if let Some(exit) = &state.exit {
                        return exit_turn_outcome(exit, state.final_text.as_deref());
                    }
                    state = shared
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }),
        }
    }

    pub fn on_self_resumed(&self, listener: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        let id = self
            .shared
            .next_listener
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.shared
            .resumed_listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((id, listener));
        let shared = Arc::downgrade(&self.shared);
        Box::new(move || {
            if let Some(shared) = shared.upgrade() {
                shared
                    .resumed_listeners
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .retain(|(key, _)| *key != id);
            }
        })
    }

    pub(crate) fn attach_start_facts(
        &self,
        spawn_spec: RpcSpawnSpec,
        resume: Option<(String, RpcSwitchSessionResult)>,
    ) {
        *self
            .spawn_spec
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(spawn_spec);
        *self.resume.lock().unwrap_or_else(PoisonError::into_inner) = resume;
    }

    fn run_command(&self, command: Value, label: &str) -> Result<(), RpcClientError> {
        write_command(self.client.as_ref(), command, label)
    }

    /// `inputEffects.send`: the input goes out under its durable id (minted or the caller's own).
    fn send_input(
        &self,
        kind: InputCommandType,
        text: &str,
        streaming_behavior: Option<&str>,
        client_message_id: Option<&str>,
    ) -> Result<(), RpcClientError> {
        // The refused response keeps its typed fields (a credential refusal drives the start chain).
        let refused = Mutex::new(None);
        let command = InputCommand {
            kind,
            message: text.to_string(),
            streaming_behavior: streaming_behavior.map(str::to_string),
            client_message_id: client_message_id.map(str::to_string),
        };
        self.input_effects
            .send_with(command, |identified| {
                write_command(
                    self.client.as_ref(),
                    identified.wire(),
                    identified.kind.wire(),
                )
                .map_err(|error| {
                    let classified = input_write_error(error.clone());
                    *refused.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                    classified
                })
            })
            .map_err(|error| match error {
                InputEffectError::Failed(message) => refused
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                    .unwrap_or(RpcClientError::Write { message }),
                other => RpcClientError::Write {
                    message: other.to_string(),
                },
            })
    }

    /// `delivery.prompt`: a busy refusal admitted nothing, so its followUp fallback keeps the id.
    fn deliver_prompt(
        &self,
        text: &str,
        follow_up: bool,
        client_message_id: Option<&str>,
    ) -> Result<(), RpcClientError> {
        let behavior = follow_up.then_some("followUp");
        let result = self.send_input(InputCommandType::Prompt, text, behavior, client_message_id);
        match result {
            Err(error) if !follow_up && is_busy_child_rejection(&error) => self.send_input(
                InputCommandType::Prompt,
                text,
                Some("followUp"),
                client_message_id,
            ),
            other => other,
        }
    }

    /// Reconciles inputs whose acknowledgement was lost (`reconcileInputs`).
    pub fn reconcile_inputs(&self) {
        self.input_effects.reconcile();
    }

    fn begin_turn(&self) {
        let mut state = self.shared.lock();
        if state.exit.is_some() {
            return;
        }
        if state.reached_idle || state.turn_outcome.is_some() {
            state.reached_idle = false;
            if state.turn_outcome.is_some() {
                state.turn_generation += 1;
            }
            state.turn_outcome = None;
        }
        state.terminal_assistant_message = None;
        state.terminal_wire_message = None;
        state.aborted_by_user = false;
        state.turn_baseline = state.final_text.clone();
    }

    fn run_prompt(
        &self,
        text: &str,
        follow_up: bool,
        client_message_id: Option<&str>,
    ) -> Result<(), RpcClientError> {
        self.begin_turn();
        self.deliver_prompt(text, follow_up, client_message_id)
            .inspect_err(|error| {
                // An uncertain input may still be delivered: its turn is not failed.
                if is_uncertain_input(error) {
                    return;
                }
                let mut state = self.shared.lock();
                self.shared
                    .settle_turn(&mut state, prompt_failure_outcome(&error.to_string()));
            })
    }

    pub fn start_initial_prompt(&self, text: &str) -> Result<(), RpcClientError> {
        self.run_prompt(text, false, None)
    }

    pub fn steer_turn(&self, text: &str) -> Result<(), RpcClientError> {
        self.steer_turn_as(text, None)
    }

    /// `delivery.steer(text, clientMessageId)`: task_send ALWAYS steers; a busy refusal degrades to
    /// followUp queueing under the same id.
    pub fn steer_turn_as(
        &self,
        text: &str,
        client_message_id: Option<&str>,
    ) -> Result<(), RpcClientError> {
        self.begin_turn();
        match self.send_input(InputCommandType::Steer, text, None, client_message_id) {
            Err(error) if is_busy_child_rejection(&error) => {
                self.deliver_prompt(text, true, client_message_id)
            }
            other => other,
        }
    }

    pub fn follow_up_turn(&self, text: &str) -> Result<(), RpcClientError> {
        self.run_prompt(text, true, None)
    }

    pub fn follow_up_turn_as(
        &self,
        text: &str,
        client_message_id: Option<&str>,
    ) -> Result<(), RpcClientError> {
        self.run_prompt(text, true, client_message_id)
    }

    pub fn abort_turn(&self) -> Result<(), RpcClientError> {
        self.shared.lock().aborted_by_user = true;
        self.run_command(json!({ "type": "abort" }), "abort")
    }

    pub fn subscribe_raw(&self, listener: Arc<dyn Fn(&Value) + Send + Sync>) -> Unsubscribe {
        self.client.on_event(listener)
    }

    pub fn wait_for_idle(&self) {
        let mut state = self.shared.lock();
        while !state.reached_idle && state.exit.is_none() {
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub fn wait_for_turn_outcome(&self) -> RunnerOutcome {
        let mut state = self.shared.lock();
        loop {
            if let Some(outcome) = &state.turn_outcome {
                return outcome.clone();
            }
            if let Some(exit) = &state.exit {
                return exit_turn_outcome(exit, state.final_text.as_deref());
            }
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub fn terminal_assistant_message(&self) -> Option<RpcTerminalAssistantMessage> {
        self.shared.lock().terminal_assistant_message.clone()
    }

    pub fn was_aborted_by_user(&self) -> bool {
        self.shared.lock().aborted_by_user
    }

    pub fn last_seen(&self) -> Option<i64> {
        self.shared.lock().last_seen_at
    }

    pub fn exit_outcome(&self) -> Option<ChildExitOutcome> {
        self.shared.lock().exit.clone()
    }

    pub fn wait_for_exit(&self) -> ChildExitOutcome {
        let mut state = self.shared.lock();
        loop {
            if let Some(exit) = &state.exit {
                return exit.clone();
            }
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub fn terminate_with(&self, options: TerminateOptions) -> std::io::Result<()> {
        self.client.terminate(options)
    }

    pub fn dispose_handle(&self) {
        {
            let mut state = self.shared.lock();
            state.disposed = true;
            self.shared.changed.notify_all();
        }
        let unsubscribes: Vec<Unsubscribe> = self
            .unsubscribe
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
            .collect();
        for unsubscribe in unsubscribes {
            unsubscribe();
        }
        self.client.detach();
    }

    pub fn switch_session_raw(
        &self,
        session_path: &str,
    ) -> Result<RpcSwitchSessionResult, RpcClientError> {
        let resumed = self
            .resume
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match resumed {
            Some((path, result)) if path == session_path => Ok(result),
            _ => self.client.switch_session(session_path),
        }
    }
}

impl ManagedChildHandle for RpcChildHandle {
    fn reconcile_inputs(&self) {
        RpcChildHandle::reconcile_inputs(self);
    }
    fn run_pause(&self) -> Option<&dyn crate::pause::native_child_pause::NativeRunPause> {
        Some(self)
    }
    fn publish_task_status(&self, status: crate::state::TaskStatus) -> Result<(), HostError> {
        self.client
            .publish_task_status(status)
            .map_err(|error| HostError {
                message: error.to_string(),
            })
    }
    fn on_durable_session_id(
        &self,
        listener: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Option<Unsubscribe> {
        self.client.on_durable_session_id(listener)
    }

    fn on_self_resumed(&self, listener: Arc<dyn Fn() + Send + Sync>) -> Option<Unsubscribe> {
        Some(RpcChildHandle::on_self_resumed(self, listener))
    }
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn session_id(&self) -> Option<String> {
        self.shared.lock().session_id.clone()
    }

    fn pid(&self) -> Option<i64> {
        self.client.pid().map(i64::from)
    }

    fn spawn_spec(&self) -> Option<RpcSpawnSpec> {
        self.spawn_spec
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn steer(&self, text: &str) -> Result<(), HostError> {
        self.steer_turn(text).map_err(host_error)
    }

    fn steer_with_identity(&self, text: &str, identity: &InputIdentity) -> Result<(), HostError> {
        self.steer_turn_as(text, Some(&identity.client_message_id))
            .map_err(host_error)
    }

    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        self.follow_up_turn(text).map_err(host_error)
    }

    fn follow_up_with_identity(
        &self,
        text: &str,
        identity: &InputIdentity,
    ) -> Result<(), HostError> {
        self.follow_up_turn_as(text, Some(&identity.client_message_id))
            .map_err(host_error)
    }

    fn abort(&self) -> Result<(), HostError> {
        self.abort_turn().map_err(host_error)
    }

    fn subscribe(&self, listener: ManagedChildListener) -> Unsubscribe {
        self.client
            .on_event(Arc::new(move |event| listener(&managed_event(event))))
    }

    fn wait_for_outcome(&self) -> RunnerOutcome {
        self.wait_for_turn_outcome()
    }

    fn switch_session(
        &self,
        session_path: &str,
    ) -> Option<Result<RpcSwitchSessionResult, HostError>> {
        Some(self.switch_session_raw(session_path).map_err(host_error))
    }

    fn get_entries(&self, since: Option<&str>) -> Option<Result<RpcEntriesResult, HostError>> {
        Some(self.client.get_entries(since).map_err(host_error))
    }

    fn last_assistant_text(&self) -> Option<String> {
        self.shared.lock().final_text.clone()
    }

    fn has_terminate(&self) -> bool {
        true
    }

    fn terminate(&self) -> Result<(), HostError> {
        self.terminate_with(TerminateOptions::default())
            .map_err(|error| HostError {
                message: error.to_string(),
            })
    }

    fn dispose(&self) -> Result<(), HostError> {
        self.dispose_handle();
        Ok(())
    }

    /// The child is a local stdio process: no transport recovery exists to wait on, so it is never recovering.
    fn transport_recovering(&self) -> bool {
        false
    }

    fn supports_stop_when_reachable(&self) -> bool {
        true
    }

    /// Marks the child stopping, then ends it: with no recovery to wait on the host is reachable now.
    fn stop_when_reachable(&self) -> Result<(), HostError> {
        self.mark_stopping();
        self.terminate()
    }

    /// From here the run counts as aborted (upstream `markAborted`), so its turn settles cancelled.
    fn mark_stopping(&self) {
        self.shared.lock().aborted_by_user = true;
    }
}

/// `writeCommand` + `assertOk`: the refused response's typed fields travel with the error.
fn write_command(
    client: &dyn RpcClientPort,
    command: Value,
    label: &str,
) -> Result<(), RpcClientError> {
    let response = client.send(command)?;
    if response.get("success").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    let detail = response
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    Err(RpcCommandError::with_refusal(label, detail, &response).into())
}

fn input_write_error(error: RpcClientError) -> InputWriteError {
    match error {
        RpcClientError::Command(error) => InputWriteError::from_message(
            error.error_code.clone(),
            format!("{}: {}", error.command, error.detail),
        ),
        other => InputWriteError::from_message(None, other.to_string()),
    }
}

fn is_uncertain_input(error: &RpcClientError) -> bool {
    matches!(error, RpcClientError::Write { message } if message.starts_with("uncertain: "))
}

/// `isBusyChildRejection` (`rpc/delivery-semantics.ts`): the child is alive and simply busy.
fn is_busy_child_rejection(error: &RpcClientError) -> bool {
    let detail = match error {
        RpcClientError::Command(error) => error.detail.to_lowercase(),
        RpcClientError::Write { message } => message.to_lowercase(),
        _ => return false,
    };
    detail.contains("already processing")
        || detail.contains("streamingbehavior")
        || detail.contains("one-at-a-time")
}

fn host_error(error: RpcClientError) -> HostError {
    HostError {
        message: error.to_string(),
    }
}

/// `startHeartbeat` driver: one `get_state` in flight at a time on its own thread; this loop owns
/// the deadline. Three consecutive misses (no answer and no session event for the deadline) end
/// the child as `child-unresponsive` and terminate it.
fn spawn_heartbeat(
    shared: Weak<HandleShared>,
    client: Arc<dyn RpcClientPort>,
    task_id: String,
    interval: Duration,
    now: Clock,
) {
    let deadline = Duration::from_millis(HEARTBEAT_DEADLINE_MS as u64);
    thread::spawn(move || {
        loop {
            if !wait_while_live(&shared, interval) {
                return;
            }
            let (answer_tx, answer_rx) = std::sync::mpsc::channel();
            let probe_client = Arc::clone(&client);
            thread::spawn(move || {
                let _ = answer_tx.send(probe_client.send(json!({ "type": "get_state" })));
            });
            let mut missed = false;
            let mut wait = deadline;
            let answer = loop {
                match answer_rx.recv_timeout(wait) {
                    Ok(answer) => break Some(answer),
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
                let Some(strong) = shared.upgrade() else {
                    return;
                };
                if !strong.live() {
                    return;
                }
                let verdict = strong
                    .heartbeat
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .miss(now());
                match verdict {
                    MissVerdict::Rearm(ms) => wait = Duration::from_millis(ms.max(1) as u64),
                    MissVerdict::Missed => {
                        missed = true;
                        wait = deadline;
                    }
                    MissVerdict::Unresponsive => {
                        declare_unresponsive(&strong, client.as_ref(), &task_id);
                        return;
                    }
                }
            };
            let Some(strong) = shared.upgrade() else {
                return;
            };
            if !strong.live() {
                return;
            }
            match answer {
                Some(Ok(response))
                    if response.get("success").and_then(Value::as_bool) != Some(false) =>
                {
                    strong
                        .heartbeat
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .answered();
                    let mut state = strong.lock();
                    state.last_seen_at = Some(now());
                    if let Some(session_id) = read_session_id(&response) {
                        state.session_id = Some(session_id);
                    }
                }
                failed => {
                    let error = match failed {
                        Some(Ok(response)) => response
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("get_state refused")
                            .to_string(),
                        Some(Err(error)) => error.to_string(),
                        None => "get_state probe ended".to_string(),
                    };
                    utils::logger::log(
                        "senpi-task heartbeat get_state failed",
                        Some(&json!({ "taskId": task_id, "error": error })),
                    );
                    let verdict = strong
                        .heartbeat
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .refused(missed);
                    if verdict == Some(MissVerdict::Unresponsive) {
                        declare_unresponsive(&strong, client.as_ref(), &task_id);
                        return;
                    }
                }
            }
        }
    });
}

/// Waits one interval; false once the child exited or the handle was disposed.
fn wait_while_live(shared: &Weak<HandleShared>, interval: Duration) -> bool {
    let Some(strong) = shared.upgrade() else {
        return false;
    };
    let state = strong.lock();
    let (state, _) = strong
        .changed
        .wait_timeout_while(state, interval, |state| {
            !state.disposed && state.exit.is_none()
        })
        .unwrap_or_else(PoisonError::into_inner);
    !state.disposed && state.exit.is_none()
}

/// `onUnresponsive`: settle the turn and the exit, then terminate the silent child.
fn declare_unresponsive(shared: &HandleShared, client: &dyn RpcClientPort, task_id: &str) {
    {
        let mut state = shared.lock();
        shared.settle_turn(
            &mut state,
            RunnerOutcome::error(
                crate::runners::RunnerFailureKind::ChildUnresponsive,
                unresponsive_message(task_id),
            ),
        );
    }
    shared.on_exit(ChildExitOutcome::Crashed {
        facts: crate::runners::types::ChildExitFacts {
            pid: client.pid().map(i64::from),
            code: None,
            signal: None,
            stderr_tail: "child-unresponsive".to_string(),
        },
    });
    if let Err(error) = client.terminate(TerminateOptions::default()) {
        utils::logger::log(
            "senpi-task unresponsive child termination failed",
            Some(&json!({ "taskId": task_id, "error": error.to_string() })),
        );
    }
}

fn read_session_id(response: &Value) -> Option<String> {
    if response.get("command").and_then(Value::as_str) != Some("get_state")
        || response.get("success").and_then(Value::as_bool) != Some(true)
    {
        return None;
    }
    response
        .pointer("/data/sessionId")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `manager/child-handle.ts` `rpcOutcome`: when `agent_end` carries no assistant message, the turn
/// is classified from the terminal `message_end` observed during it.
fn with_terminal_fallback<'a>(event: &'a Value, terminal: Option<&Value>) -> Cow<'a, Value> {
    let has_assistant = event
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages
                .iter()
                .any(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        });
    match terminal {
        Some(message) if !has_assistant => {
            let mut effective = event.clone();
            effective["messages"] = json!([message]);
            Cow::Owned(effective)
        }
        _ => Cow::Borrowed(event),
    }
}

fn terminal_message(message: &Value) -> Option<RpcTerminalAssistantMessage> {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let string = |key: &str| message.get(key).and_then(Value::as_str).map(str::to_string);
    Some(RpcTerminalAssistantMessage {
        text: extract_assistant_text(message),
        stop_reason: string("stopReason"),
        error_message: string("errorMessage"),
    })
}

/// Project an untrusted wire event onto the manager's event shape.
pub fn managed_event(event: &Value) -> ManagedChildEvent {
    let string = |key: &str| event.get(key).and_then(Value::as_str).map(str::to_string);
    let event_type = string("type").unwrap_or_default();
    // Payloads are cloned only for the event types whose consumers read them; streaming
    // deltas (`message_update`, `tool_execution_update`) would otherwise deep-copy the
    // whole partial message per subscriber.
    let value_for = |key: &str, wanted: bool| {
        if wanted {
            event.get(key).cloned()
        } else {
            None
        }
    };
    let is_message = matches!(
        event_type.as_str(),
        "message_start" | "message_update" | "message_end"
    );
    let is_tool_start = event_type == "tool_execution_start";
    let is_tool_end = event_type == "tool_execution_end";
    ManagedChildEvent {
        message: value_for("message", is_message),
        tool_call_id: string("toolCallId"),
        tool_name: string("toolName"),
        args: value_for("args", is_tool_start),
        input: value_for("input", is_tool_start),
        result: value_for("result", is_tool_end),
        is_error: event.get("isError").and_then(Value::as_bool),
        source: string("source"),
        model: value_for("model", event_type == "model_changed"),
        thinking_level: string("thinkingLevel"),
        effect_id: string("effectId"),
        to: string("to"),
        from: string("from"),
        chain_key: string("chainKey"),
        reason: string("reason"),
        limit: string("limit"),
        last_error: string("lastError"),
        event_type,
    }
}

// Normalize RPC wire controls onto the manager's existing native pause seam.
impl crate::pause::native_child_pause::NativeRunPause for RpcChildHandle {
    fn pause_for_run(
        &self,
        owner: crate::pause::ports::RunPauseOwner,
    ) -> Result<
        crate::pause::ports::EngineInheritReceipt,
        crate::pause::native_child_pause::NativePauseError,
    > {
        use crate::runners::rpc::native_run_pause::NativeRunPause;
        let wire = crate::runners::task_parent_control::RunPauseOwner {
            parent_session_id: owner.parent_session_id,
            parent_pause_seq: owner.parent_pause_seq,
            run_ref: crate::runners::task_parent_control::RunRef {
                id: owner.run_ref.id,
                kind: match owner.run_ref.kind {
                    crate::pause::ports::RunRefKind::Task => "task",
                    crate::pause::ports::RunRefKind::Dag => "dag",
                    crate::pause::ports::RunRefKind::Team => "team",
                    crate::pause::ports::RunRefKind::Workpool => "workpool",
                }
                .into(),
            },
        };
        RpcChildHandle::run_pause(self)
            .pause_for_run(&wire)
            .map(|receipt| match receipt {
                crate::runners::task_parent_control::EngineInheritReceipt::Held { child_seq } => {
                    crate::pause::ports::EngineInheritReceipt::Inherited { child_seq }
                }
                crate::runners::task_parent_control::EngineInheritReceipt::WrongParent => {
                    crate::pause::ports::EngineInheritReceipt::WrongParent
                }
            })
            .map_err(|error| {
                crate::pause::native_child_pause::NativePauseError::Engine(error.to_string())
            })
    }
    fn resume_for_run(
        &self,
        request: crate::pause::ports::ResumeTurnRequest,
    ) -> Result<
        crate::pause::native_child_pause::NativeResumeResult,
        crate::pause::native_child_pause::NativePauseError,
    > {
        use crate::runners::rpc::native_run_pause::{NativeResumeReceipt, NativeRunPause};
        let wire = crate::runners::task_parent_control::ResumeRequest {
            parent_session_id: request.owner.parent_session_id,
            parent_pause_seq: request.owner.parent_pause_seq,
            resume_id: request.resume_id,
            child_seq_cutoff: request.child_seq_cutoff,
        };
        RpcChildHandle::run_pause(self)
            .resume_for_run(&wire)
            .map(|receipt| match receipt {
                NativeResumeReceipt::Refused { reason } => {
                    crate::pause::native_child_pause::NativeResumeResult::Refused(
                        match reason.as_str() {
                            "wrong_parent" => crate::pause::ports::ResumeRefusal::WrongParent,
                            "not_held" => crate::pause::ports::ResumeRefusal::NotHeld,
                            _ => crate::pause::ports::ResumeRefusal::Stale,
                        },
                    )
                }
                NativeResumeReceipt::OtherHoldLive => {
                    crate::pause::native_child_pause::NativeResumeResult::OtherHoldLive
                }
                NativeResumeReceipt::Turn { turn_id, outcome } => {
                    crate::pause::native_child_pause::NativeResumeResult::Turn(
                        crate::pause::ports::ResumedChildTurn {
                            turn_id,
                            outcome: Box::new(move || outcome()),
                        },
                    )
                }
            })
            .map_err(|error| {
                crate::pause::native_child_pause::NativePauseError::Engine(error.to_string())
            })
    }
    fn hold_input(
        &self,
        parent: &str,
        input: crate::pause::ports::HoldInput,
    ) -> Result<
        crate::pause::ports::EngineHoldInputResult,
        crate::pause::native_child_pause::NativePauseError,
    > {
        use crate::runners::rpc::native_run_pause::NativeRunPause;
        RpcChildHandle::run_pause(self)
            .hold_input(
                parent,
                &crate::runners::task_parent_control::HeldGuidanceInput {
                    input_id: input.input_id,
                    revision: input.revision,
                    text: input.text,
                },
            )
            .map(|receipt| match receipt {
                crate::runners::task_parent_control::EngineHoldInputResult::Held {
                    entry_id,
                    duplicate,
                } => crate::pause::ports::EngineHoldInputResult::Held {
                    entry_id,
                    duplicate,
                },
                crate::runners::task_parent_control::EngineHoldInputResult::AlreadyDelivered {
                    revision,
                } => crate::pause::ports::EngineHoldInputResult::AlreadyDelivered { revision },
                crate::runners::task_parent_control::EngineHoldInputResult::InputIdConflict {
                    held_revision,
                } => crate::pause::ports::EngineHoldInputResult::InputIdConflict { held_revision },
            })
            .map_err(|error| {
                crate::pause::native_child_pause::NativePauseError::Engine(error.to_string())
            })
    }
    fn observe_hold(
        &self,
        parent: &str,
    ) -> Result<
        Option<crate::pause::ports::ObservedHold>,
        crate::pause::native_child_pause::NativePauseError,
    > {
        use crate::runners::rpc::native_run_pause::NativeRunPause;
        RpcChildHandle::run_pause(self)
            .observe_hold(parent)
            .map(|hold| {
                hold.map(|hold| crate::pause::ports::ObservedHold {
                    held: hold.held,
                    child_seq: hold.child_seq,
                })
            })
            .map_err(|error| {
                crate::pause::native_child_pause::NativePauseError::Engine(error.to_string())
            })
    }
}
