//! Port of `ensure-daemon.ts`: authenticated probe, detached spawn, readiness polling.

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lsp_core::abort::AbortSignal;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::ipc_protocol::{auth_envelope, read_auth_token};
use crate::ownership::{DaemonOwner, OwnerPing};
use crate::paths::{DaemonPaths, packaged_runtime_defaults};
use crate::runtime_contract::{
    DaemonRuntimeDefaults, Env, InvalidRuntimeOverrideError, resolve_daemon_runtime,
};
use crate::socket_jsonrpc::{LineBuffer, encode_json_line};
use crate::transport;

pub use crate::runtime_contract::OMO_LSP_DAEMON_CLI;

pub const PROBE_TIMEOUT_MS: u64 = 500;
const DEFAULT_READY_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_POLL_INTERVAL_MS: u64 = 100;

/// Failures of `ensure_daemon_running`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureDaemonError {
    /// TS `DaemonUnreachableError`.
    Unreachable { socket: PathBuf },
    /// The startup signal fired (TS rejects with the abort reason).
    Aborted(String),
    /// Spawning the daemon process failed synchronously.
    Spawn(String),
}

impl fmt::Display for EnsureDaemonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable { socket } => {
                write!(
                    formatter,
                    "LSP daemon did not become reachable at {}",
                    socket.display()
                )
            }
            Self::Aborted(reason) | Self::Spawn(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for EnsureDaemonError {}

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

pub type SpawnDaemonFn = Arc<dyn Fn(&DaemonPaths) -> std::io::Result<()> + Send + Sync>;
pub type SleepFn = Arc<dyn Fn(u64, Option<AbortSignal>) -> BoxFuture<()> + Send + Sync>;
pub type ProbeFn = Arc<dyn Fn(DaemonPaths, Option<AbortSignal>) -> BoxFuture<bool> + Send + Sync>;

/// TS `EnsureDaemonDeps`.
#[derive(Clone)]
pub struct EnsureDaemonDeps {
    pub probe: ProbeFn,
    pub spawn_daemon: SpawnDaemonFn,
    pub sleep: SleepFn,
    pub now: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Default for EnsureDaemonDeps {
    fn default() -> Self {
        let epoch = Instant::now();
        Self {
            probe: Arc::new(|paths, signal| {
                Box::pin(async move { probe_daemon(&paths, PROBE_TIMEOUT_MS, signal).await })
            }),
            spawn_daemon: Arc::new(spawn_daemon_process),
            sleep: Arc::new(|ms, _signal| Box::pin(tokio::time::sleep(Duration::from_millis(ms)))),
            now: Arc::new(move || u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)),
        }
    }
}

/// TS `EnsureDaemonOptions`.
#[derive(Clone, Default)]
pub struct EnsureDaemonOptions {
    pub ready_timeout_ms: Option<u64>,
    pub poll_interval_ms: Option<u64>,
    pub signal: Option<AbortSignal>,
}

/// TS `ensureDaemonRunning`.
pub async fn ensure_daemon_running(
    paths: &DaemonPaths,
    deps: &EnsureDaemonDeps,
    options: EnsureDaemonOptions,
) -> Result<(), EnsureDaemonError> {
    let ready_timeout_ms = options.ready_timeout_ms.unwrap_or(DEFAULT_READY_TIMEOUT_MS);
    let poll_interval_ms = options.poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);
    let signal = options.signal;
    throw_if_aborted(signal.as_ref())?;
    if await_with_signal((deps.probe)(paths.clone(), signal.clone()), signal.as_ref()).await? {
        return Ok(());
    }
    throw_if_aborted(signal.as_ref())?;
    (deps.spawn_daemon)(paths).map_err(|error| EnsureDaemonError::Spawn(error.to_string()))?;
    let deadline = (deps.now)().saturating_add(ready_timeout_ms);
    loop {
        throw_if_aborted(signal.as_ref())?;
        if await_with_signal((deps.probe)(paths.clone(), signal.clone()), signal.as_ref()).await? {
            return Ok(());
        }
        if (deps.now)() >= deadline {
            return Err(EnsureDaemonError::Unreachable {
                socket: paths.socket.clone(),
            });
        }
        await_with_signal(
            (deps.sleep)(poll_interval_ms, signal.clone()),
            signal.as_ref(),
        )
        .await?;
    }
}

fn abort_error(signal: &AbortSignal) -> EnsureDaemonError {
    EnsureDaemonError::Aborted(signal.reason().map_or_else(
        || "daemon startup cancelled".to_string(),
        |reason| reason.to_string(),
    ))
}

