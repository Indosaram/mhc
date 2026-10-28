//! Shared pool of started language servers keyed by workspace root + server id.
use crate::client::{ClientTimings, LspClient};
use crate::config::ResolvedServer;
use crate::error::LspError;
use crate::limits;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// Whether clients link through the `mhc-lsp` daemon.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DaemonMode {
    /// Always spawn servers in this process.
    #[default]
    Off,
    /// Use the daemon (starting it when needed) and fall back to a local spawn on any failure.
    Auto {
        /// The `mhc-lsp` executable.
        binary: PathBuf,
        /// Socket path.
        socket: PathBuf,
    },
}

/// Pool settings.
#[derive(Clone, Debug)]
pub struct LspManagerOptions {
    pub idle_timeout: Duration,
    pub init_timeout: Duration,
    pub reaper_interval: Duration,
    pub timings: ClientTimings,
    pub daemon: DaemonMode,
}

impl Default for LspManagerOptions {
    fn default() -> Self {
        Self {
            idle_timeout: limits::IDLE_TIMEOUT,
            init_timeout: limits::INIT_TIMEOUT,
            reaper_interval: limits::REAPER_INTERVAL,
            timings: ClientTimings::default(),
            daemon: DaemonMode::Off,
        }
    }
}

type InitState = Option<Result<Arc<LspClient>, LspError>>;

struct Slot {
    client: Arc<LspClient>,
    ready: watch::Receiver<InitState>,
    ref_count: usize,
    waiters: usize,
    last_used: Instant,
    init_started: Option<Instant>,
    init_timeout: Duration,
}

/// One row of [`LspManager::snapshot`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientSnapshot {
    pub root: PathBuf,
    pub server_id: String,
    pub ref_count: usize,
    pub pending_waiters: usize,
    pub idle_for: Duration,
    pub initializing: bool,
    pub alive: bool,
    pub via_daemon: bool,
    pub command: Vec<String>,
}

/// Starts each (root, server) once, shares it between callers and stops it when idle.
pub struct LspManager {
    options: LspManagerOptions,
    slots: Mutex<HashMap<(PathBuf, String), Slot>>,
    reaper_started: Mutex<bool>,
    disposed: Mutex<bool>,
    me: Weak<LspManager>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn stop_later(client: Arc<LspClient>) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move { client.stop().await });
    }
}

