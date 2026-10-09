use crate::abort::AbortSignal;
use crate::lsp::cleanup_errors::report_best_effort_cleanup_error;
use crate::lsp::client::LspClient;
use crate::lsp::client::LspClientOptions;
use crate::lsp::client::LspDiagnosticsResult;
use crate::lsp::constants::CLIENT_RESPAWN_COOLDOWN_MS;
use crate::lsp::constants::CLIENT_RESPAWN_RETRY_LIMIT;
use crate::lsp::constants::IDLE_TIMEOUT_MS;
use crate::lsp::constants::INIT_TIMEOUT_MS;
use crate::lsp::constants::MAX_RESIDENT_CLIENTS;
use crate::lsp::constants::REAPER_INTERVAL_MS;
use crate::lsp::errors::LspError;
use crate::lsp::process_signal_cleanup::SignalCleanupGuard;
use crate::lsp::process_signal_cleanup::install_process_signal_cleanup;
use crate::lsp::types::ResolvedServer;
use indexmap::IndexMap;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::PoisonError;
use std::sync::Weak;
use std::time::Duration;
use tokio::sync::watch;

pub type ClientFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The client surface the manager and directory diagnostics depend on. [`LspClient`]
/// implements it; tests inject fakes (TS used `clientFactory` plus subclass overrides).
pub trait ManagedLspClient: Send + Sync {
    fn start(&self) -> ClientFuture<'_, Result<(), LspError>>;
    fn initialize(&self) -> ClientFuture<'_, Result<(), LspError>>;
    fn stop(&self) -> ClientFuture<'_, ()>;
    fn is_alive(&self) -> bool;
    fn command(&self) -> Vec<String>;
    fn diagnostics<'a>(
        &'a self,
        file_path: &'a str,
        signal: Option<&'a AbortSignal>,
    ) -> ClientFuture<'a, Result<LspDiagnosticsResult, LspError>>;
    /// The concrete client for tools that need the full request surface.
    fn as_lsp_client(&self) -> Option<&LspClient> {
        None
    }
}

impl ManagedLspClient for LspClient {
    fn start(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async move { LspClient::start(self) })
    }

    fn initialize(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(LspClient::initialize(self))
    }

    fn stop(&self) -> ClientFuture<'_, ()> {
        Box::pin(LspClient::stop(self))
    }

    fn is_alive(&self) -> bool {
        LspClient::is_alive(self)
    }

    fn command(&self) -> Vec<String> {
        LspClient::command(self)
    }

    fn diagnostics<'a>(
        &'a self,
        file_path: &'a str,
        signal: Option<&'a AbortSignal>,
    ) -> ClientFuture<'a, Result<LspDiagnosticsResult, LspError>> {
        Box::pin(LspClient::diagnostics(self, file_path, signal))
    }

    fn as_lsp_client(&self) -> Option<&LspClient> {
        Some(self)
    }
}

pub type SharedClient = Arc<dyn ManagedLspClient>;
pub type ClientFactory =
    Arc<dyn Fn(&str, &ResolvedServer) -> Result<SharedClient, LspError> + Send + Sync>;
pub type ManagerNow = Arc<dyn Fn() -> f64 + Send + Sync>;

/// TS `ClientSnapshot`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientSnapshot {
    pub root: String,
    pub server_id: String,
    pub ref_count: usize,
    pub pending_waiters: usize,
    pub last_used_at: f64,
    pub is_initializing: bool,
    pub alive: bool,
    pub command: Vec<String>,
}

/// TS `LspManagerOptions`.
#[derive(Clone, Default)]
pub struct LspManagerOptions {
    pub idle_timeout_ms: Option<u64>,
    pub init_timeout_ms: Option<u64>,
    pub max_resident_clients: Option<usize>,
    pub reaper_interval_ms: Option<u64>,
    pub client_factory: Option<ClientFactory>,
    pub now: Option<ManagerNow>,
}

type InitOutcome = Option<Result<(), LspError>>;

