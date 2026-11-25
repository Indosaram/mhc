use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::host::HostError;
use crate::manager::child_handle::{
    ManagedChildHandle, ManagedChildListener, Unsubscribe,
};
use crate::manager::types::{
    ManagedRunner, ManagedRunnerError, ManagedRunnerResult, ManagedStartSpec,
};
use crate::runners::rpc::errors::RpcCommandError;
use crate::runners::rpc::exit_mapping::ChildExitInput;
use crate::runners::rpc::handle::{Clock, RpcChildHandle, RpcChildHandleOptions};
use crate::runners::rpc::protocol_client::{
    RpcClientError, RpcClientPort, RpcEventListener, RpcExitListener,
};
use crate::runners::rpc::ui_auto_answer::build_auto_ui_response;
use crate::runners::types::TerminateOptions;
use crate::runners::{RunnerFailure, RunnerFailureKind, RunnerOutcome};

const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 10_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const AVAILABILITY_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquiredCloudBox {
    pub machine: String,
    pub provider: String,
    pub transport: String,
    pub socket_path: String,
    pub remote_home: String,
    pub lease_id: String,
    #[serde(default)]
    pub reused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncedWorkspace {
    pub machine: String,
    pub task_id: String,
    pub remote_cwd: String,
    #[serde(default)]
    pub file_count: usize,
    #[serde(default)]
    pub archive_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutResult {
    pub machine: String,
    pub task_id: String,
    #[serde(default)]
    pub has_changes: bool,
    #[serde(default)]
    pub changed_files: Vec<String>,
    pub patch_path: Option<String>,
    #[serde(default)]
    pub patch_bytes: usize,
}

pub trait CloudPoolBackend: Send + Sync {
    fn is_available(&self) -> bool;
    fn acquire(
        &self,
        task_id: &str,
        provider: Option<&str>,
        ttl_seconds: Option<u64>,
    ) -> Result<AcquiredCloudBox, String>;
    fn sync_in(
        &self,
        machine: &str,
        local_cwd: &Path,
        task_id: &str,
    ) -> Result<SyncedWorkspace, String>;
    fn sync_out(
        &self,
        machine: &str,
        task_id: &str,
        patch_out: &Path,
        cleanup: bool,
    ) -> Result<SyncOutResult, String>;
    fn release(&self, machine: &str, lease_id: &str, delete: bool) -> Result<(), String>;
}

#[derive(Debug, Clone)]
pub struct CloudRunnerSettings {
    pub provider: Option<String>,
    pub ttl_seconds: Option<u64>,
    pub sync_workspace: bool,
    pub apply_patch: bool,
    pub delete_on_release: bool,
    pub heartbeat_interval_ms: u64,
}

impl Default for CloudRunnerSettings {
    fn default() -> Self {
        Self {
            provider: None,
            ttl_seconds: Some(1_800),
            sync_workspace: true,
            apply_patch: true,
            delete_on_release: false,
            heartbeat_interval_ms: DEFAULT_HEARTBEAT_INTERVAL_MS,
        }
    }
}

pub struct FerryxCliCloudBackend {
    explicit_bin: Option<PathBuf>,
    availability_cache: Mutex<Option<(Instant, bool)>>,
}

impl FerryxCliCloudBackend {
    pub fn new(explicit_bin: Option<PathBuf>) -> Self {
        Self {
            explicit_bin,
            availability_cache: Mutex::new(None),
        }
    }

    pub fn resolve_binary(&self) -> Option<PathBuf> {
        if let Some(bin) = &self.explicit_bin {
            if bin.is_file() {
                return Some(bin.clone());
            }
        }
        if let Ok(env_bin) = std::env::var("FERRYX_CLI_PATH") {
            let trimmed = env_bin.trim();
            if !trimmed.is_empty() {
                let candidate = PathBuf::from(trimmed);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        if let Some(path_var) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let candidate = dir.join("ferryx-cli");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        for fallback in [
            "/Applications/Ferryx.app/Contents/MacOS/ferryx-cli",
            "/tmp/fx-cli-mac/ferryx-cli",
        ] {
            let candidate = PathBuf::from(fallback);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    fn run_json_command(&self, args: &[String]) -> Result<Value, String> {
        let bin = self
            .resolve_binary()
            .ok_or_else(|| "ferryx-cli binary not found on PATH or FERRYX_CLI_PATH".to_string())?;
        let output = Command::new(&bin)
            .args(args)
            .output()
            .map_err(|error| format!("failed to run {}: {error}", bin.display()))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            return Err(format!(
                "ferryx-cli {} exited {:?}: {}",
                args.join(" "),
                output.status.code(),
                stderr.trim()
            ));
        }
        let line = stdout
            .lines()
            .rev()
            .find(|l| {
                let t = l.trim();
                t.starts_with('{') || t.starts_with('[')
            })
            .ok_or_else(|| format!("ferryx-cli returned non-JSON output: {}", stdout.trim()))?;
        serde_json::from_str::<Value>(line.trim())
            .map_err(|error| format!("invalid JSON from ferryx-cli: {error}"))
    }
}

impl CloudPoolBackend for FerryxCliCloudBackend {
    fn is_available(&self) -> bool {
        {
            let cache = self
                .availability_cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some((at, ready)) = *cache {
                if at.elapsed() < AVAILABILITY_CACHE_TTL {
                    return ready;
                }
            }
        }
        let ready = self
            .run_json_command(&["cloud".into(), "providers".into(), "--json".into()])
            .ok()
            .and_then(|v| v.as_array().cloned())
            .is_some_and(|providers| {
                providers
                    .iter()
                    .any(|p| p.get("configured").and_then(Value::as_bool) == Some(true))
            });
        let mut cache = self
            .availability_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *cache = Some((Instant::now(), ready));
        ready
    }

    fn acquire(
        &self,
        task_id: &str,
        provider: Option<&str>,
        ttl_seconds: Option<u64>,
    ) -> Result<AcquiredCloudBox, String> {
        let mut args = vec![
            "cloud".into(),
            "acquire".into(),
            "--owner-pid".into(),
            std::process::id().to_string(),
            "--task-id".into(),
            task_id.to_string(),
            "--json".into(),
        ];
        if let Some(p) = provider {
            args.push("--provider".into());
            args.push(p.to_string());
        }
        if let Some(ttl) = ttl_seconds {
            args.push("--ttl".into());
            args.push(format!("{ttl}s"));
        }
        let value = self.run_json_command(&args)?;
        serde_json::from_value(value).map_err(|error| format!("bad acquire payload: {error}"))
    }

    fn sync_in(
        &self,
        machine: &str,
        local_cwd: &Path,
        task_id: &str,
    ) -> Result<SyncedWorkspace, String> {
        let args = vec![
            "cloud".into(),
            "sync-in".into(),
            machine.to_string(),
            "--local-cwd".into(),
            local_cwd.to_string_lossy().into_owned(),
            "--task-id".into(),
            task_id.to_string(),
            "--json".into(),
        ];
        let value = self.run_json_command(&args)?;
        serde_json::from_value(value).map_err(|error| format!("bad sync-in payload: {error}"))
    }

    fn sync_out(
        &self,
        machine: &str,
        task_id: &str,
        patch_out: &Path,
        cleanup: bool,
    ) -> Result<SyncOutResult, String> {
        let mut args = vec![
            "cloud".into(),
            "sync-out".into(),
            machine.to_string(),
            "--task-id".into(),
            task_id.to_string(),
            "--patch-out".into(),
            patch_out.to_string_lossy().into_owned(),
            "--json".into(),
        ];
        if cleanup {
            args.push("--cleanup".into());
        }
        let value = self.run_json_command(&args)?;
        serde_json::from_value(value).map_err(|error| format!("bad sync-out payload: {error}"))
    }

    fn release(&self, machine: &str, lease_id: &str, delete: bool) -> Result<(), String> {
        let Some(bin) = self.resolve_binary() else {
            return Ok(());
        };
        let mut args = vec![
            "cloud".into(),
            "release".into(),
            machine.to_string(),
            "--lease".into(),
            lease_id.to_string(),
        ];
        if delete {
            args.push("--delete".into());
        }
        let output = Command::new(&bin)
            .args(&args)
            .output()
            .map_err(|error| format!("failed to run {}: {error}", bin.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("ferryx-cli cloud release failed: {}", stderr.trim()));
        }
        Ok(())
    }
}

type PendingSender = Sender<Result<Value, RpcClientError>>;

#[derive(Default)]
struct SocketClientState {
    session_id: Option<String>,
    pending: HashMap<String, PendingSender>,
    next_listener_id: u64,
    event_listeners: Vec<(u64, RpcEventListener)>,
    exit_listeners: Vec<(u64, RpcExitListener)>,
    exited: Option<ChildExitInput>,
    closed: bool,
}

#[cfg(unix)]
pub struct RemoteSocketRpcClient {
    writer: Mutex<std::os::unix::net::UnixStream>,
    state: Arc<Mutex<SocketClientState>>,
    next_request_id: AtomicU64,
}

#[cfg(not(unix))]
pub struct RemoteSocketRpcClient {
    state: Arc<Mutex<SocketClientState>>,
    next_request_id: AtomicU64,
}

#[cfg(unix)]
impl RemoteSocketRpcClient {
    pub fn connect(socket_path: &Path) -> io::Result<Arc<Self>> {
        let stream = std::os::unix::net::UnixStream::connect(socket_path)?;
        let reader_stream = stream.try_clone()?;
        let client = Arc::new(Self {
            writer: Mutex::new(stream),
            state: Arc::new(Mutex::new(SocketClientState::default())),
            next_request_id: AtomicU64::new(0),
        });
        let weak = Arc::downgrade(&client);
        let _ = thread::Builder::new()
            .name("senpi-cloud-rpc-reader".into())
            .spawn(move || read_socket_loop(weak, reader_stream));
        Ok(client)
    }

    pub fn open_session(
        &self,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, RpcClientError> {
        let mut command = json!({
            "type": "open_session",
            "cwd": cwd,
            "kind": "worker",
            "retainOnDisconnect": false,
        });
        if let Some(reference) = model.filter(|m| !m.trim().is_empty()) {
            if let Some((provider, model_id)) = reference.split_once('/') {
                if let Some(obj) = command.as_object_mut() {
                    obj.insert("provider".into(), Value::String(provider.to_string()));
                    obj.insert("modelId".into(), Value::String(model_id.to_string()));
                }
            }
        }
        let response = self.send_raw(command, false)?;
        if response.get("success").and_then(Value::as_bool) != Some(true) {
            let msg = response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("open_session failed")
                .to_string();
            return Err(RpcCommandError::new("open_session", msg).into());
        }
        let session_id = response
            .pointer("/data/sessionId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| RpcClientError::Exited {
                message: "remote host open_session returned no sessionId".into(),
            })?;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.session_id = Some(session_id.clone());
        Ok(session_id)
    }

    fn send_raw(&self, mut command: Value, attach_session: bool) -> Result<Value, RpcClientError> {
        let req_id = format!(
            "cloud-req-{}",
            self.next_request_id.fetch_add(1, Ordering::Relaxed) + 1
        );
        let (tx, rx) = mpsc::channel();
        let session_id = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.closed || state.exited.is_some() {
                return Err(RpcClientError::NotRunning {
                    stderr_tail: "remote cloud socket closed".into(),
                });
            }
            state.pending.insert(req_id.clone(), tx);
            state.session_id.clone()
        };

        if let Some(obj) = command.as_object_mut() {
            obj.insert("id".into(), Value::String(req_id.clone()));
            if attach_session {
                if let Some(sid) = session_id {
                    obj.entry("sessionId".to_string())
                        .or_insert_with(|| Value::String(sid));
                }
            }
        }
        if let Err(error) = self.write_line(&command) {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pending
                .remove(&req_id);
            return Err(RpcClientError::Write {
                message: format!("failed writing to cloud RPC socket: {error}"),
            });
        }

        match rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                self.state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .pending
                    .remove(&req_id);
                Err(RpcClientError::Exited {
                    message: format!("cloud RPC request `{req_id}` timed out"),
                })
            }
        }
    }

    fn write_line(&self, value: &Value) -> io::Result<()> {
        let mut encoded = serde_json::to_string(value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        encoded.push('\n');
        let mut stream = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        stream.write_all(encoded.as_bytes())?;
        stream.flush()
    }

    fn close_remote_session_best_effort(&self) {
        let session_id = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.closed {
                return;
            }
            state.closed = true;
            state.session_id.clone()
        };
        if let Some(sid) = session_id {
            let _ = self.write_line(&json!({
                "type": "close_session",
                "sessionId": sid,
            }));
        }
        if let Ok(stream) = self.writer.lock() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

#[cfg(unix)]
fn read_socket_loop(weak: Weak<RemoteSocketRpcClient>, stream: std::os::unix::net::UnixStream) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Some(client) = weak.upgrade() else {
                    return;
                };
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    handle_socket_frame(&client, parsed);
                }
            }
            Err(_) => break,
        }
    }
    if let Some(client) = weak.upgrade() {
        notify_socket_exit(&client);
    }
}

