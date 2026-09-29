//! Port of `daemon-server.ts` + `run-daemon.ts`: the long-lived socket server.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use tokio::task::{AbortHandle, JoinSet};

use crate::ensure_daemon::{PROBE_TIMEOUT_MS, ping_daemon};
use crate::ownership::{
    DaemonOwner, StartupError, acquire_startup_lease, endpoint_identity,
    remove_daemon_metadata_for_owner, write_daemon_owner,
};
use crate::paths::DaemonPaths;
use crate::platform;
use crate::request_routing::{
    ActiveRequests, DaemonRouteState, Dispatch, core_dispatch, handle_daemon_message,
};
use crate::socket_jsonrpc::{LineBuffer, encode_json_line};
use crate::transport::{Listener, Stream};
use crate::version_reap::{ReapStaleDaemonVersionsDeps, reap_stale_daemon_versions};

const DEFAULT_IDLE_SHUTDOWN_MS: u64 = 30 * 60_000;
const DEFAULT_IDLE_CHECK_INTERVAL_MS: u64 = 60_000;

pub type IdleHook = Arc<dyn Fn() + Send + Sync>;

/// TS `DaemonServerOptions`, plus the core dispatcher seam and a reap switch.
#[derive(Clone, Default)]
pub struct DaemonServerOptions {
    pub idle_shutdown_ms: Option<u64>,
    pub idle_check_interval_ms: Option<u64>,
    /// Replaces the default idle action (close the server).
    pub on_idle_shutdown: Option<IdleHook>,
    /// Replaces lsp-core dispatch (tests); `None` uses [`core_dispatch`].
    pub dispatch: Option<Dispatch>,
    /// Skips the background reap of older version directories.
    pub skip_version_reap: bool,
}

struct Inner {
    paths: DaemonPaths,
    owner: DaemonOwner,
    closed: AtomicBool,
    closed_notify: Notify,
    connections: AtomicUsize,
    last_active: Mutex<Instant>,
    tasks: Mutex<Vec<AbortHandle>>,
    connection_tasks: Mutex<HashMap<u64, AbortHandle>>,
}

impl Inner {
    fn touch(&self) {
        *self
            .last_active
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.last_active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .elapsed()
    }
}

/// TS `DaemonServerHandle`.
#[derive(Clone)]
pub struct DaemonServerHandle {
    inner: Arc<Inner>,
}

impl DaemonServerHandle {
    pub fn owner(&self) -> &DaemonOwner {
        &self.inner.owner
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Resolves once `close` has finished (including idle shutdown).
    pub async fn closed(&self) {
        loop {
            let notified = self.inner.closed_notify.notified();
            if self.is_closed() {
                return;
            }
            notified.await;
        }
    }

    /// TS `close`: idempotent; drops every connection and removes owned metadata.
    pub async fn close(&self) {
        close_inner(&self.inner).await;
    }
}

async fn close_inner(inner: &Arc<Inner>) {
    if inner.closed.swap(true, Ordering::SeqCst) {
        return;
    }
    for task in inner
        .tasks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain(..)
    {
        task.abort();
    }
    for (_, task) in inner
        .connection_tasks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain()
    {
        task.abort();
    }
    remove_daemon_metadata_for_owner(&inner.paths, &inner.owner);
    lsp_core::lsp::manager::dispose_default_lsp_manager().await;
    inner.closed_notify.notify_waiters();
}

/// TS `startDaemonServer`. Must run inside a tokio runtime.
pub async fn start_daemon_server(
    paths: &DaemonPaths,
    options: DaemonServerOptions,
) -> Result<DaemonServerHandle, StartupError> {
    let idle_shutdown_ms = options.idle_shutdown_ms.unwrap_or(DEFAULT_IDLE_SHUTDOWN_MS);
    let idle_check_interval_ms = options
        .idle_check_interval_ms
        .unwrap_or(DEFAULT_IDLE_CHECK_INTERVAL_MS);
    let ping_paths = paths.clone();
    let lease = acquire_startup_lease(paths, move |token| {
        let paths = ping_paths.clone();
        async move { ping_daemon(&paths, &token, PROBE_TIMEOUT_MS, None).await }
    })
    .await?;

    let bound = bind_and_publish(paths, &lease.owner).await;
    let (listener, owner) = match bound {
        Ok(bound) => bound,
        Err(error) => {
            lease.lock.release();
            return Err(error);
        }
    };
    let inner = Arc::new(Inner {
        paths: paths.clone(),
        owner,
        closed: AtomicBool::new(false),
        closed_notify: Notify::new(),
        connections: AtomicUsize::new(0),
        last_active: Mutex::new(Instant::now()),
        tasks: Mutex::new(Vec::new()),
        connection_tasks: Mutex::new(HashMap::new()),
    });
    let state = DaemonRouteState {
        token: lease.token.clone(),
        owner: inner.owner.clone(),
        active_requests: None,
        dispatch: options.dispatch.clone().unwrap_or_else(core_dispatch),
    };
    let accept = tokio::spawn(accept_loop(listener, Arc::clone(&inner), state));
    inner
        .tasks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(accept.abort_handle());

    if let Err(error) = assert_self_probe(paths, &lease.token, &inner.owner).await {
        lease.lock.release();
        close_inner(&inner).await;
        return Err(error);
    }
    lease.lock.release();

    if !options.skip_version_reap {
        let reap_paths = paths.clone();
        drop(tokio::task::spawn_blocking(move || {
            reap_stale_daemon_versions(&reap_paths, &ReapStaleDaemonVersionsDeps::default())
        }));
    }

    let idle = tokio::spawn(idle_loop(
        Arc::clone(&inner),
        idle_shutdown_ms,
        idle_check_interval_ms,
        options.on_idle_shutdown,
    ));
    inner
        .tasks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(idle.abort_handle());
    Ok(DaemonServerHandle { inner })
}

async fn bind_and_publish(
    paths: &DaemonPaths,
    lease_owner: &DaemonOwner,
) -> Result<(Listener, DaemonOwner), StartupError> {
    let listener = Listener::bind(&paths.socket)?;
    if !cfg!(windows) {
        platform::set_socket_mode(&paths.socket)?;
    }
    let owner = DaemonOwner {
        endpoint: endpoint_identity(&paths.socket.to_string_lossy()),
        ..lease_owner.clone()
    };
    write_daemon_owner(paths, &owner)?;
    Ok((listener, owner))
}

async fn assert_self_probe(
    paths: &DaemonPaths,
    token: &str,
    owner: &DaemonOwner,
) -> Result<(), StartupError> {
    for file in [&paths.pid, &paths.endpoint, &paths.owner] {
        platform::set_private_file_mode(file)?;
    }
    match ping_daemon(paths, token, PROBE_TIMEOUT_MS, None).await {
        Some(ping) if ping.nonce == owner.nonce => Ok(()),
        _ => Err(StartupError::Deferred("self_probe_failed")),
    }
}

async fn accept_loop(listener: Listener, inner: Arc<Inner>, state: DaemonRouteState) {
    static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);
    loop {
        let stream = match listener.accept().await {
            Ok(stream) => stream,
            Err(error) => {
                log_server_error(&error.to_string());
                continue;
            }
        };
        let id = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
        inner.connections.fetch_add(1, Ordering::SeqCst);
        inner.touch();
        let task = tokio::spawn(serve_connection(
            stream,
            Arc::clone(&inner),
            state.clone(),
            id,
        ));
        inner
            .connection_tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, task.abort_handle());
    }
}

