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
use crate::manager::child_handle::Unsubscribe;
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
    fn ensure_socket(&self, _machine: &str) -> Result<String, String> {
        Err("socket re-open not supported by this backend".into())
    }
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
        if let Some(bin) = &self.explicit_bin
            && bin.is_file()
        {
            return Some(bin.clone());
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
        if let Some(home) = std::env::var_os("HOME") {
            let local_bin = PathBuf::from(home).join(".local/bin/ferryx-cli");
            if local_bin.is_file() {
                return Some(local_bin);
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
            if let Some((at, ready)) = *cache
                && at.elapsed() < AVAILABILITY_CACHE_TTL
            {
                return ready;
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

    fn ensure_socket(&self, machine: &str) -> Result<String, String> {
        let bin = self
            .resolve_binary()
            .ok_or_else(|| "ferryx-cli binary not found on PATH or FERRYX_CLI_PATH".to_string())?;
        let output = Command::new(&bin)
            .args(["cloud", "socket", machine])
            .output()
            .map_err(|error| format!("failed to run {}: {error}", bin.display()))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            return Err(format!(
                "ferryx-cli cloud socket {machine} exited {:?}: {}",
                output.status.code(),
                stderr.trim()
            ));
        }
        stdout
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(|line| line.trim().to_string())
            .ok_or_else(|| "ferryx-cli cloud socket printed no socket path".to_string())
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
            return Err(format!(
                "ferryx-cli cloud release failed: {}",
                stderr.trim()
            ));
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

    pub fn open_session(&self, cwd: &str, model: Option<&str>) -> Result<String, RpcClientError> {
        let mut command = json!({
            "type": "open_session",
            "cwd": cwd,
            "kind": "worker",
            "retainOnDisconnect": false,
        });
        if let Some(reference) = model.filter(|m| !m.trim().is_empty())
            && let Some((provider, model_id)) = reference.split_once('/')
            && let Some(obj) = command.as_object_mut()
        {
            obj.insert("provider".into(), Value::String(provider.to_string()));
            obj.insert("modelId".into(), Value::String(model_id.to_string()));
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
            if attach_session && let Some(sid) = session_id {
                obj.entry("sessionId".to_string())
                    .or_insert_with(|| Value::String(sid));
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
        let listeners: Vec<RpcEventListener> = state
            .event_listeners
            .iter()
            .map(|(_, l)| Arc::clone(l))
            .collect();
        (state.session_id.clone(), listeners)
    };
    if let (Some(expected), Some(frame_sid)) = (
        expected_sid.as_deref(),
        parsed.get("sessionId").and_then(Value::as_str),
    ) && frame_sid != expected
    {
        return;
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
        let exit_listeners: Vec<RpcExitListener> = state
            .exit_listeners
            .iter()
            .map(|(_, l)| Arc::clone(l))
            .collect();
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
        Err(io::Error::other(
            "cloud Unix socket transport requires Unix",
        ))
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
            state
                .event_listeners
                .retain(|(existing, _)| *existing != id);
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
    pub(crate) inner: Arc<RpcChildHandle>,
    pub(crate) operation: Mutex<CloudOperation>,
    pub(crate) finished: AtomicBool,
    backend: Arc<dyn CloudPoolBackend>,
    acquired: AcquiredCloudBox,
    sync_ctx: Option<SyncContext>,
    delete_on_release: bool,
    settled_sync: AtomicBool,
    released: AtomicBool,
}

#[derive(Default)]
pub(crate) struct CloudOperation {
    pub(crate) closing: bool,
}

impl CloudChildHandle {
    pub(crate) fn ensure_released(&self) -> Result<(), HostError> {
        if self.released.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        match self.backend.release(
            &self.acquired.machine,
            &self.acquired.lease_id,
            self.delete_on_release,
        ) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.released.store(false, Ordering::Release);
                Err(HostError {
                    message: format!("cloud lease release failed: {error}"),
                })
            }
        }
    }

    pub(crate) fn settle_workspace_changes(&self, outcome: RunnerOutcome) -> RunnerOutcome {
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
        let sync_res =
            self.backend
                .sync_out(&self.acquired.machine, &ctx.task_id, &patch_path, true);
        let RunnerOutcome::Completed { mut final_response } = outcome else {
            if let Err(error) = sync_res {
                utils::logger::log(
                    "senpi-task cloud sync-out failed",
                    Some(&json!({ "taskId": ctx.task_id, "error": error })),
                );
            }
            return outcome;
        };
        match sync_res {
            Ok(out) if out.has_changes => {
                if ctx.apply_patch
                    && let Some(path) = out.patch_path.as_deref()
                {
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
            Ok(_) => {}
            Err(error) => {
                let suffix =
                    format!("\n\n[cloud sync] warning: failed to retrieve remote diff: {error}");
                final_response = format!("{}{suffix}", final_response.trim_end());
            }
        }
        RunnerOutcome::Completed { final_response }
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

    fn release_after_start_failure(&self, acquired: &AcquiredCloudBox, message: String) -> String {
        match self.backend.release(
            &acquired.machine,
            &acquired.lease_id,
            self.settings.delete_on_release,
        ) {
            Ok(()) => message,
            Err(error) => format!("{message}; cloud lease release failed: {error}"),
        }
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
                    return Err(ManagedRunnerError::Runner(RunnerFailure::new(
                        RunnerFailureKind::SessionCreateFailed,
                        self.release_after_start_failure(
                            &acquired,
                            format!("cloud workspace sync-in failed: {error}"),
                        ),
                    )));
                }
            }
        } else {
            ("/tmp".to_string(), None)
        };

        let connect_result = RemoteSocketRpcClient::connect(Path::new(&acquired.socket_path))
            .or_else(
                |first_err| match self.backend.ensure_socket(&acquired.machine) {
                    Ok(reopened_path) => RemoteSocketRpcClient::connect(Path::new(&reopened_path)),
                    Err(_) => Err(first_err),
                },
            );
        let client = match connect_result {
            Ok(client) => client,
            Err(error) => {
                return Err(ManagedRunnerError::Runner(RunnerFailure::new(
                    RunnerFailureKind::SessionCreateFailed,
                    self.release_after_start_failure(
                        &acquired,
                        format!(
                            "cloud socket connect failed ({}): {error}",
                            acquired.socket_path
                        ),
                    ),
                )));
            }
        };

        if let Err(error) = client.open_session(&remote_cwd, spec.model.as_deref()) {
            client.detach();
            return Err(ManagedRunnerError::Runner(RunnerFailure::new(
                RunnerFailureKind::SessionCreateFailed,
                self.release_after_start_failure(
                    &acquired,
                    format!("cloud open_session failed: {error}"),
                ),
            )));
        }

        let rpc_port: Arc<dyn RpcClientPort> = client;
        let inner = RpcChildHandle::new(RpcChildHandleOptions {
            client: Arc::clone(&rpc_port),
            task_id: spec.task_id.clone(),
            heartbeat_interval_ms: self.settings.heartbeat_interval_ms,
            now: Arc::clone(&self.now),
            input_store: None,
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

        let handle = Arc::new(CloudChildHandle {
            inner,
            operation: Mutex::default(),
            finished: AtomicBool::new(false),
            backend: Arc::clone(&self.backend),
            acquired,
            sync_ctx,
            delete_on_release: self.settings.delete_on_release,
            settled_sync: AtomicBool::new(false),
            released: AtomicBool::new(false),
        });
        if let Err(error) = handle.inner.start_initial_prompt(&initial_prompt) {
            let cleanup = crate::lifecycle::destroy::destroy_unregistered(handle.as_ref());
            let cleanup_message = cleanup
                .err()
                .map(|error| format!("; lifecycle cleanup failed: {error}"))
                .unwrap_or_default();
            return Err(ManagedRunnerError::Runner(RunnerFailure::new(
                RunnerFailureKind::ChildPromptFailed,
                format!("cloud initial prompt failed: {error}{cleanup_message}"),
            )));
        }
        Ok(handle)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    use crate::lifecycle::lifecycle_tests::ManagedResident;

    #[derive(Default)]
    struct ManagerRegistry(Mutex<std::sync::Weak<crate::manager::TaskManager>>);

    impl ManagerRegistry {
        fn manager(&self) -> Arc<crate::manager::TaskManager> {
            self.0
                .lock()
                .expect("lock test manager registry")
                .upgrade()
                .expect("manager is alive")
        }
    }

    impl crate::lifecycle::ResidencyRegistry for ManagerRegistry {
        fn get(&self, task_id: &str) -> Option<Arc<dyn crate::lifecycle::ResidentHandle>> {
            self.manager().get_resident_handle(task_id).map(|handle| {
                Arc::new(ManagedResident(handle)) as Arc<dyn crate::lifecycle::ResidentHandle>
            })
        }
        fn entries(&self) -> Vec<Arc<dyn crate::lifecycle::ResidentHandle>> {
            self.manager()
                .resident_task_ids()
                .iter()
                .filter_map(|task_id| self.get(task_id))
                .collect()
        }
        fn forget(&self, task_id: &str) {
            self.manager().forget(task_id);
        }
        fn has_pending_sends(&self, task_id: &str) -> bool {
            self.manager().has_pending_sends(task_id)
        }
        fn try_begin_send(&self, task_id: &str) -> bool {
            self.manager().try_begin_send(task_id)
        }
        fn end_send(&self, task_id: &str) {
            self.manager().end_send(task_id);
        }
    }

    struct LifecycleDestruction {
        lifecycle: crate::lifecycle::TaskLifecycle,
        finished: std::sync::mpsc::Sender<()>,
    }

    impl crate::steering::DestructionPort for LifecycleDestruction {
        fn supports_fenced_eviction(&self) -> bool {
            true
        }
        fn destroy_resident_task(
            &self,
            task_id: &str,
            cause: crate::lifecycle::DestroyCause,
        ) -> Result<(), HostError> {
            let result = self.lifecycle.destroy_resident_task(task_id, cause);
            self.finished.send(()).expect("test watches destruction");
            result.map_err(|error| HostError {
                message: error.to_string(),
            })
        }
        fn evict_managed_if_current(
            &self,
            task_id: &str,
            handle: &Arc<dyn crate::manager::ManagedChildHandle>,
        ) -> Result<(), HostError> {
            let result = self.lifecycle.evict_managed_if_current(task_id, handle);
            self.finished.send(()).expect("test watches destruction");
            result.map_err(|error| HostError {
                message: error.to_string(),
            })
        }
    }

    struct FakeCloudBackend {
        socket_path: PathBuf,
        patch_content: Option<String>,
        released: Arc<Mutex<Vec<(String, String, bool)>>>,
        synced_in: Arc<Mutex<Vec<String>>>,
        fail_release_once: AtomicBool,
        release_gate: Option<(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>)>,
    }

    impl FakeCloudBackend {
        fn workspace(&self, task_id: &str) -> PathBuf {
            self.socket_path
                .parent()
                .expect("fake socket has a parent")
                .join("remote-workspaces")
                .join(task_id)
        }
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
            let workspace = self.workspace(task_id);
            std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
            self.synced_in
                .lock()
                .expect("lock fake synced-in records")
                .push(format!("{machine}:{task_id}"));
            Ok(SyncedWorkspace {
                machine: machine.into(),
                task_id: task_id.into(),
                remote_cwd: workspace.to_string_lossy().into_owned(),
                file_count: 1,
                archive_bytes: 128,
            })
        }

        fn sync_out(
            &self,
            machine: &str,
            task_id: &str,
            patch_out: &Path,
            cleanup: bool,
        ) -> Result<SyncOutResult, String> {
            if cleanup {
                std::fs::remove_dir_all(self.workspace(task_id))
                    .map_err(|error| error.to_string())?;
            }
            if let Some(patch) = &self.patch_content {
                if let Some(parent) = patch_out.parent() {
                    std::fs::create_dir_all(parent).expect("create fake patch directory");
                }
                std::fs::write(patch_out, patch).expect("write fake cloud patch");
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
            if self.fail_release_once.swap(false, Ordering::SeqCst) {
                return Err("fixture release failed".into());
            }
            if let Some((entered, proceed)) = &self.release_gate {
                entered.send(()).expect("release observer");
                proceed
                    .lock()
                    .expect("lock release gate")
                    .recv_timeout(Duration::from_secs(5))
                    .expect("release gate");
            }
            self.released
                .lock()
                .expect("lock fake release records")
                .push((machine.into(), lease_id.into(), delete));
            Ok(())
        }
    }

    fn spawn_fake_host_server(socket_path: PathBuf) -> thread::JoinHandle<Vec<Value>> {
        let listener = UnixListener::bind(&socket_path).expect("bind fake cloud host socket");
        thread::spawn(move || {
            let mut received = Vec::new();
            let (stream, _) = listener.accept().expect("accept cloud host connection");
            let mut writer = stream.try_clone().expect("clone cloud host connection");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                let n = reader
                    .read_line(&mut line)
                    .expect("read cloud host command");
                if n == 0 {
                    break;
                }
                let cmd: Value =
                    serde_json::from_str(line.trim()).expect("parse cloud host command");
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
                        writeln!(writer, "{resp}").expect("send open-session response");
                    }
                    "prompt" => {
                        let resp = json!({
                            "type": "response",
                            "id": id,
                            "command": "prompt",
                            "success": true,
                            "data": { "disposition": "started" }
                        });
                        writeln!(writer, "{resp}").expect("send prompt response");
                        let msg = json!({
                            "type": "message_end",
                            "sessionId": "rpc-cloud-1",
                            "message": {
                                "role": "assistant",
                                "content": [{ "type": "text", "text": "REMOTE_SOCKET_OK" }],
                                "stopReason": "stop"
                            }
                        });
                        writeln!(writer, "{msg}").expect("send assistant message");
                        let end = json!({
                            "type": "agent_end",
                            "sessionId": "rpc-cloud-1",
                            "willRetry": false,
                            "messages": []
                        });
                        writeln!(writer, "{end}").expect("send agent-end event");
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
    fn fake_host_reports_invalid_utf8_instead_of_treating_it_as_eof() {
        let dir = tempfile::tempdir().expect("create fake host directory");
        let socket_path = dir.path().join("invalid-command.sock");
        let server = spawn_fake_host_server(socket_path.clone());
        let mut client =
            std::os::unix::net::UnixStream::connect(socket_path).expect("connect to fake host");
        client
            .write_all(&[0xff, b'\n'])
            .expect("send invalid host command");
        assert!(server.join().is_err());
    }

    struct CloudManagerFixture {
        server: thread::JoinHandle<Vec<Value>>,
        released: Arc<Mutex<Vec<(String, String, bool)>>>,
        store: crate::store::TaskRecordStore,
        manager: Arc<crate::manager::TaskManager>,
        lifecycle: crate::lifecycle::TaskLifecycle,
        destruction_done: mpsc::Receiver<()>,
        task_id: String,
        _dir: tempfile::TempDir,
    }

    fn start_cloud_manager(
        release_gate: Option<(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>)>,
    ) -> CloudManagerFixture {
        use crate::manager::cloud_router::{CloudOffloadConfig, CloudOffloadMode};
        use crate::manager::types::{
            ManagedRunners, ManagerStartSpec, ResolvedChildPlan, StartResult, TaskManagerOptions,
        };
        use crate::store::{StateDirConfig, TaskRecordStore};
        let dir = tempfile::tempdir().expect("create cloud manager fixture directory");
        let socket_path = dir.path().join("manager-cloud.sock");
        let server = spawn_fake_host_server(socket_path.clone());
        let released = Arc::new(Mutex::new(Vec::new()));
        let runner = Arc::new(CloudManagedRunner::new(
            Arc::new(FakeCloudBackend {
                socket_path,
                patch_content: None,
                released: released.clone(),
                synced_in: Arc::new(Mutex::new(Vec::new())),
                fail_release_once: AtomicBool::new(false),
                release_gate,
            }),
            CloudRunnerSettings::default(),
        ));
        let store = TaskRecordStore::new(&StateDirConfig {
            project_dir: dir.path().to_path_buf(),
            task_state_dir: Some(dir.path().join("state")),
        });
        let registry = Arc::new(ManagerRegistry::default());
        let lifecycle =
            crate::lifecycle::create_task_lifecycle(crate::lifecycle::LifecycleDeps::new(
                Arc::new(store.clone()),
                registry.clone(),
                crate::lifecycle::TaskSettings::resolve(&json!({}))
                    .expect("valid test lifecycle settings"),
            ));
        let (finished, destruction_done) = std::sync::mpsc::channel();
        let mut options = TaskManagerOptions::new(
            store.clone(),
            ManagedRunners {
                in_process: runner.clone(),
                process: runner.clone(),
            },
            Arc::new(|_| {
                Ok(ResolvedChildPlan {
                    model: "hms/deepseek-v4.1-flash-free".into(),
                    ..ResolvedChildPlan::default()
                })
            }),
            dir.path().to_string_lossy(),
        );
        options.cloud_runner = Some(runner);
        options.cloud_available = Some(Arc::new(|| true));
        options.cloud_offload = CloudOffloadConfig {
            mode: CloudOffloadMode::Always,
            ..CloudOffloadConfig::default()
        };
        options.destruction = Some(Arc::new(LifecycleDestruction {
            lifecycle: lifecycle.clone(),
            finished,
        }));
        let manager = Arc::new(crate::manager::TaskManager::new(options));
        *registry.0.lock().expect("lock test manager registry") = Arc::downgrade(&manager);
        let StartResult::Started(started) = manager.start(&ManagerStartSpec {
            prompt: "run".into(),
            parent_session_id: "parent-1".into(),
            depth: 1,
            ..ManagerStartSpec::default()
        }) else {
            panic!("cloud task should start");
        };
        CloudManagerFixture {
            _dir: dir,
            server,
            released,
            store,
            manager,
            lifecycle,
            destruction_done,
            task_id: started.task_id,
        }
    }

    #[test]
    fn manager_completion_evicts_cloud_session_and_releases_lease_immediately() {
        use crate::state::{ResidencyState, TaskStatus};
        let fixture = start_cloud_manager(None);
        fixture
            .destruction_done
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("completion must invoke lifecycle eviction");
        let record = fixture
            .manager
            .wait_for(&fixture.task_id, None, Some(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(record.status, TaskStatus::Completed);
        assert_eq!(record.residency_state, ResidencyState::Evicted);
        assert_eq!(fixture.released.lock().unwrap().len(), 1);
        assert!(
            fixture
                .manager
                .get_resident_handle(&fixture.task_id)
                .is_none()
        );
        let received = fixture.server.join().unwrap();
        assert_eq!(
            received
                .iter()
                .filter(|cmd| cmd["type"] == "close_session")
                .count(),
            1
        );
    }

    #[test]
    fn completion_and_concurrent_eviction_close_cloud_once() {
        let (entered, release_entered) = mpsc::channel();
        let (proceed, release_gate) = mpsc::channel();
        let fixture = start_cloud_manager(Some((entered, Mutex::new(release_gate))));
        release_entered
            .recv_timeout(Duration::from_secs(5))
            .expect("completion eviction entered cloud lease release");
        let before = fixture.store.load(&fixture.task_id).unwrap().unwrap();
        assert_eq!(before.status, crate::state::TaskStatus::Running);
        let outcome = fixture
            .manager
            .send_to_task(&crate::steering::SendInput::new(
                &fixture.task_id,
                "must not enter the removed workspace",
            ));
        assert!(outcome.is_err(), "input must be rejected during cleanup");
        let after = fixture.store.load(&fixture.task_id).unwrap().unwrap();
        assert_eq!(after.status, before.status);
        assert_eq!(after.notification.run_epoch, before.notification.run_epoch);
        let (contended, second_contended) = mpsc::channel();
        *fixture
            .lifecycle
            .context()
            .on_destroy_contended
            .lock()
            .unwrap() = Some(Arc::new(move |_| {
            contended.send(()).unwrap();
        }));
        let lifecycle = fixture.lifecycle.clone();
        let task_id = fixture.task_id.clone();
        let competing = thread::spawn(move || {
            lifecycle.destroy_resident_task(&task_id, crate::lifecycle::DestroyCause::Evict)
        });
        second_contended
            .recv_timeout(Duration::from_secs(5))
            .expect("explicit eviction must overlap completion eviction");
        proceed.send(()).unwrap();
        fixture
            .destruction_done
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        competing.join().unwrap().unwrap();
        let done = fixture
            .manager
            .wait_for(&fixture.task_id, None, Some(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(done.status, crate::state::TaskStatus::Completed);
        assert_eq!(fixture.released.lock().unwrap().len(), 1);
        assert!(
            fixture
                .manager
                .get_resident_handle(&fixture.task_id)
                .is_none()
        );
        let received = fixture.server.join().unwrap();
        assert_eq!(
            received
                .iter()
                .filter(|cmd| cmd["type"] == "close_session")
                .count(),
            1
        );
    }

    #[test]
    fn completion_and_concurrent_shutdown_close_cloud_once() {
        let (entered, release_entered) = mpsc::channel();
        let (proceed, release_gate) = mpsc::channel();
        let fixture = start_cloud_manager(Some((entered, Mutex::new(release_gate))));
        release_entered
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let (contended, shutdown_contended) = mpsc::channel();
        *fixture
            .lifecycle
            .context()
            .on_destroy_contended
            .lock()
            .unwrap() = Some(Arc::new(move |_| {
            contended.send(()).unwrap();
        }));
        let lifecycle = fixture.lifecycle.clone();
        let shutdown = thread::spawn(move || {
            lifecycle.suspend_on_session_shutdown(&crate::lifecycle::SuspendInput {
                parent_session_id: "parent-1".into(),
                own_task_id: None,
                reason: "quit".into(),
            })
        });
        shutdown_contended
            .recv_timeout(Duration::from_secs(5))
            .expect("shutdown must overlap completion eviction");
        proceed.send(()).unwrap();
        fixture
            .destruction_done
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        shutdown.join().unwrap().unwrap();
        let done = fixture
            .manager
            .wait_for(&fixture.task_id, None, Some(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(done.status, crate::state::TaskStatus::Completed);
        assert_eq!(done.residency_state, crate::state::ResidencyState::Evicted);
        assert_eq!(fixture.released.lock().unwrap().len(), 1);
        let received = fixture.server.join().unwrap();
        assert_eq!(
            received
                .iter()
                .filter(|cmd| cmd["type"] == "close_session")
                .count(),
            1
        );
    }

    #[test]
    fn lifecycle_reports_a_failed_cloud_release_and_can_retry_it() {
        let dir = tempfile::tempdir().expect("create cloud fixture directory");
        let socket_path = dir.path().join("cloud.sock");
        let server = spawn_fake_host_server(socket_path.clone());
        let released = Arc::new(Mutex::new(Vec::new()));
        let backend = Arc::new(FakeCloudBackend {
            socket_path,
            patch_content: None,
            released: released.clone(),
            synced_in: Arc::new(Mutex::new(Vec::new())),
            fail_release_once: AtomicBool::new(true),
            release_gate: None,
        });
        let runner = CloudManagedRunner::new(backend, CloudRunnerSettings::default());
        let handle = runner
            .start(&ManagedStartSpec {
                task_id: "st_0000000a".into(),
                cwd: dir.path().to_string_lossy().into_owned(),
                state_dir: dir.path().join("state").to_string_lossy().into_owned(),
                prompt: "run".into(),
                ..ManagedStartSpec::default()
            })
            .expect("start cloud runner");
        assert!(matches!(
            handle.wait_for_outcome(),
            RunnerOutcome::Completed { .. }
        ));

        assert!(crate::lifecycle::lifecycle_tests::evict_managed_handle(handle.clone()).is_err());
        assert!(released.lock().unwrap().is_empty());
        crate::lifecycle::lifecycle_tests::evict_managed_handle(handle)
            .expect("retry cloud lease release");
        assert_eq!(released.lock().unwrap().len(), 1);
        server.join().expect("fake host exited");
    }

    #[test]
    fn cloud_runner_completes_turn_applies_patch_and_releases_lease() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@t"])
            .current_dir(&repo)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(&repo)
            .status()
            .unwrap();
        std::fs::write(repo.join("hello.txt"), "before\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(&repo)
            .status()
            .unwrap();

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
            fail_release_once: AtomicBool::new(false),
            release_gate: None,
        });
        let runner = CloudManagedRunner::new(backend, CloudRunnerSettings::default());

        let spec = ManagedStartSpec {
            task_id: "st_0000000b".into(),
            cwd: repo.to_string_lossy().into_owned(),
            state_dir: dir.path().join("state").to_string_lossy().into_owned(),
            prompt: "update hello.txt".into(),
            model: Some("hms/deepseek-v4.1-flash-free".into()),
            ..ManagedStartSpec::default()
        };

        let handle = runner.start(&spec).unwrap();
        let remote_workspace = dir.path().join("remote-workspaces").join(&spec.task_id);
        assert!(
            remote_workspace.is_dir(),
            "the remote workspace starts present"
        );
        let outcome = handle.wait_for_outcome();
        assert!(
            !remote_workspace.exists(),
            "sync-out removes the remote workspace"
        );
        assert!(
            handle
                .follow_up("must not use the removed workspace")
                .is_err(),
            "a completed cloud handle must reject input before lifecycle eviction"
        );
        assert!(
            released.lock().unwrap().is_empty(),
            "a completed turn must not release a resident lease"
        );
        crate::lifecycle::lifecycle_tests::evict_managed_handle(handle.clone())
            .expect("evict cloud handle");
        let received = server.join().unwrap();

        match outcome {
            RunnerOutcome::Completed { final_response } => {
                assert!(
                    final_response.contains("REMOTE_SOCKET_OK"),
                    "unexpected text: {final_response}"
                );
                assert!(
                    final_response.contains("[cloud sync] applied 1 file change(s)"),
                    "unexpected text: {final_response}"
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }

        assert_eq!(
            std::fs::read_to_string(repo.join("hello.txt")).unwrap(),
            "after-from-cloud\n"
        );
        assert_eq!(
            synced_in.lock().unwrap().as_slice(),
            &["fx-pool-test:st_0000000b"]
        );
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