#[cfg(unix)]
fn handle_socket_frame(client: &RemoteSocketRpcClient, parsed: Value) {
    let frame_type = parsed.get("type").and_then(Value::as_str);
    if frame_type == Some("response") {
        if let Some(id) = parsed.get("id").and_then(Value::as_str) {
            let sender = client
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pending
                .remove(id);
            if let Some(tx) = sender {
                let _ = tx.send(Ok(parsed));
                return;
            }
        }
        return;
    }

    if frame_type == Some("extension_ui_request") {
        if let Some(reply) = build_auto_ui_response(&parsed) {
            let mut reply_obj = match reply {
                Value::Object(map) => map,
                _ => Map::new(),
            };
            let sid = client
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .session_id
                .clone();
            if let Some(sid) = sid {
                reply_obj
                    .entry("sessionId".to_string())
                    .or_insert_with(|| Value::String(sid));
            }
            let _ = client.write_line(&Value::Object(reply_obj));
        }
        return;
    }

    let (expected_sid, listeners) = {
        let state = client.state.lock().unwrap_or_else(PoisonError::into_inner);
        let listeners: Vec<RpcEventListener> =
            state.event_listeners.iter().map(|(_, l)| Arc::clone(l)).collect();
        (state.session_id.clone(), listeners)
    };
    if let (Some(expected), Some(frame_sid)) = (
        expected_sid.as_deref(),
        parsed.get("sessionId").and_then(Value::as_str),
    ) {
        if frame_sid != expected {
            return;
        }
    }
    for listener in listeners {
        listener(&parsed);
    }
}