impl LspManager {
    pub fn new(options: LspManagerOptions) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            options,
            slots: Mutex::default(),
            reaper_started: Mutex::new(false),
            disposed: Mutex::new(false),
            me: me.clone(),
        })
    }

    pub fn options(&self) -> &LspManagerOptions {
        &self.options
    }

    fn ensure_reaper(&self) {
        let mut started = lock(&self.reaper_started);
        if *started {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        *started = true;
        let me = self.me.clone();
        let interval = self.options.reaper_interval;
        handle.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let Some(manager) = me.upgrade() else { break };
                if *lock(&manager.disposed) {
                    break;
                }
                manager.reap_stale();
            }
        });
    }

    /// Stops idle clients past the idle timeout and clients stuck initializing.
    pub fn reap_stale(&self) {
        let now = Instant::now();
        let mut stale = Vec::new();
        lock(&self.slots).retain(|_, slot| {
            let stuck = slot
                .init_started
                .is_some_and(|started| now.duration_since(started) > slot.init_timeout);
            let idle = slot.init_started.is_none()
                && slot.ref_count == 0
                && slot.waiters == 0
                && now.duration_since(slot.last_used) > self.options.idle_timeout;
            if stuck || idle {
                stale.push(Arc::clone(&slot.client));
            }
            !(stuck || idle)
        });
        stale.into_iter().for_each(stop_later);
    }

    fn spawn_init(
        &self,
        root: &Path,
        server: &ResolvedServer,
    ) -> (Arc<LspClient>, watch::Receiver<InitState>) {
        let client = Arc::new(LspClient::new(
            root.to_path_buf(),
            server.clone(),
            self.options.timings,
        ));
        let (sender, receiver) = watch::channel::<InitState>(None);
        let starting = Arc::clone(&client);
        let daemon = self.options.daemon.clone();
        let init_timeout = server.init_timeout.unwrap_or(self.options.init_timeout);
        tokio::spawn(async move {
            let started = async {
                link_or_spawn(&starting, &daemon).await?;
                starting.initialize().await
            };
            let outcome = match tokio::time::timeout(init_timeout, started).await {
                Ok(Ok(())) => Ok(Arc::clone(&starting)),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(LspError::RequestTimeout {
                    method: "initialize".to_owned(),
                    stderr_tail: None,
                }),
            };
            if outcome.is_err() {
                starting.stop().await;
            }
            let _sent = sender.send(Some(outcome));
        });
        (client, receiver)
    }

    /// Returns a started, initialized client and takes a reference on it; pair with
    /// [`LspManager::release_client`].
    pub async fn get_client(
        &self,
        root: &Path,
        server: &ResolvedServer,
    ) -> Result<Arc<LspClient>, LspError> {
        for _attempt in 0..3 {
            if *lock(&self.disposed) {
                return Err(LspError::Other("LspManager has been disposed".to_owned()));
            }
            self.ensure_reaper();
            let key = (root.to_path_buf(), server.id.clone());
            let mut ready = {
                let mut slots = lock(&self.slots);
                let now = Instant::now();
                let stuck = slots.get(&key).is_some_and(|slot| {
                    slot.init_started
                        .is_some_and(|started| now.duration_since(started) > slot.init_timeout)
                });
                if stuck && let Some(slot) = slots.remove(&key) {
                    stop_later(slot.client);
                }
                if !slots.contains_key(&key) {
                    let (client, ready) = self.spawn_init(root, server);
                    slots.insert(
                        key.clone(),
                        Slot {
                            client,
                            ready,
                            ref_count: 0,
                            waiters: 0,
                            last_used: now,
                            init_started: Some(now),
                            init_timeout: server.init_timeout.unwrap_or(self.options.init_timeout),
                        },
                    );
                }
                let slot = slots
                    .get_mut(&key)
                    .ok_or_else(|| LspError::Other("slot vanished".to_owned()))?;
                slot.waiters += 1;
                slot.ready.clone()
            };
            let state = ready
                .wait_for(Option::is_some)
                .await
                .map(|state| state.clone())
                .ok()
                .flatten();
            let mut slots = lock(&self.slots);
            let slot = slots.get_mut(&key);
            match state {
                Some(Ok(client)) => {
                    let Some(slot) = slot.filter(|slot| Arc::ptr_eq(&slot.client, &client)) else {
                        continue;
                    };
                    slot.waiters = slot.waiters.saturating_sub(1);
                    slot.init_started = None;
                    if !client.is_alive() {
                        if let Some(dead) = slots.remove(&key) {
                            stop_later(dead.client);
                        }
                        continue;
                    }
                    slot.ref_count += 1;
                    slot.last_used = Instant::now();
                    return Ok(client);
                }
                Some(Err(error)) => {
                    if let Some(slot) = slot {
                        slot.waiters = slot.waiters.saturating_sub(1);
                        slots.remove(&key);
                    }
                    return Err(error);
                }
                None => {
                    if let Some(slot) = slot {
                        slot.waiters = slot.waiters.saturating_sub(1);
                    }
                    return Err(LspError::Other(
                        "LSP initialization was abandoned".to_owned(),
                    ));
                }
            }
        }
        Err(LspError::Other(format!(
            "LSP server {} kept dying while starting",
            server.id
        )))
    }

    pub fn release_client(&self, root: &Path, server_id: &str) {
        if let Some(slot) = lock(&self.slots).get_mut(&(root.to_path_buf(), server_id.to_owned()))
            && slot.ref_count > 0
        {
            slot.ref_count -= 1;
            slot.last_used = Instant::now();
        }
    }

    /// Drops the pooled client (only if it is still `client` when given) and stops it.
    pub fn invalidate_client(&self, root: &Path, server_id: &str, client: Option<&Arc<LspClient>>) {
        let key = (root.to_path_buf(), server_id.to_owned());
        let mut slots = lock(&self.slots);
        let matches = slots
            .get(&key)
            .is_some_and(|slot| client.is_none_or(|client| Arc::ptr_eq(&slot.client, client)));
        if matches && let Some(slot) = slots.remove(&key) {
            stop_later(slot.client);
        }
    }

    /// Starts a server in the background without taking a reference.
    pub fn warmup_client(&self, root: &Path, server: &ResolvedServer) {
        if *lock(&self.disposed) || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        self.ensure_reaper();
        let key = (root.to_path_buf(), server.id.clone());
        let mut slots = lock(&self.slots);
        if slots.contains_key(&key) {
            return;
        }
        let (client, ready) = self.spawn_init(root, server);
        let now = Instant::now();
        slots.insert(
            key.clone(),
            Slot {
                client: Arc::clone(&client),
                ready: ready.clone(),
                ref_count: 0,
                waiters: 0,
                last_used: now,
                init_started: Some(now),
                init_timeout: server.init_timeout.unwrap_or(self.options.init_timeout),
            },
        );
        drop(slots);
        let me = self.me.clone();
        let mut ready = ready;
        tokio::spawn(async move {
            let state = ready
                .wait_for(Option::is_some)
                .await
                .map(|state| state.clone())
                .ok()
                .flatten();
            let Some(manager) = me.upgrade() else { return };
            let mut slots = lock(&manager.slots);
            let same = slots
                .get(&key)
                .is_some_and(|slot| Arc::ptr_eq(&slot.client, &client));
            match state {
                Some(Ok(_)) if same => {
                    if let Some(slot) = slots.get_mut(&key) {
                        slot.init_started = None;
                        slot.last_used = Instant::now();
                    }
                }
                _ if same => {
                    slots.remove(&key);
                }
                _ => {}
            }
        });
    }

    pub fn is_server_initializing(&self, root: &Path, server_id: &str) -> bool {
        lock(&self.slots)
            .get(&(root.to_path_buf(), server_id.to_owned()))
            .is_some_and(|slot| slot.init_started.is_some() && slot.ready.borrow().is_none())
    }

    pub fn snapshot(&self) -> Vec<ClientSnapshot> {
        let now = Instant::now();
        let mut rows: Vec<ClientSnapshot> = lock(&self.slots)
            .iter()
            .map(|((root, server_id), slot)| {
                let initializing = slot.ready.borrow().is_none();
                ClientSnapshot {
                    root: root.clone(),
                    server_id: server_id.clone(),
                    ref_count: slot.ref_count,
                    pending_waiters: slot.waiters,
                    idle_for: now.duration_since(slot.last_used),
                    initializing,
                    alive: !initializing && slot.client.is_alive(),
                    via_daemon: slot.client.via_daemon(),
                    command: slot.client.command(),
                }
            })
            .collect();
        rows.sort_by(|a, b| (&a.root, &a.server_id).cmp(&(&b.root, &b.server_id)));
        rows
    }

    pub fn client_count(&self) -> usize {
        lock(&self.slots).len()
    }

    /// Stops every client; later `get_client` calls fail.
    pub async fn stop_all(&self) {
        *lock(&self.disposed) = true;
        let clients: Vec<Arc<LspClient>> = lock(&self.slots)
            .drain()
            .map(|(_, slot)| slot.client)
            .collect();
        for client in clients {
            client.stop().await;
        }
    }
}

#[cfg(unix)]
async fn link_or_spawn(client: &LspClient, daemon: &DaemonMode) -> Result<(), LspError> {
    if let DaemonMode::Auto { binary, socket } = daemon
        && let Some(connection) = crate::daemon::connect_shared(binary, socket, client).await
    {
        client.attach_daemon_connection(connection);
        return Ok(());
    }
    client.start().await
}

#[cfg(not(unix))]
async fn link_or_spawn(client: &LspClient, _daemon: &DaemonMode) -> Result<(), LspError> {
    client.start().await
}