fn throw_if_aborted(signal: Option<&AbortSignal>) -> Result<(), EnsureDaemonError> {
    match signal {
        Some(signal) if signal.aborted() => Err(abort_error(signal)),
        _ => Ok(()),
    }
}

async fn await_with_signal<T>(
    future: BoxFuture<T>,
    signal: Option<&AbortSignal>,
) -> Result<T, EnsureDaemonError> {
    let Some(signal) = signal else {
        return Ok(future.await);
    };
    tokio::select! {
        biased;
        () = signal.cancelled() => Err(abort_error(signal)),
        value = future => Ok(value),
    }
}

/// TS `probeDaemon`: authenticated ping succeeds.
pub async fn probe_daemon(
    paths: &DaemonPaths,
    timeout_ms: u64,
    signal: Option<AbortSignal>,
) -> bool {
    let Some(token) = read_auth_token(paths) else {
        return false;
    };
    ping_daemon(paths, &token, timeout_ms, signal)
        .await
        .is_some()
}

/// TS `pingDaemon`: `omo/ping` over the socket; any failure, timeout or abort is `None`.
pub async fn ping_daemon(
    paths: &DaemonPaths,
    token: &str,
    timeout_ms: u64,
    signal: Option<AbortSignal>,
) -> Option<OwnerPing> {
    if signal.as_ref().is_some_and(AbortSignal::aborted) {
        return None;
    }
    let exchange = async {
        let mut stream = transport::connect(&paths.socket).await.ok()?;
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "omo/ping", "params": {"_omo": auth_envelope(token)}});
        stream
            .write_all(encode_json_line(&request).as_bytes())
            .await
            .ok()?;
        let mut decoder = LineBuffer::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            // The first complete line decides the outcome, parsed or not.
            if let Some(line) = decoder.push(&chunk[..read]).into_iter().next() {
                return line
                    .ok()
                    .and_then(|message| DaemonOwner::from_ping_response(&message));
            }
        }
    };
    let timed = tokio::time::timeout(Duration::from_millis(timeout_ms), exchange);
    match signal {
        Some(signal) => tokio::select! {
            biased;
            () = signal.cancelled() => None,
            result = timed => result.ok().flatten(),
        },
        None => timed.await.ok().flatten(),
    }
}

/// TS `resolveDaemonNodeExecutable`: launcher for a Node `cli.js` daemon override.
pub fn resolve_daemon_node_executable(
    cached_exec_path: &str,
    original_argv0: &str,
    path_exists: impl Fn(&str) -> bool,
) -> String {
    if path_exists(cached_exec_path) {
        return cached_exec_path.to_string();
    }
    if Path::new(original_argv0).is_absolute() && path_exists(original_argv0) {
        return original_argv0.to_string();
    }
    "node".to_string()
}

/// TS `resolveDaemonCliPath`.
pub fn resolve_daemon_cli_path(
    env: &Env,
    defaults: &DaemonRuntimeDefaults,
) -> Result<PathBuf, InvalidRuntimeOverrideError> {
    resolve_daemon_runtime(env, defaults).map(|runtime| runtime.cli_path)
}

/// Program and arguments that start the daemon for `cli_path`.
///
/// A `.js` override keeps the Node launch contract (`node <cli.js> daemon`); anything
/// else is treated as this native binary and run directly as `<cli> daemon`.
pub fn daemon_launch_command(cli_path: &Path) -> (PathBuf, Vec<PathBuf>) {
    let is_node_script = cli_path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("js") || extension.eq_ignore_ascii_case("mjs")
    });
    if is_node_script {
        // No Node runtime is embedded, so the PATH-resolved `node` fallback applies.
        return (
            PathBuf::from("node"),
            vec![cli_path.to_path_buf(), PathBuf::from("daemon")],
        );
    }
    (cli_path.to_path_buf(), vec![PathBuf::from("daemon")])
}

/// TS `spawnDaemonProcess`: detached daemon with stdout/stderr appended to the log.
pub fn spawn_daemon_process(paths: &DaemonPaths) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = paths.log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log)?;
    let (program, args) = daemon_launch_command(&paths.cli_path);
    let mut command = std::process::Command::new(&program);
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    match command.spawn() {
        // The child is intentionally not waited on (TS `child.unref()`).
        Ok(_child) => Ok(()),
        Err(error) => {
            let mut log = log;
            let _ignored = writeln!(log, "[lsp-daemon] failed to spawn daemon: {error}");
            Ok(())
        }
    }
}

/// Default runtime defaults re-exported for callers that resolve the CLI path.
pub fn default_runtime_defaults() -> DaemonRuntimeDefaults {
    packaged_runtime_defaults()
}

#[cfg(test)]
#[path = "ensure_daemon_tests.rs"]
mod tests;