#[cfg(unix)]
fn notify_socket_exit(client: &RemoteSocketRpcClient) {
    let (pending, exit_listeners, exit_input) = {
        let mut state = client.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.exited.is_some() {
            return;
        }
        let exit_input = ChildExitInput {
            pid: None,
            code: Some(if state.closed { 0 } else { 1 }),
            signal: None,
            error: None,
            stderr: if state.closed {
                String::new()
            } else {
                "remote cloud RPC socket disconnected".into()
            },
        };
        state.exited = Some(exit_input.clone());
        let pending: Vec<PendingSender> = state.pending.drain().map(|(_, tx)| tx).collect();
        let exit_listeners: Vec<RpcExitListener> =
            state.exit_listeners.iter().map(|(_, l)| Arc::clone(l)).collect();
        (pending, exit_listeners, exit_input)
    };
    for tx in pending {
        let _ = tx.send(Err(RpcClientError::Exited {
            message: "remote cloud RPC socket closed".into(),
        }));
    }
    for listener in exit_listeners {
        listener(&exit_input);
    }
}

#[cfg(not(unix))]
impl RemoteSocketRpcClient {
    pub fn connect(_socket_path: &Path) -> io::Result<Arc<Self>> {
        Err(io::Error::other("cloud Unix socket transport requires Unix"))
    }