struct ManagedClient {
    id: u64,
    client: SharedClient,
    ref_count: usize,
    pending_waiters: usize,
    last_used_at: f64,
    init: Option<watch::Receiver<InitOutcome>>,
    is_initializing: bool,
    initializing_since: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
struct RespawnBudget {
    dead_generations: usize,
    exhausted_at: Option<f64>,
}

#[derive(Default)]
struct ManagerState {
    clients: IndexMap<String, ManagedClient>,
    pending_stops: HashMap<String, watch::Receiver<bool>>,
    respawn_budgets: HashMap<String, RespawnBudget>,
    disposed: bool,
    next_id: u64,
    reaper: Option<tokio::task::JoinHandle<()>>,
    signal_cleanup: Option<SignalCleanupGuard>,
}

/// TS `LspManager`: pools one client per `(root, server id)`, shares initialization
/// between concurrent callers, limits resident clients via idle LRU eviction,
/// enforces pending-stop tombstones to prevent duplicate client spawn races,
/// and bounds respawn attempts for crash-looping servers.
pub struct LspManager {
    state: Mutex<ManagerState>,
    idle_timeout_ms: f64,
    init_timeout_ms: f64,
    max_resident_clients: usize,
    reaper_interval_ms: u64,
    client_factory: ClientFactory,
    now: ManagerNow,
}

impl std::fmt::Debug for LspManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspManager")
            .field("clients", &self.client_count())
            .field("max_resident_clients", &self.max_resident_clients)
            .finish()
    }
}

async fn stop_client_best_effort(client: SharedClient) {
    client.stop().await;
}

fn system_now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

fn get_key(root: &str, server_id: &str) -> String {
    format!("{root}::{server_id}")
}

async fn await_with_signal<F, T>(fut: F, signal: Option<&AbortSignal>) -> Result<T, LspError>
where
    F: Future<Output = T>,
{
    match signal {
        None => Ok(fut.await),
        Some(sig) => {
            if sig.aborted() {
                return Err(LspError::Aborted);
            }
            tokio::select! {
                res = fut => Ok(res),
                () = sig.cancelled() => Err(LspError::Aborted),
            }
        }
    }
}

async fn await_init(
    mut receiver: watch::Receiver<InitOutcome>,
    signal: Option<&AbortSignal>,
) -> Result<(), LspError> {
    let wait = async {
        match receiver.wait_for(Option::is_some).await {
            Ok(outcome) => outcome.clone().unwrap_or(Ok(())),
            Err(_) => Err(LspError::other("LSP client initialization was abandoned")),
        }
    };
    match signal {
        None => wait.await,
        Some(signal) => {
            if signal.aborted() {
                return Err(LspError::Aborted);
            }
            tokio::select! {
                outcome = wait => outcome,
                () = signal.cancelled() => Err(LspError::Aborted),
            }
        }
    }
}

