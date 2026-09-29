//! `runners/rpc/handle.ts`: the steerable handle over one RPC child, tracking each turn's outcome.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::host::HostError;
use crate::manager::child_handle::{
    ManagedChildEvent, ManagedChildHandle, ManagedChildListener, Unsubscribe,
};
use crate::runners::RunnerOutcome;
use crate::runners::rpc::errors::RpcCommandError;
use crate::runners::rpc::exit_mapping::classify_child_exit;
use crate::runners::rpc::protocol_client::{RpcClientError, RpcClientPort};
use crate::runners::rpc::turn_outcome::{
    agent_end_outcome, exit_turn_outcome, extract_assistant_text, prompt_failure_outcome,
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
}

#[derive(Default)]
struct HandleState {
    reached_idle: bool,
    session_id: Option<String>,
    final_text: Option<String>,
    turn_baseline: Option<String>,
    turn_outcome: Option<RunnerOutcome>,
    terminal_assistant_message: Option<RpcTerminalAssistantMessage>,
    /// The raw terminal assistant `message_end` payload of the current turn.
    terminal_wire_message: Option<Value>,
    aborted_by_user: bool,
    last_seen_at: Option<i64>,
    exit: Option<ChildExitOutcome>,
    disposed: bool,
}

struct HandleShared {
    state: Mutex<HandleState>,
    changed: Condvar,
}

impl HandleShared {
    fn lock(&self) -> MutexGuard<'_, HandleState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn settle_turn(&self, state: &mut HandleState, settled: RunnerOutcome) {
        if state.turn_outcome.is_some() {
            return;
        }
        state.turn_outcome = Some(settled);
        state.reached_idle = true;
        self.changed.notify_all();
    }

    fn on_event(&self, event: &Value) {
        let mut state = self.lock();
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
    task_id: String,
    client: Arc<dyn RpcClientPort>,
    shared: Arc<HandleShared>,
    spawn_spec: Mutex<Option<RpcSpawnSpec>>,
    resume: Mutex<Option<(String, RpcSwitchSessionResult)>>,
    unsubscribe: Mutex<Vec<Unsubscribe>>,
}

impl RpcChildHandle {
    pub fn new(options: RpcChildHandleOptions) -> Arc<Self> {
        let shared = Arc::new(HandleShared {
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
        Arc::new(Self {
            task_id: options.task_id,
            client,
            shared,
            spawn_spec: Mutex::new(None),
            resume: Mutex::new(None),
            unsubscribe: Mutex::new(vec![unsubscribe_event, unsubscribe_exit]),
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
        let response = self.client.send(command)?;
        if response.get("success").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        let detail = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        Err(RpcCommandError::new(label, detail).into())
    }

    fn begin_turn(&self) {
        let mut state = self.shared.lock();
        if state.exit.is_some() {
            return;
        }
        if state.reached_idle || state.turn_outcome.is_some() {
            state.reached_idle = false;
            state.turn_outcome = None;
        }
        state.terminal_assistant_message = None;
        state.terminal_wire_message = None;
        state.aborted_by_user = false;
        state.turn_baseline = state.final_text.clone();
    }

    fn run_prompt(&self, text: &str, follow_up: bool) -> Result<(), RpcClientError> {
        self.begin_turn();
        let command = if follow_up {
            json!({ "type": "prompt", "message": text, "streamingBehavior": "followUp" })
        } else {
            json!({ "type": "prompt", "message": text })
        };
        self.run_command(command, "prompt").inspect_err(|error| {
            let mut state = self.shared.lock();
            self.shared
                .settle_turn(&mut state, prompt_failure_outcome(&error.to_string()));
        })
    }

    pub fn start_initial_prompt(&self, text: &str) -> Result<(), RpcClientError> {
        self.run_prompt(text, false)
    }

    pub fn steer_turn(&self, text: &str) -> Result<(), RpcClientError> {
        self.begin_turn();
        self.run_command(json!({ "type": "steer", "message": text }), "steer")
    }

    pub fn follow_up_turn(&self, text: &str) -> Result<(), RpcClientError> {
        self.run_prompt(text, true)
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

    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        self.follow_up_turn(text).map_err(host_error)
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
}

fn host_error(error: RpcClientError) -> HostError {
    HostError {
        message: error.to_string(),
    }
}

fn spawn_heartbeat(
    shared: Weak<HandleShared>,
    client: Arc<dyn RpcClientPort>,
    task_id: String,
    interval: Duration,
    now: Clock,
) {
    thread::spawn(move || {
        loop {
            let Some(strong) = shared.upgrade() else {
                return;
            };
            {
                let state = strong.lock();
                let (state, _) = strong
                    .changed
                    .wait_timeout_while(state, interval, |state| {
                        !state.disposed && state.exit.is_none()
                    })
                    .unwrap_or_else(PoisonError::into_inner);
                if state.disposed || state.exit.is_some() {
                    return;
                }
            }
            drop(strong);
            match client.send(json!({ "type": "get_state" })) {
                Ok(response) => {
                    let Some(strong) = shared.upgrade() else {
                        return;
                    };
                    let mut state = strong.lock();
                    state.last_seen_at = Some(now());
                    if let Some(session_id) = read_session_id(&response) {
                        state.session_id = Some(session_id);
                    }
                }
                Err(error) => utils::logger::log(
                    "senpi-task heartbeat get_state failed",
                    Some(&json!({ "taskId": task_id, "error": error.to_string() })),
                ),
            }
        }
    });
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
fn with_terminal_fallback(event: &Value, terminal: Option<&Value>) -> Value {
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
            effective
        }
        _ => event.clone(),
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
    let value = |key: &str| event.get(key).cloned();
    ManagedChildEvent {
        event_type: string("type").unwrap_or_default(),
        message: value("message"),
        tool_call_id: string("toolCallId"),
        tool_name: string("toolName"),
        args: value("args"),
        input: value("input"),
        result: value("result"),
        is_error: event.get("isError").and_then(Value::as_bool),
        to: string("to"),
        from: string("from"),
        chain_key: string("chainKey"),
        reason: string("reason"),
        last_error: string("lastError"),
    }
}