    pub fn open_session(&self, _cwd: &str, _model: Option<&str>) -> Result<String, RpcClientError> {
        Err(RpcClientError::Exited {
            message: "cloud Unix socket transport requires Unix".into(),
        })
    }

    fn send_raw(&self, _command: Value, _attach_session: bool) -> Result<Value, RpcClientError> {
        Err(RpcClientError::Exited {
            message: "cloud Unix socket transport requires Unix".into(),
        })
    }

    fn close_remote_session_best_effort(&self) {}
}

impl RpcClientPort for RemoteSocketRpcClient {
    fn pid(&self) -> Option<u32> {
        None
    }

    fn send(&self, command: Value) -> Result<Value, RpcClientError> {
        self.send_raw(command, true)
    }

    fn on_event(&self, listener: RpcEventListener) -> Unsubscribe {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.next_listener_id += 1;
        let id = state.next_listener_id;
        state.event_listeners.push((id, listener));
        let shared = Arc::clone(&self.state);
        Box::new(move || {
            let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
            state.event_listeners.retain(|(existing, _)| *existing != id);
        })
    }

    fn on_exit(&self, listener: RpcExitListener) -> Unsubscribe {
        let existing_exit = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(exit) = state.exited.clone() {
                Some(exit)
            } else {
                state.next_listener_id += 1;
                let id = state.next_listener_id;
                state.exit_listeners.push((id, listener.clone()));
                let shared = Arc::clone(&self.state);
                return Box::new(move || {
                    let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    state.exit_listeners.retain(|(existing, _)| *existing != id);
                });
            }
        };
        if let Some(exit) = existing_exit {
            listener(&exit);
        }
        Box::new(|| {})
    }

    fn stderr_tail(&self) -> String {
        String::new()
    }

    fn detach(&self) {
        self.close_remote_session_best_effort();
    }

    fn terminate(&self, _options: TerminateOptions) -> io::Result<()> {
        self.close_remote_session_best_effort();
        Ok(())
    }
}