impl LspManager {
    /// Creates a manager and starts its reaper when a Tokio runtime is available.
    pub fn new(options: LspManagerOptions) -> Arc<Self> {
        let manager = Arc::new(Self {
            state: Mutex::new(ManagerState {
                next_id: 1,
                ..ManagerState::default()
            }),
            idle_timeout_ms: options.idle_timeout_ms.unwrap_or(IDLE_TIMEOUT_MS) as f64,
            init_timeout_ms: options.init_timeout_ms.unwrap_or(INIT_TIMEOUT_MS) as f64,
            max_resident_clients: options
                .max_resident_clients
                .unwrap_or(MAX_RESIDENT_CLIENTS),
            reaper_interval_ms: options.reaper_interval_ms.unwrap_or(REAPER_INTERVAL_MS),
            client_factory: options.client_factory.unwrap_or_else(|| {
                Arc::new(|root: &str, server: &ResolvedServer| {
                    Ok(Arc::new(LspClient::new(
                        root,
                        server.clone(),
                        LspClientOptions::default(),
                    )) as SharedClient)
                })
            }),
            now: options.now.unwrap_or_else(|| Arc::new(system_now_ms)),
        });
        manager.start_reaper();
        let weak = Arc::downgrade(&manager);
        manager.lock().signal_cleanup = install_process_signal_cleanup(move || {
            let manager = weak.upgrade();
            async move {
                if let Some(manager) = manager {
                    manager.stop_all().await;
                }
            }
        });
        manager
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ManagerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn start_reaper(self: &Arc<Self>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let weak: Weak<Self> = Arc::downgrade(self);
        let interval = Duration::from_millis(self.reaper_interval_ms.max(1));
        let task = handle.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let Some(manager) = weak.upgrade() else {
                    return;
                };
                manager.reap_stale();
            }
        });
        self.lock().reaper = Some(task);
    }

    /// Deletes-then-stops with a tombstone: the pending stop is recorded so a concurrent getClient
    /// for the same key awaits it instead of spawning a duplicate client while the old one lingers
    /// (production stops reach ~21s). The tombstone removes itself once the stop settles.
    fn tombstone_stop(self: &Arc<Self>, key: &str, client: SharedClient) -> watch::Receiver<bool> {
        let (tx, rx) = watch::channel(false);
        {
            let mut state = self.lock();
            state.pending_stops.insert(key.to_string(), rx.clone());
        }

        let weak = Arc::downgrade(self);
        let key_str = key.to_string();
        tokio::spawn(async move {
            stop_client_best_effort(client).await;
            let _ = tx.send(true);
            if let Some(mgr) = weak.upgrade() {
                let mut state = mgr.lock();
                state.pending_stops.remove(&key_str);
            }
        });

        rx
    }

    fn record_dead_generation(&self, key: &str) {
        let mut state = self.lock();
        let budget = state
            .respawn_budgets
            .entry(key.to_string())
            .or_insert_with(|| RespawnBudget {
                dead_generations: 0,
                exhausted_at: None,
            });
        budget.dead_generations += 1;
    }

    fn mark_healthy_generation(&self, key: &str) {
        let mut state = self.lock();
        state.respawn_budgets.remove(key);
    }

    /// Throws while the respawn budget is spent and the cooldown has not elapsed.
    fn ensure_respawn_allowed(&self, key: &str, root: &str, server_id: &str) -> Result<(), LspError> {
        let mut state = self.lock();
        let Some(budget) = state.respawn_budgets.get_mut(key) else {
            return Ok(());
        };
        if budget.dead_generations < CLIENT_RESPAWN_RETRY_LIMIT {
            return Ok(());
        }
        let now = (self.now)();
        match budget.exhausted_at {
            None => {
                budget.exhausted_at = Some(now);
                Err(LspError::ClientRespawnBudgetExceeded {
                    server_id: server_id.to_string(),
                    root: root.to_string(),
                    retry_limit: CLIENT_RESPAWN_RETRY_LIMIT,
                })
            }
            Some(exhausted_at) => {
                if now - exhausted_at < CLIENT_RESPAWN_COOLDOWN_MS as f64 {
                    Err(LspError::ClientRespawnBudgetExceeded {
                        server_id: server_id.to_string(),
                        root: root.to_string(),
                        retry_limit: CLIENT_RESPAWN_RETRY_LIMIT,
                    })
                } else {
                    budget.dead_generations = 0;
                    budget.exhausted_at = None;
                    Ok(())
                }
            }
        }
    }

    /// Frees capacity for one new client by stopping the least recently used idle client.
    ///
    /// Only clients with no in-flight work are eligible, so a resident server is never torn down
    /// underneath a live request; when every resident client is busy the cap is exceeded instead.
    fn evict_for_admission(self: &Arc<Self>) {
        loop {
            let victim = {
                let mut state = self.lock();
                if state.clients.len() < self.max_resident_clients {
                    break;
                }
                // Find least recently used idle client
                let mut candidate_key: Option<String> = None;
                let mut candidate_last_used = f64::MAX;
                for (key, managed) in state.clients.iter() {
                    if managed.ref_count > 0 || managed.pending_waiters > 0 || managed.is_initializing {
                        continue;
                    }
                    if candidate_key.is_none() || managed.last_used_at < candidate_last_used {
                        candidate_key = Some(key.clone());
                        candidate_last_used = managed.last_used_at;
                    }
                }
                candidate_key.and_then(|key| state.clients.shift_remove(&key).map(|m| (key, m.client)))
            };

            match victim {
                Some((key, client)) => {
                    self.tombstone_stop(&key, client);
                }
                None => break,
            }
        }
    }

    fn reap_stale(self: &Arc<Self>) {
        let now = (self.now)();
        let mut stale = Vec::new();
        {
            let mut state = self.lock();
            let mut remove_keys = Vec::new();
            for (key, managed) in state.clients.iter() {
                let init_stuck = managed.is_initializing
                    && managed
                        .initializing_since
                        .is_some_and(|since| now - since > self.init_timeout_ms);
                let idle = !managed.is_initializing
                    && managed.ref_count == 0
                    && managed.pending_waiters == 0
                    && now - managed.last_used_at > self.idle_timeout_ms;
                if init_stuck || idle {
                    remove_keys.push(key.clone());
                }
            }
            for key in remove_keys {
                if let Some(managed) = state.clients.shift_remove(&key) {
                    stale.push((key, managed.client));
                }
            }
        }
        for (key, client) in stale {
            self.tombstone_stop(&key, client);
        }
    }

    async fn try_delete_if_orphaned(self: &Arc<Self>, key: &str, id: u64) {
        let removed = {
            let mut state = self.lock();
            let orphaned = state.clients.get(key).is_some_and(|managed| {
                managed.id == id
                    && managed.ref_count == 0
                    && managed.pending_waiters == 0
                    && !managed.is_initializing
            });
            if orphaned {
                state.clients.shift_remove(key)
            } else {
                None
            }
        };
        if let Some(managed) = removed {
            let mut rx = self.tombstone_stop(key, managed.client);
            let _ = rx.wait_for(|settled| *settled).await;
        }
    }

    fn remove_if_same(&self, key: &str, id: u64) -> Option<ManagedClient> {
        let mut state = self.lock();
        if state
            .clients
            .get(key)
            .is_some_and(|managed| managed.id == id)
        {
            state.clients.shift_remove(key)
        } else {
            None
        }
    }

    fn start_init(&self, client: SharedClient) -> watch::Receiver<InitOutcome> {
        let (sender, receiver) = watch::channel(None);
        tokio::spawn(async move {
            let outcome = match client.start().await {
                Ok(()) => client.initialize().await,
                Err(error) => Err(error),
            };
            sender.send_replace(Some(outcome));
        });
        receiver
    }

    /// TS `getClient`: returns a started, initialized client and takes one reference.
    pub async fn get_client(
        self: &Arc<Self>,
        root: &str,
        server: &ResolvedServer,
        signal: Option<&AbortSignal>,
    ) -> Result<SharedClient, LspError> {
        if self.lock().disposed {
            return Err(LspError::other("LspManager has been disposed"));
        }
        if let Some(signal) = signal {
            signal.throw_if_aborted()?;
        }

        let key = get_key(root, &server.id);

        loop {
            let pending_stop = {
                let state = self.lock();
                state.pending_stops.get(&key).cloned()
            };
            match pending_stop {
                None => break,
                Some(mut rx) => {
                    await_with_signal(rx.wait_for(|settled| *settled), signal).await?
                        .map_err(|error| LspError::other(error.to_string()))?;
                    if let Some(signal) = signal {
                        signal.throw_if_aborted()?;
                    }
                }
            }
        }

        if self.lock().disposed {
            return Err(LspError::other("LspManager has been disposed"));
        }

        let now = (self.now)();
        let stuck = {
            let mut state = self.lock();
            let is_stuck = state.clients.get(&key).is_some_and(|managed| {
                managed.is_initializing
                    && managed
                        .initializing_since
                        .is_some_and(|since| now - since > self.init_timeout_ms)
            });
            if is_stuck {
                state.clients.shift_remove(&key)
            } else {
                None
            }
        };
        if let Some(managed) = stuck {
            let mut rx = self.tombstone_stop(&key, managed.client);
            let _ = rx.wait_for(|settled| *settled).await;
        }

        let existing = {
            let mut state = self.lock();
            state.clients.get_mut(&key).map(|managed| {
                if managed.init.is_some() {
                    managed.pending_waiters += 1;
                }
                (managed.id, managed.client.clone(), managed.init.clone())
            })
        };

        if let Some((id, client, init)) = existing {
            if let Some(init) = init {
                let outcome = await_init(init, signal).await;
                if let Some(managed) = self
                    .lock()
                    .clients
                    .get_mut(&key)
                    .filter(|managed| managed.id == id)
                {
                    managed.pending_waiters -= 1;
                }
                if let Err(error) = outcome {
                    self.try_delete_if_orphaned(&key, id).await;
                    return Err(error);
                }
            }

            if let Some(signal) = signal.filter(|signal| signal.aborted()) {
                self.try_delete_if_orphaned(&key, id).await;
                signal.throw_if_aborted()?;
            }

            if !client.is_alive() {
                self.record_dead_generation(&key);
                self.remove_if_same(&key, id);
                let mut rx = self.tombstone_stop(&key, client);
                let _ = rx.wait_for(|settled| *settled).await;
                return Box::pin(self.get_client(root, server, signal)).await;
            }

            self.mark_healthy_generation(&key);
            let now = (self.now)();
            if let Some(managed) = self
                .lock()
                .clients
                .get_mut(&key)
                .filter(|managed| managed.id == id)
            {
                managed.ref_count += 1;
                managed.last_used_at = now;
            }
            return Ok(client);
        }

        self.evict_for_admission();
        self.ensure_respawn_allowed(&key, root, &server.id)?;

        let client = (self.client_factory)(root, server)?;
        let init_started_at = (self.now)();
        let init = self.start_init(client.clone());
        let id = {
            let mut state = self.lock();
            let id = state.next_id;
            state.next_id += 1;
            state.clients.insert(
                key.clone(),
                ManagedClient {
                    id,
                    client: client.clone(),
                    ref_count: 0,
                    pending_waiters: 1,
                    last_used_at: init_started_at,
                    init: Some(init.clone()),
                    is_initializing: true,
                    initializing_since: Some(init_started_at),
                },
            );
            id
        };

        if let Err(error) = await_init(init, signal).await {
            if let Some(managed) = self
                .lock()
                .clients
                .get_mut(&key)
                .filter(|managed| managed.id == id)
            {
                managed.pending_waiters -= 1;
            }
            self.remove_if_same(&key, id);
            let mut rx = self.tombstone_stop(&key, client);
            let _ = rx.wait_for(|settled| *settled).await;
            return Err(error);
        }

        if let Some(managed) = self
            .lock()
            .clients
            .get_mut(&key)
            .filter(|managed| managed.id == id)
        {
            managed.pending_waiters -= 1;
            managed.is_initializing = false;
            managed.initializing_since = None;
            managed.init = None;
        }

        if let Some(signal) = signal.filter(|signal| signal.aborted()) {
            self.try_delete_if_orphaned(&key, id).await;
            signal.throw_if_aborted()?;
        }

        if !client.is_alive() {
            self.record_dead_generation(&key);
            self.remove_if_same(&key, id);
            let mut rx = self.tombstone_stop(&key, client);
            let _ = rx.wait_for(|settled| *settled).await;
            return Box::pin(self.get_client(root, server, signal)).await;
        }

        self.mark_healthy_generation(&key);
        let now = (self.now)();
        if let Some(managed) = self
            .lock()
            .clients
            .get_mut(&key)
            .filter(|managed| managed.id == id)
        {
            managed.ref_count += 1;
            managed.last_used_at = now;
        }
        Ok(client)
    }

    pub fn release_client(&self, root: &str, server_id: &str) {
        let now = (self.now)();
        if let Some(managed) = self
            .lock()
            .clients
            .get_mut(&get_key(root, server_id))
            .filter(|managed| managed.ref_count > 0)
        {
            managed.ref_count -= 1;
            managed.last_used_at = now;
        }
    }

    /// TS `invalidateClient`: drops the pooled client (only if it is `client`, when given).
    pub fn invalidate_client(self: &Arc<Self>, root: &str, server_id: &str, client: Option<&SharedClient>) {
        let key = get_key(root, server_id);
        let removed = {
            let mut state = self.lock();
            let matches = state.clients.get(&key).is_some_and(|managed| {
                client.is_none_or(|client| Arc::ptr_eq(&managed.client, client))
            });
            if matches {
                state.clients.shift_remove(&key)
            } else {
                None
            }
        };
        if let Some(managed) = removed {
            self.tombstone_stop(&key, managed.client);
        }
    }

    /// TS `warmupClient`: starts initialization in the background without taking a reference.
    pub fn warmup_client(self: &Arc<Self>, root: &str, server: &ResolvedServer) {
        let key = get_key(root, &server.id);
        {
            let state = self.lock();
            if state.disposed || state.clients.contains_key(&key) || state.pending_stops.contains_key(&key) {
                return;
            }
        }

        self.evict_for_admission();
        if self.ensure_respawn_allowed(&key, root, &server.id).is_err() {
            return;
        }

        let Ok(client) = (self.client_factory)(root, server) else {
            return;
        };
        let init_started_at = (self.now)();
        let mut init = self.start_init(client.clone());
        let id = {
            let mut state = self.lock();
            let id = state.next_id;
            state.next_id += 1;
            state.clients.insert(
                key.clone(),
                ManagedClient {
                    id,
                    client: client.clone(),
                    ref_count: 0,
                    pending_waiters: 0,
                    last_used_at: init_started_at,
                    init: Some(init.clone()),
                    is_initializing: true,
                    initializing_since: Some(init_started_at),
                },
            );
            id
        };
        let manager = Arc::downgrade(self);
        tokio::spawn(async move {
            let outcome = init
                .wait_for(Option::is_some)
                .await
                .map(|outcome| outcome.clone().unwrap_or(Ok(())));
            let Some(manager) = manager.upgrade() else {
                return;
            };
            match outcome {
                Ok(Ok(())) => {
                    let now = (manager.now)();
                    if let Some(managed) = manager
                        .lock()
                        .clients
                        .get_mut(&key)
                        .filter(|managed| managed.id == id)
                    {
                        managed.is_initializing = false;
                        managed.initializing_since = None;
                        managed.init = None;
                        managed.last_used_at = now;
                    }
                }
                _ => {
                    manager.remove_if_same(&key, id);
                    manager.tombstone_stop(&key, client);
                }
            }
        });
    }

    pub fn is_server_initializing(&self, root: &str, server_id: &str) -> bool {
        self.lock()
            .clients
            .get(&get_key(root, server_id))
            .is_some_and(|managed| managed.is_initializing)
    }

    pub fn get_snapshot(&self) -> Vec<ClientSnapshot> {
        self.lock()
            .clients
            .iter()
            .map(|(key, managed)| {
                let (root, server_id) = key.split_once("::").unwrap_or((key.as_str(), ""));
                ClientSnapshot {
                    root: root.to_string(),
                    server_id: server_id.to_string(),
                    ref_count: managed.ref_count,
                    pending_waiters: managed.pending_waiters,
                    last_used_at: managed.last_used_at,
                    is_initializing: managed.is_initializing,
                    alive: managed.client.is_alive(),
                    command: managed.client.command(),
                }
            })
            .collect()
    }

    pub fn has_client(&self, root: &str, server_id: &str) -> bool {
        self.lock().clients.contains_key(&get_key(root, server_id))
    }

    pub fn client_count(&self) -> usize {
        self.lock().clients.len()
    }

    /// TS `stopAll`: disposes the manager and stops every pooled client.
    pub async fn stop_all(&self) {
        let (clients, pending_stops): (Vec<SharedClient>, Vec<watch::Receiver<bool>>) = {
            let mut state = self.lock();
            state.disposed = true;
            if let Some(reaper) = state.reaper.take() {
                reaper.abort();
            }
            state.signal_cleanup = None;
            state.respawn_budgets.clear();
            let clients = state
                .clients
                .drain(..)
                .map(|(_, managed)| managed.client)
                .collect();
            let pending_stops = state.pending_stops.values().cloned().collect();
            (clients, pending_stops)
        };

        for client in clients {
            stop_client_best_effort(client).await;
        }
        for mut rx in pending_stops {
            let _ = rx.wait_for(|settled| *settled).await;
        }
        self.lock().pending_stops.clear();
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Some(reaper) = state.reaper.take() {
            reaper.abort();
        }
        if !state.clients.is_empty() {
            report_best_effort_cleanup_error(
                "manager drop",
                &format!(
                    "{} LSP client(s) dropped without stopAll",
                    state.clients.len()
                ),
            );
        }
    }
}

static DEFAULT_MANAGER: OnceLock<Mutex<Option<Arc<LspManager>>>> = OnceLock::new();

fn default_slot() -> &'static Mutex<Option<Arc<LspManager>>> {
    DEFAULT_MANAGER.get_or_init(|| Mutex::new(None))
}

/// TS `getLspManager`: the process-wide default manager.
pub fn get_lsp_manager() -> Arc<LspManager> {
    default_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(|| LspManager::new(LspManagerOptions::default()))
        .clone()
}

/// TS `disposeDefaultLspManager`.
pub async fn dispose_default_lsp_manager() {
    let manager = default_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(manager) = manager {
        manager.stop_all().await;
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
