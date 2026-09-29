//! `runners/rpc/protocol-client.ts`: newline-delimited JSON RPC client over a spawned senpi child.
//!
//! Correlates responses to requests by id, fans events out to subscribers, and auto-answers
//! `extension_ui_request` with safe deny/cancel defaults so the child never blocks on UI. A
//! malformed line is reported and skipped; the connection survives. The client itself never
//! signals the process: [`RpcClientPort::terminate`] delegates to `terminate.rs`.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread;

use serde_json::{Map, Value, json};

use crate::manager::child_handle::Unsubscribe;
use crate::runners::rpc::errors::RpcCommandError;
use crate::runners::rpc::exit_mapping::{ChildExitInput, tail_stderr};
use crate::runners::rpc::process::{RpcChildProcess, signal_name};
use crate::runners::rpc::terminate::terminate_rpc_child;
use crate::runners::rpc::ui_auto_answer::build_auto_ui_response;
use crate::runners::types::{RpcEntriesResult, RpcSwitchSessionResult, TerminateOptions};

const STDERR_BUFFER_CAP: usize = 16_384;
const STDERR_TAIL_CAP: usize = 4_096;

pub type MalformedLineHandler = Arc<dyn Fn(&str, &str) + Send + Sync>;
pub type RpcEventListener = Arc<dyn Fn(&Value) + Send + Sync>;
pub type RpcExitListener = Arc<dyn Fn(&ChildExitInput) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcClientError {
    #[error("RPC process is not running. Stderr: {stderr_tail}")]
    NotRunning { stderr_tail: String },
    #[error("{message}")]
    Exited { message: String },
    #[error("{message}")]
    Write { message: String },
    #[error(transparent)]
    Command(#[from] RpcCommandError),
}

/// The client surface the RPC handle programs against (a fake implements it in tests).
pub trait RpcClientPort: Send + Sync {
    fn pid(&self) -> Option<u32>;
    /// Send one command and block until its correlated response arrives.
    fn send(&self, command: Value) -> Result<Value, RpcClientError>;
    fn on_event(&self, listener: RpcEventListener) -> Unsubscribe;
    /// Fires once when the child is gone; a listener added afterwards fires immediately.
    fn on_exit(&self, listener: RpcExitListener) -> Unsubscribe;
    fn stderr_tail(&self) -> String;
    fn detach(&self);
    fn terminate(&self, options: TerminateOptions) -> io::Result<()>;

    fn switch_session(&self, session_path: &str) -> Result<RpcSwitchSessionResult, RpcClientError> {
        let response =
            self.send(json!({ "type": "switch_session", "sessionPath": session_path }))?;
        if is_success_for(&response, "switch_session") {
            let cancelled = response
                .pointer("/data/cancelled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            return Ok(RpcSwitchSessionResult { cancelled });
        }
        Err(command_error(&response, "switch_session").into())
    }

    fn get_entries(&self, since: Option<&str>) -> Result<RpcEntriesResult, RpcClientError> {
        let command = match since {
            Some(since) => json!({ "type": "get_entries", "since": since }),
            None => json!({ "type": "get_entries" }),
        };
        let response = self.send(command)?;
        if is_success_for(&response, "get_entries") {
            let entries = response
                .pointer("/data/entries")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let leaf_id = response
                .pointer("/data/leafId")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Ok(RpcEntriesResult { entries, leaf_id });
        }
        Err(command_error(&response, "get_entries").into())
    }
}

#[derive(Clone, Default)]
pub struct RpcProtocolClientOptions {
    pub on_malformed_line: Option<MalformedLineHandler>,
    /// Defaults to `true`.
    pub auto_answer_ui: Option<bool>,
}

type PendingSender = Sender<Result<Value, RpcClientError>>;

#[derive(Default)]
struct ClientState {
    pending: HashMap<String, PendingSender>,
    event_listeners: Vec<(u64, RpcEventListener)>,
    exit_listeners: Vec<(u64, RpcExitListener)>,
    stderr: String,
    exit: Option<ChildExitInput>,
}

struct ClientShared {
    state: Mutex<ClientState>,
    next_request_id: AtomicU64,
    next_listener_id: AtomicU64,
}

impl ClientShared {
    fn lock(&self) -> std::sync::MutexGuard<'_, ClientState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct RpcProtocolClient {
    child: Arc<RpcChildProcess>,
    shared: Arc<ClientShared>,
}

/// One in-flight request; [`Self::wait`] blocks until the correlated response or the exit.
pub struct PendingResponse {
    receiver: Receiver<Result<Value, RpcClientError>>,
}

impl PendingResponse {
    pub fn wait(self) -> Result<Value, RpcClientError> {
        self.receiver.recv().unwrap_or_else(|_| {
            Err(RpcClientError::Exited {
                message: "RPC process exited".to_string(),
            })
        })
    }
}

impl RpcProtocolClient {
    pub fn new(child: Arc<RpcChildProcess>, options: RpcProtocolClientOptions) -> Self {
        let shared = Arc::new(ClientShared {
            state: Mutex::new(ClientState::default()),
            next_request_id: AtomicU64::new(0),
            next_listener_id: AtomicU64::new(0),
        });
        let client = Self {
            child: Arc::clone(&child),
            shared: Arc::clone(&shared),
        };
        if let Some(error) = child.spawn_error() {
            finalize(
                &shared,
                ChildExitInput {
                    error: Some(error.to_string()),
                    ..ChildExitInput::default()
                },
            );
            return client;
        }
        let stderr_pump = child.take_stderr().map(|stderr| {
            let shared = Arc::clone(&shared);
            thread::spawn(move || pump_stderr(stderr, &shared))
        });
        let on_malformed_line = options.on_malformed_line.unwrap_or_else(|| {
            Arc::new(|line: &str, error: &str| {
                utils::logger::log(
                    "senpi-task rpc malformed line",
                    Some(&json!({ "line": line, "error": error })),
                );
            })
        });
        let auto_answer_ui = options.auto_answer_ui.unwrap_or(true);
        let stdout = child.take_stdout();
        thread::spawn(move || {
            if let Some(stdout) = stdout {
                let mut reader = BufReader::new(stdout);
                let mut raw = Vec::new();
                while matches!(reader.read_until(b'\n', &mut raw), Ok(read) if read > 0) {
                    let line = String::from_utf8_lossy(&raw).trim().to_string();
                    raw.clear();
                    if !line.is_empty() {
                        handle_line(&shared, &child, &line, &on_malformed_line, auto_answer_ui);
                    }
                }
            }
            if let Some(pump) = stderr_pump {
                let _ = pump.join();
            }
            let status = child.wait_exit();
            let stderr = shared.lock().stderr.clone();
            finalize(
                &shared,
                ChildExitInput {
                    code: status.code,
                    signal: status.signal.map(signal_name),
                    error: None,
                    pid: child.pid().map(i64::from),
                    stderr,
                },
            );
        });
        client
    }

    pub fn exited(&self) -> bool {
        self.shared.lock().exit.is_some()
    }

    /// Length of the raw stderr buffer, exposed for tests/diagnostics.
    pub fn stderr_buffer_length(&self) -> usize {
        self.shared.lock().stderr.chars().count()
    }

    /// Start a request without waiting, so several can be in flight at once.
    pub fn request(&self, command: Value) -> Result<PendingResponse, RpcClientError> {
        let mut command = match command {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        let id = match command.get("id").and_then(Value::as_str) {
            Some(id) => id.to_string(),
            None => {
                let next = self.shared.next_request_id.fetch_add(1, Ordering::SeqCst) + 1;
                format!("senpi-task_{next}")
            }
        };
        command.insert("id".to_string(), Value::String(id.clone()));
        let (sender, receiver) = mpsc::channel();
        {
            let mut state = self.shared.lock();
            if state.exit.is_some() {
                return Err(RpcClientError::NotRunning {
                    stderr_tail: tail_stderr(&state.stderr, STDERR_TAIL_CAP),
                });
            }
            state.pending.insert(id.clone(), sender);
        }
        if let Err(error) = self.child.write_line(&Value::Object(command).to_string()) {
            self.shared.lock().pending.remove(&id);
            return Err(RpcClientError::Write {
                message: error.to_string(),
            });
        }
        Ok(PendingResponse { receiver })
    }
}

impl RpcClientPort for RpcProtocolClient {
    fn pid(&self) -> Option<u32> {
        self.child.pid()
    }

    fn send(&self, command: Value) -> Result<Value, RpcClientError> {
        self.request(command)?.wait()
    }

    fn on_event(&self, listener: RpcEventListener) -> Unsubscribe {
        let id = self.shared.next_listener_id.fetch_add(1, Ordering::SeqCst);
        self.shared.lock().event_listeners.push((id, listener));
        let weak = Arc::downgrade(&self.shared);
        Box::new(move || remove_listener(&weak, id, |state| &mut state.event_listeners))
    }

    fn on_exit(&self, listener: RpcExitListener) -> Unsubscribe {
        let id = self.shared.next_listener_id.fetch_add(1, Ordering::SeqCst);
        let already_exited = {
            let mut state = self.shared.lock();
            match &state.exit {
                Some(exit) => Some(exit.clone()),
                None => {
                    state.exit_listeners.push((id, Arc::clone(&listener)));
                    None
                }
            }
        };
        if let Some(exit) = already_exited {
            listener(&exit);
        }
        let weak = Arc::downgrade(&self.shared);
        Box::new(move || remove_listener(&weak, id, |state| &mut state.exit_listeners))
    }

    fn stderr_tail(&self) -> String {
        tail_stderr(&self.shared.lock().stderr, STDERR_TAIL_CAP)
    }

    fn detach(&self) {
        let mut state = self.shared.lock();
        state.event_listeners.clear();
        state.exit_listeners.clear();
    }

    fn terminate(&self, options: TerminateOptions) -> io::Result<()> {
        terminate_rpc_child(&self.child, options)
    }
}

fn remove_listener<T>(
    weak: &Weak<ClientShared>,
    id: u64,
    list: impl FnOnce(&mut ClientState) -> &mut Vec<(u64, T)>,
) {
    if let Some(shared) = weak.upgrade() {
        list(&mut shared.lock()).retain(|(listener_id, _)| *listener_id != id);
    }
}

fn pump_stderr(mut stderr: impl Read, shared: &ClientShared) {
    let mut chunk = [0_u8; 8192];
    while let Ok(read) = stderr.read(&mut chunk) {
        if read == 0 {
            break;
        }
        let mut state = shared.lock();
        state
            .stderr
            .push_str(&String::from_utf8_lossy(&chunk[..read]));
        state.stderr = tail_stderr(&state.stderr, STDERR_BUFFER_CAP);
    }
}

fn handle_line(
    shared: &ClientShared,
    child: &RpcChildProcess,
    line: &str,
    on_malformed_line: &MalformedLineHandler,
    auto_answer_ui: bool,
) {
    let parsed: Value = match serde_json::from_str(line) {
        Ok(parsed) => parsed,
        Err(error) => {
            on_malformed_line(line, &error.to_string());
            return;
        }
    };
    match parsed.get("type").and_then(Value::as_str) {
        Some("response") => {
            let Some(id) = parsed.get("id").and_then(Value::as_str) else {
                return;
            };
            let pending = shared.lock().pending.remove(id);
            if let Some(pending) = pending {
                let _ = pending.send(Ok(parsed));
            }
        }
        Some("extension_ui_request") => {
            if auto_answer_ui && let Some(answer) = build_auto_ui_response(&parsed) {
                let _ = child.write_line(&answer.to_string());
            }
        }
        _ => {
            let listeners: Vec<RpcEventListener> = shared
                .lock()
                .event_listeners
                .iter()
                .map(|(_, listener)| Arc::clone(listener))
                .collect();
            for listener in listeners {
                listener(&parsed);
            }
        }
    }
}

fn finalize(shared: &ClientShared, exit: ChildExitInput) {
    let (pending, listeners) = {
        let mut state = shared.lock();
        if state.exit.is_some() {
            return;
        }
        state.exit = Some(exit.clone());
        let listeners: Vec<RpcExitListener> = state
            .exit_listeners
            .drain(..)
            .map(|(_, listener)| listener)
            .collect();
        (std::mem::take(&mut state.pending), listeners)
    };
    let failure = match &exit.error {
        Some(error) => RpcClientError::Exited {
            message: error.clone(),
        },
        None => RpcClientError::Exited {
            message: format!(
                "RPC process exited. Stderr: {}",
                tail_stderr(&exit.stderr, STDERR_TAIL_CAP)
            ),
        },
    };
    for (_, sender) in pending {
        let _ = sender.send(Err(failure.clone()));
    }
    for listener in listeners {
        listener(&exit);
    }
}

pub(crate) fn is_success_for(response: &Value, command: &str) -> bool {
    response.get("success").and_then(Value::as_bool) == Some(true)
        && response.get("command").and_then(Value::as_str) == Some(command)
}

fn command_error(response: &Value, expected_command: &str) -> RpcCommandError {
    if response.get("success").and_then(Value::as_bool) != Some(true) {
        let detail = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return RpcCommandError::new(expected_command, detail);
    }
    let actual = response
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("undefined");
    RpcCommandError::new(
        expected_command,
        format!("unexpected response command: {actual}"),
    )
}