struct SyncContext {
    local_cwd: PathBuf,
    state_dir: PathBuf,
    task_id: String,
    apply_patch: bool,
}

pub struct CloudChildHandle {
    inner: Arc<RpcChildHandle>,
    backend: Arc<dyn CloudPoolBackend>,
    acquired: AcquiredCloudBox,
    sync_ctx: Option<SyncContext>,
    delete_on_release: bool,
    settled_sync: AtomicBool,
    released: AtomicBool,
}

impl CloudChildHandle {
    fn ensure_released(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.backend.release(
            &self.acquired.machine,
            &self.acquired.lease_id,
            self.delete_on_release,
        );
    }

    fn settle_workspace_changes(&self, outcome: RunnerOutcome) -> RunnerOutcome {
        if self.settled_sync.swap(true, Ordering::AcqRel) {
            return outcome;
        }
        let Some(ctx) = &self.sync_ctx else {
            return outcome;
        };
        let patch_path = ctx
            .state_dir
            .join("cloud-patches")
            .join(format!("{}.patch", ctx.task_id));
        let sync_res = self
            .backend
            .sync_out(&self.acquired.machine, &ctx.task_id, &patch_path, true);
        let RunnerOutcome::Completed { mut final_response } = outcome else {
            return outcome;
        };
        match sync_res {
            Ok(out) if out.has_changes => {
                if ctx.apply_patch {
                    if let Some(path) = out.patch_path.as_deref() {
                        match apply_patch_to_local(&ctx.local_cwd, Path::new(path)) {
                            Ok(()) => {
                                let suffix = format!(
                                    "\n\n[cloud sync] applied {} file change(s) from {} ({}).",
                                    out.changed_files.len(),
                                    self.acquired.machine,
                                    self.acquired.provider
                                );
                                final_response = format!("{}{suffix}", final_response.trim_end());
                            }
                            Err(reason) => {
                                let suffix = format!(
                                    "\n\n[cloud sync] remote changes saved to {path} (not auto-applied: {reason})."
                                );
                                final_response = format!("{}{suffix}", final_response.trim_end());
                            }
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(error) => {
                let suffix = format!("\n\n[cloud sync] warning: failed to retrieve remote diff: {error}");
                final_response = format!("{}{suffix}", final_response.trim_end());
            }
        }
        RunnerOutcome::Completed { final_response }
    }
}

impl Drop for CloudChildHandle {
    fn drop(&mut self) {
        self.ensure_released();
    }
}

impl ManagedChildHandle for CloudChildHandle {
    fn task_id(&self) -> &str {
        self.inner.task_id()
    }

    fn session_id(&self) -> Option<String> {
        self.inner.session_id()
    }

    fn pid(&self) -> Option<i64> {
        None
    }

    fn steer(&self, text: &str) -> Result<(), HostError> {
        self.inner.steer(text)
    }

    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        self.inner.follow_up(text)
    }

    fn abort(&self) -> Result<(), HostError> {
        self.inner.abort()
    }

    fn subscribe(&self, listener: ManagedChildListener) -> Unsubscribe {
        self.inner.subscribe(listener)
    }

    fn wait_for_outcome(&self) -> RunnerOutcome {
        let raw = self.inner.wait_for_outcome();
        let settled = self.settle_workspace_changes(raw);
        let _ = self.inner.dispose();
        self.ensure_released();
        settled
    }

    fn get_entries(
        &self,
        since: Option<&str>,
    ) -> Option<Result<crate::runners::types::RpcEntriesResult, HostError>> {
        self.inner.get_entries(since)
    }

    fn last_assistant_text(&self) -> Option<String> {
        self.inner.last_assistant_text()
    }

    fn has_terminate(&self) -> bool {
        true
    }

    fn terminate(&self) -> Result<(), HostError> {
        let res = self.inner.terminate();
        self.ensure_released();
        res
    }

    fn dispose(&self) -> Result<(), HostError> {
        let res = self.inner.dispose();
        self.ensure_released();
        res
    }
}

fn apply_patch_to_local(local_cwd: &Path, patch_path: &Path) -> Result<(), String> {
    let direct = Command::new("git")
        .arg("-C")
        .arg(local_cwd)
        .args(["apply", "--whitespace=nowarn"])
        .arg(patch_path)
        .output()
        .map_err(|error| format!("failed to run git apply: {error}"))?;
    if direct.status.success() {
        return Ok(());
    }
    let three_way = Command::new("git")
        .arg("-C")
        .arg(local_cwd)
        .args(["apply", "--3way", "--whitespace=nowarn"])
        .arg(patch_path)
        .output()
        .map_err(|error| format!("failed to run git apply --3way: {error}"))?;
    if three_way.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&direct.stderr);
    Err(stderr.trim().to_string())
}

pub struct CloudManagedRunner {
    backend: Arc<dyn CloudPoolBackend>,
    settings: CloudRunnerSettings,
    now: Clock,
}

impl CloudManagedRunner {
    pub fn new(backend: Arc<dyn CloudPoolBackend>, settings: CloudRunnerSettings) -> Self {
        Self {
            backend,
            settings,
            now: Arc::new(|| chrono::Utc::now().timestamp_millis()),
        }
    }

    pub fn is_available(&self) -> bool {
        self.backend.is_available()
    }
}

impl ManagedRunner for CloudManagedRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        let acquired = self
            .backend
            .acquire(
                &spec.task_id,
                self.settings.provider.as_deref(),
                self.settings.ttl_seconds,
            )
            .map_err(|message| {
                ManagedRunnerError::Runner(RunnerFailure::new(
                    RunnerFailureKind::SessionCreateFailed,
                    format!("cloud box acquire failed: {message}"),
                ))
            })?;

        let local_cwd = PathBuf::from(&spec.cwd);
        let (remote_cwd, sync_ctx) = if self.settings.sync_workspace && local_cwd.is_dir() {
            match self
                .backend
                .sync_in(&acquired.machine, &local_cwd, &spec.task_id)
            {
                Ok(synced) => (
                    synced.remote_cwd,
                    Some(SyncContext {
                        local_cwd,
                        state_dir: PathBuf::from(&spec.state_dir),
                        task_id: spec.task_id.clone(),
                        apply_patch: self.settings.apply_patch,
                    }),
                ),
                Err(error) => {
                    let _ = self.backend.release(
                        &acquired.machine,
                        &acquired.lease_id,
                        self.settings.delete_on_release,
                    );
                    return Err(ManagedRunnerError::Runner(RunnerFailure::new(
                        RunnerFailureKind::SessionCreateFailed,
                        format!("cloud workspace sync-in failed: {error}"),
                    )));
                }
            }
        } else {
            ("/tmp".to_string(), None)
        };

        let client = match RemoteSocketRpcClient::connect(Path::new(&acquired.socket_path)) {
            Ok(client) => client,
            Err(error) => {
                let _ = self.backend.release(
                    &acquired.machine,
                    &acquired.lease_id,
                    self.settings.delete_on_release,
                );
                return Err(ManagedRunnerError::Runner(RunnerFailure::new(
                    RunnerFailureKind::SessionCreateFailed,
                    format!("cloud socket connect failed ({}): {error}", acquired.socket_path),
                )));
            }
        };

        if let Err(error) = client.open_session(&remote_cwd, spec.model.as_deref()) {
            client.detach();
            let _ = self.backend.release(
                &acquired.machine,
                &acquired.lease_id,
                self.settings.delete_on_release,
            );
            return Err(ManagedRunnerError::Runner(RunnerFailure::new(
                RunnerFailureKind::SessionCreateFailed,
                format!("cloud open_session failed: {error}"),
            )));
        }

        let rpc_port: Arc<dyn RpcClientPort> = client;
        let inner = RpcChildHandle::new(RpcChildHandleOptions {
            client: Arc::clone(&rpc_port),
            task_id: spec.task_id.clone(),
            heartbeat_interval_ms: self.settings.heartbeat_interval_ms,
            now: Arc::clone(&self.now),
        });

        let initial_prompt = match spec
            .instructions
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(instructions) => format!("{instructions}\n\n{}", spec.prompt),
            None => spec.prompt.clone(),
        };