/// Decrements the live-connection count however the connection task ends.
struct ConnectionGuard {
    inner: Arc<Inner>,
    id: u64,
    active: ActiveRequests,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        for (_, entry) in self
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
        {
            entry.controller.abort();
        }
        self.inner
            .connection_tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
        self.inner.connections.fetch_sub(1, Ordering::SeqCst);
        self.inner.touch();
    }
}

async fn serve_connection(stream: Stream, inner: Arc<Inner>, mut state: DaemonRouteState, id: u64) {
    let active: ActiveRequests = Arc::default();
    state.active_requests = Some(Arc::clone(&active));
    let _guard = ConnectionGuard {
        inner: Arc::clone(&inner),
        id,
        active,
    };
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let mut requests = JoinSet::new();
    let mut decoder = LineBuffer::new();
    let mut chunk = [0_u8; 8192];
    loop {
        tokio::select! {
            read = reader.read(&mut chunk) => {
                let Ok(read) = read else { break };
                if read == 0 {
                    break;
                }
                for message in decoder.push(&chunk[..read]).into_iter().filter_map(Result::ok) {
                    inner.touch();
                    requests.spawn(respond(message, state.clone(), Arc::clone(&writer)));
                }
            }
            Some(_finished) = requests.join_next(), if !requests.is_empty() => {}
        }
    }
}

async fn respond(
    message: Value,
    state: DaemonRouteState,
    writer: Arc<tokio::sync::Mutex<tokio::io::WriteHalf<Stream>>>,
) {
    let Some(response) = handle_daemon_message(&message, &state).await else {
        return;
    };
    let mut writer = writer.lock().await;
    if let Err(error) = writer
        .write_all(encode_json_line(&response).as_bytes())
        .await
    {
        log_server_error(&error.to_string());
    }
}

async fn idle_loop(
    inner: Arc<Inner>,
    idle_shutdown_ms: u64,
    check_interval_ms: u64,
    on_idle: Option<IdleHook>,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(check_interval_ms.max(1)));
    interval.tick().await;
    loop {
        interval.tick().await;
        if inner.connections.load(Ordering::SeqCst) > 0 {
            continue;
        }
        if lsp_core::lsp::manager::get_lsp_manager().client_count() > 0 {
            inner.touch();
            continue;
        }
        if inner.idle_for() < Duration::from_millis(idle_shutdown_ms) {
            continue;
        }
        if let Some(hook) = &on_idle {
            hook();
            continue;
        }
        // Closing aborts this task, so hand the close to a fresh task.
        let closing = Arc::clone(&inner);
        drop(tokio::spawn(async move { close_inner(&closing).await }));
        return;
    }
}

fn log_server_error(message: &str) {
    eprintln!("[lsp-daemon] server error: {message}");
}

/// TS `runDaemon`: serve until idle shutdown or SIGTERM/SIGINT. A reachable existing
/// owner is success (exit 0).
pub async fn run_daemon(paths: &DaemonPaths) -> Result<(), StartupError> {
    let handle = match start_daemon_server(paths, DaemonServerOptions::default()).await {
        Ok(handle) => handle,
        Err(StartupError::AlreadyRunning) => return Ok(()),
        Err(error) => return Err(error),
    };
    wait_for_shutdown_signal(&handle).await;
    handle.close().await;
    Ok(())
}

#[cfg(unix)]
async fn wait_for_shutdown_signal(handle: &DaemonServerHandle) {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut interrupt)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        handle.closed().await;
        return;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = interrupt.recv() => {}
        () = handle.closed() => {}
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal(handle: &DaemonServerHandle) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = handle.closed() => {}
    }
}