        if let Err(error) = inner.start_initial_prompt(&initial_prompt) {
            inner.dispose_handle();
            let _ = self.backend.release(
                &acquired.machine,
                &acquired.lease_id,
                self.settings.delete_on_release,
            );
            return Err(ManagedRunnerError::Runner(RunnerFailure::new(
                RunnerFailureKind::ChildPromptFailed,
                format!("cloud initial prompt failed: {error}"),
            )));
        }

        Ok(Arc::new(CloudChildHandle {
            inner,
            backend: Arc::clone(&self.backend),
            acquired,
            sync_ctx,
            delete_on_release: self.settings.delete_on_release,
            settled_sync: AtomicBool::new(false),
            released: AtomicBool::new(false),
        }))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    struct FakeCloudBackend {
        socket_path: PathBuf,
        patch_content: Option<String>,
        released: Arc<Mutex<Vec<(String, String, bool)>>>,
        synced_in: Arc<Mutex<Vec<String>>>,
    }

    impl CloudPoolBackend for FakeCloudBackend {
        fn is_available(&self) -> bool {
            true
        }

        fn acquire(
            &self,
            _task_id: &str,
            _provider: Option<&str>,
            _ttl_seconds: Option<u64>,
        ) -> Result<AcquiredCloudBox, String> {
            Ok(AcquiredCloudBox {
                machine: "fx-pool-test".into(),
                provider: "daytona".into(),
                transport: "ssh".into(),
                socket_path: self.socket_path.to_string_lossy().into_owned(),
                remote_home: "/home/daytona".into(),
                lease_id: "lease-test-1".into(),
                reused: true,
            })
        }

        fn sync_in(
            &self,
            machine: &str,
            _local_cwd: &Path,
            task_id: &str,
        ) -> Result<SyncedWorkspace, String> {
            self.synced_in
                .lock()
                .unwrap()
                .push(format!("{machine}:{task_id}"));
            Ok(SyncedWorkspace {
                machine: machine.into(),
                task_id: task_id.into(),
                remote_cwd: format!("/home/daytona/.ferryx-cloud/workspaces/{task_id}"),
                file_count: 1,
                archive_bytes: 128,
            })
        }

        fn sync_out(
            &self,
            machine: &str,
            task_id: &str,
            patch_out: &Path,
            _cleanup: bool,
        ) -> Result<SyncOutResult, String> {
            if let Some(patch) = &self.patch_content {
                if let Some(parent) = patch_out.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(patch_out, patch).unwrap();
                Ok(SyncOutResult {
                    machine: machine.into(),
                    task_id: task_id.into(),
                    has_changes: true,
                    changed_files: vec!["M\thello.txt".into()],
                    patch_path: Some(patch_out.to_string_lossy().into_owned()),
                    patch_bytes: patch.len(),
                })
            } else {
                Ok(SyncOutResult {
                    machine: machine.into(),
                    task_id: task_id.into(),
                    has_changes: false,
                    changed_files: vec![],
                    patch_path: None,
                    patch_bytes: 0,
                })
            }
        }

        fn release(&self, machine: &str, lease_id: &str, delete: bool) -> Result<(), String> {
            self.released
                .lock()
                .unwrap()
                .push((machine.into(), lease_id.into(), delete));
            Ok(())
        }
    }

    fn spawn_fake_host_server(socket_path: PathBuf) -> thread::JoinHandle<Vec<Value>> {
        let listener = UnixListener::bind(&socket_path).unwrap();
        thread::spawn(move || {
            let mut received = Vec::new();
            let (stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                let n = reader.read_line(&mut line).unwrap_or(0);
                if n == 0 {
                    break;
                }
                let cmd: Value = serde_json::from_str(line.trim()).unwrap();
                let cmd_type = cmd["type"].as_str().unwrap_or("").to_string();
                let id = cmd["id"].as_str().unwrap_or("").to_string();
                received.push(cmd.clone());
                match cmd_type.as_str() {
                    "open_session" => {
                        let resp = json!({
                            "type": "response",
                            "id": id,
                            "command": "open_session",
                            "success": true,
                            "data": { "sessionId": "rpc-cloud-1" }
                        });
                        writeln!(writer, "{resp}").unwrap();
                    }
                    "prompt" => {
                        let resp = json!({
                            "type": "response",
                            "id": id,
                            "command": "prompt",
                            "success": true,
                            "data": { "disposition": "started" }
                        });
                        writeln!(writer, "{resp}").unwrap();
                        let msg = json!({
                            "type": "message_end",
                            "sessionId": "rpc-cloud-1",
                            "message": {
                                "role": "assistant",
                                "content": [{ "type": "text", "text": "REMOTE_SOCKET_OK" }],
                                "stopReason": "stop"
                            }
                        });
                        writeln!(writer, "{msg}").unwrap();
                        let end = json!({
                            "type": "agent_end",
                            "sessionId": "rpc-cloud-1",
                            "willRetry": false,
                            "messages": []
                        });
                        writeln!(writer, "{end}").unwrap();
                    }
                    "close_session" => {
                        break;
                    }
                    _ => {}
                }
            }
            received
        })
    }

    #[test]
    fn cloud_runner_completes_turn_applies_patch_and_releases_lease() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap();
        Command::new("git").args(["config", "user.email", "t@t"]).current_dir(&repo).status().unwrap();
        Command::new("git").args(["config", "user.name", "t"]).current_dir(&repo).status().unwrap();
        std::fs::write(repo.join("hello.txt"), "before\n").unwrap();
        Command::new("git").args(["add", "."]).current_dir(&repo).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&repo).status().unwrap();

        let socket_path = dir.path().join("fake-cloud.sock");
        let server = spawn_fake_host_server(socket_path.clone());

        let patch = "\
diff --git a/hello.txt b/hello.txt
--- a/hello.txt
+++ b/hello.txt
@@ -1 +1 @@
-before
+after-from-cloud
";
        let released = Arc::new(Mutex::new(Vec::new()));
        let synced_in = Arc::new(Mutex::new(Vec::new()));
        let backend = Arc::new(FakeCloudBackend {
            socket_path,
            patch_content: Some(patch.into()),
            released: Arc::clone(&released),
            synced_in: Arc::clone(&synced_in),
        });
        let runner = CloudManagedRunner::new(backend, CloudRunnerSettings::default());

        let spec = ManagedStartSpec {
            task_id: "st_cloud_1".into(),
            cwd: repo.to_string_lossy().into_owned(),
            state_dir: dir.path().join("state").to_string_lossy().into_owned(),
            prompt: "update hello.txt".into(),
            model: Some("hms/deepseek-v4.1-flash-free".into()),
            ..ManagedStartSpec::default()
        };

        let handle = runner.start(&spec).unwrap();
        let outcome = handle.wait_for_outcome();
        let received = server.join().unwrap();

        match outcome {
            RunnerOutcome::Completed { final_response } => {
                assert!(final_response.contains("REMOTE_SOCKET_OK"), "unexpected text: {final_response}");
                assert!(
                    final_response.contains("[cloud sync] applied 1 file change(s)"),
                    "unexpected text: {final_response}"
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }

        assert_eq!(std::fs::read_to_string(repo.join("hello.txt")).unwrap(), "after-from-cloud\n");
        assert_eq!(synced_in.lock().unwrap().as_slice(), &["fx-pool-test:st_cloud_1"]);
        assert_eq!(
            released.lock().unwrap().as_slice(),
            &[("fx-pool-test".into(), "lease-test-1".into(), false)]
        );
        assert_eq!(received[0]["type"], "open_session");
        assert_eq!(received[0]["provider"], "hms");
        assert_eq!(received[0]["modelId"], "deepseek-v4.1-flash-free");
        assert_eq!(received[1]["type"], "prompt");
        assert_eq!(received[1]["sessionId"], "rpc-cloud-1");
    }
}
