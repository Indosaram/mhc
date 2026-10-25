//! Port of `memory/sync-runner.ts` (`createSyncRunner`): end-to-end encrypted memory sync for bound
//! sessions. Every session bound to one memory repository shares one engine (a refcount per repo);
//! the last release closes it. The engine runs only when `memory.sync.enabled` is on AND a device
//! session exists; with no session the runner stays idle and logs [`SYNC_IDLE_NO_DEVICE_SESSION`].
//! Local commits from any writer reach the engine through a watch on the repository's HEAD reflog.
//!
//! The engine's futures are not `Send` and need a `tokio::task::LocalSet`, so every engine lives on
//! one dedicated thread (current-thread runtime + `LocalSet`) that the runner talks to over
//! channels. The device session source is injected; the account sign-in that produces a real
//! session is not part of this component.

pub type OnEndedCallback = Option<Rc<dyn Fn(Box<dyn Fn()>)>>;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use memory_core::identity::layout::MemoryIdentityPaths;
use memory_core::sync::engine::error::EngineResult;
use memory_core::sync::engine::{
    DeviceSync, EngineStatus, OpenDeviceSyncOptions, SyncSession, open_device_sync,
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::sync_runtime::check_sync_runtime;

/// The one line a runner logs when this device has no session to sync with (no network, no state).
pub const SYNC_IDLE_NO_DEVICE_SESSION: &str = "sync idle: no device session";
/// The error code a runner logs when a previous engine for the repo did not close within the bound.
pub const SYNC_CLOSE_WAIT_EXCEEDED: &str = "sync-close-wait-exceeded";
/// The status code of a repo whose sync engine could not be loaded.
pub const SYNC_UNAVAILABLE: &str = "sync-unavailable";
const CLOSE_WAIT_MS: u64 = 10_000;

pub type SyncFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// Maps a sync principal to the name the account's device list gives it.
pub type DeviceNameLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// A device's account session. `device_name` is supplied by the device sign-in resolver.
#[derive(Clone)]
pub struct DeviceSyncSession {
    pub session: SyncSession,
    pub device_name: Option<DeviceNameLookup>,
    pub on_ended: OnEndedCallback,
}

/// Resolves the account session for an identity. Called on the engine thread, so the returned
/// future and the session (which holds `Rc` ports) never cross threads.
pub type SyncSessionResolver = Arc<
    dyn Fn(
            crate::device_session::resolver::SyncSessionRequest,
        ) -> Pin<
            Box<
                dyn Future<
                    Output = Result<crate::device_session::resolver::SyncSessionResolution, String>,
                >,
            >,
        > + Send
        + Sync,
>;
/// Opens a device engine (on the engine thread); the default is `open_device_sync`.
pub type SyncOpen =
    Arc<dyn Fn(OpenDeviceSyncOptions) -> EngineResult<Option<DeviceSync>> + Send + Sync>;

/// No device sign-in or device keys exist on this client: the runner stays idle.
pub fn no_account_session() -> SyncSessionResolver {
    Arc::new(|_| {
        Box::pin(async { Ok(crate::device_session::resolver::SyncSessionResolution::SignedOut) })
    })
}

pub trait SyncRunnerLogger: Send + Sync {
    fn info(&self, message: &str, identity: &str);
    fn warn(&self, message: &str, identity: &str);
    fn error(&self, message: &str, identity: &str, code: &str);
}

/// A repo's sync state: the engine's status, or the typed report that the engine could not load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncRepoStatus {
    Engine(EngineStatus),
    Unavailable { reason: String },
}

pub type SyncChange = Arc<dyn Fn(PathBuf, Option<SyncRepoStatus>) + Send + Sync>;
pub trait SyncRunner: Send + Sync {
    fn on_change(&self, _listener: SyncChange) {}
    fn acquire<'a>(
        &'a self,
        identity: &'a str,
        paths: &'a MemoryIdentityPaths,
        sync_enabled: bool,
    ) -> SyncFuture<'a, ()>;
    fn release<'a>(&'a self, repo: &'a Path) -> SyncFuture<'a, Result<(), String>>;
    fn status<'a>(&'a self, repo: &'a Path) -> SyncFuture<'a, Option<SyncRepoStatus>>;
    /// A sync principal's device name, from the account sessions this runner resolved.
    fn device_name(&self, principal: &str) -> Option<String>;
}

pub struct SyncRunnerOptions {
    pub logger: Arc<dyn SyncRunnerLogger>,
    pub resolve_session: Option<SyncSessionResolver>,
    /// Replaces `open_device_sync`; when set the runtime check is skipped (as upstream with `open`).
    pub open: Option<SyncOpen>,
    /// How long an acquire waits for the repo's previous engine to close; default 10 s.
    pub close_wait: Option<Duration>,
}

/// Watches `<git_dir>/logs/HEAD`; every change calls `on_change`. Returns the unwatch.
pub fn watch_head_log(
    git_dir: &Path,
    on_change: Arc<dyn Fn() + Send + Sync>,
    warn: Arc<dyn Fn(&str) + Send + Sync>,
) -> Box<dyn FnOnce() + Send> {
    use notify::Watcher;
    let log = git_dir.join("logs").join("HEAD");
    let on_event_warn = warn.clone();
    let watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
            Ok(event) if matches!(event.kind, notify::EventKind::Access(_)) => {}
            Ok(_) => on_change(),
            Err(error) => on_event_warn(&format!(
                "memory sync: the HEAD reflog watch stopped ({error})"
            )),
        });
    let started = watcher.and_then(|mut watcher| {
        watcher
            .watch(&log, notify::RecursiveMode::NonRecursive)
            .map(|()| watcher)
    });
    match started {
        Ok(watcher) => Box::new(move || drop(watcher)),
        Err(error) => {
            warn(&format!(
                "memory sync: cannot watch {} for local commits ({error}); local edits sync on the next stream event or restart",
                log.display()
            ));
            Box::new(|| {})
        }
    }
}

#[derive(Clone)]
enum Outcome {
    Running,
    Idle,
    Unavailable(String),
}

enum Cmd {
    Start {
        identity: String,
        paths: Box<MemoryIdentityPaths>,
        reply: oneshot::Sender<Outcome>,
    },
    Status {
        repo: PathBuf,
        reply: oneshot::Sender<Option<EngineStatus>>,
    },
    Close {
        repo: PathBuf,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Clone)]
struct WorkerConfig {
    logger: Arc<dyn SyncRunnerLogger>,
    resolve: SyncSessionResolver,
    open: SyncOpen,
    check_runtime: bool,
    names: Arc<Mutex<BTreeMap<String, DeviceNameLookup>>>,
    listeners: Arc<Mutex<Vec<SyncChange>>>,
}

struct Running {
    device: Rc<DeviceSync>,
    unwatch: Box<dyn FnOnce() + Send>,
}

type Engines = Rc<RefCell<HashMap<PathBuf, Running>>>;

async fn start_engine(
    cfg: &WorkerConfig,
    engines: &Engines,
    identity: String,
    paths: MemoryIdentityPaths,
) -> Outcome {
    use crate::device_session::resolver::{
        SyncSessionRequest, SyncSessionResolution, SyncSessionSpace,
    };
    let request = SyncSessionRequest {
        identity: identity.clone(),
        paths: paths.clone(),
        space: SyncSessionSpace {
            profile: None,
            project_dir: paths.repo.clone(),
        },
    };
    let resolved = (cfg.resolve)(request).await;
    let session = match resolved {
        Ok(SyncSessionResolution::Open(session)) => Some(DeviceSyncSession {
            session: session.session,
            device_name: session.device_name,
            on_ended: Some(Rc::new(move |notify| {
                session.ended.borrow_mut().push(notify)
            })),
        }),
        Ok(SyncSessionResolution::SignedOut) => None,
        Ok(SyncSessionResolution::Idle(reason)) => {
            cfg.logger
                .info(&format!("sync idle: {reason:?}"), &identity);
            return Outcome::Idle;
        }
        Err(error) => {
            cfg.logger.error(&error, &identity, "sync-session-failed");
            return Outcome::Idle;
        }
    };
    if let Some(lookup) = session.as_ref().and_then(|s| s.device_name.clone()) {
        cfg.names
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(identity.clone(), lookup);
    }
    let Some(session) = session else {
        cfg.logger.info(SYNC_IDLE_NO_DEVICE_SESSION, &identity);
        return Outcome::Idle;
    };
    if cfg.check_runtime
        && let Err(reason) = check_sync_runtime()
    {
        cfg.logger.warn(
            &format!("memory sync unavailable: the sync engine could not be loaded ({SYNC_UNAVAILABLE}): {reason}"),
            &identity,
        );
        for notify in lock(&cfg.listeners).clone() {
            notify(
                paths.repo.clone(),
                Some(SyncRepoStatus::Unavailable {
                    reason: reason.clone(),
                }),
            );
        }
        return Outcome::Unavailable(reason);
    }
    let on_ended = session.on_ended.clone();
    let log_logger = cfg.logger.clone();
    let log_identity = identity.clone();
    let opened = (cfg.open)(OpenDeviceSyncOptions {
        on_status_change: Some(Rc::new({
            let engines = engines.clone();
            let repo = paths.repo.clone();
            let listeners = cfg.listeners.clone();
            move || {
                let engines = engines.clone();
                let repo = repo.clone();
                let listeners = listeners.clone();
                tokio::task::spawn_local(async move {
                    let status = engines
                        .borrow()
                        .get(&repo)
                        .and_then(|running| running.device.engine.status().ok())
                        .map(SyncRepoStatus::Engine);
                    let callbacks = lock(&listeners).clone();
                    for notify in callbacks {
                        notify(repo.clone(), status.clone());
                    }
                });
            }
        })),
        repo_dir: paths.repo.clone(),
        state_dir: paths.runtime.join("sync-device"),
        locks_dir: paths.locks.clone(),
        session: session.session,
        machine_id: None,
        log: Some(Rc::new(move |line: &str| {
            log_logger.info(line, &log_identity)
        })),
    });
    let device = match opened {
        Err(error) => {
            cfg.logger.error(
                &format!("memory sync failed to start: {error}"),
                &identity,
                "sync-start-failed",
            );
            return Outcome::Idle;
        }
        Ok(None) => {
            cfg.logger.info(
                "memory sync idle: another OmO process on this device is syncing this memory",
                &identity,
            );
            return Outcome::Idle;
        }
        Ok(Some(device)) => Rc::new(device),
    };
    if let Some(on_ended) = on_ended {
        let device = device.clone();
        on_ended(Box::new(move || {
            let device = device.clone();
            tokio::task::spawn_local(async move {
                if let Err(error) = device.close().await {
                    eprintln!("memory sync close failed: {error}");
                }
            });
        }));
    }
    device.engine.start();
    // The watch callback runs on a notify thread; hand each change to a local task that owns the
    // (non-Send) device handle. Dropping the watcher drops the sender and ends that task.
    let (changes, mut received) = mpsc::unbounded_channel::<()>();
    let notified = device.clone();
    tokio::task::spawn_local(async move {
        while received.recv().await.is_some() {
            notified.notify_local_commit();
        }
    });
    let warn_logger = cfg.logger.clone();
    let warn_identity = identity.clone();
    let unwatch = watch_head_log(
        &paths.repo.join(".git"),
        Arc::new(move || {
            let _ = changes.send(());
        }),
        Arc::new(move |message| warn_logger.warn(message, &warn_identity)),
    );
    // A watch is armed some time after it is created; one debounced local-commit cycle covers a
    // commit that landed in that window.
    device.notify_local_commit();
    engines.borrow_mut().insert(
        paths.repo.clone(),
        Running {
            device: device.clone(),
            unwatch,
        },
    );
    if let Ok(status) = device.engine.status() {
        for notify in lock(&cfg.listeners).clone() {
            notify(
                paths.repo.clone(),
                Some(SyncRepoStatus::Engine(status.clone())),
            );
        }
    }
    Outcome::Running
}

async fn handle(cfg: WorkerConfig, engines: Engines, cmd: Cmd) {
    match cmd {
        Cmd::Start {
            identity,
            paths,
            reply,
        } => {
            let _ = reply.send(start_engine(&cfg, &engines, identity, *paths).await);
        }
        Cmd::Status { repo, reply } => {
            let status = engines
                .borrow()
                .get(&repo)
                .and_then(|running| running.device.status().ok());
            let _ = reply.send(status);
        }
        Cmd::Close { repo, reply } => {
            let running = engines.borrow_mut().remove(&repo);
            let result = match running {
                None => Ok(()),
                Some(running) => {
                    (running.unwatch)();
                    running
                        .device
                        .close()
                        .await
                        .map_err(|error| error.to_string())
                }
            };
            for notify in lock(&cfg.listeners).clone() {
                notify(repo.clone(), None);
            }
            let _ = reply.send(result);
        }
    }
}

fn spawn_worker(cfg: WorkerConfig) -> std::io::Result<mpsc::UnboundedSender<Cmd>> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
    std::thread::Builder::new()
        .name("omo-memory-sync".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, async move {
                let engines: Engines = Rc::new(RefCell::new(HashMap::new()));
                while let Some(cmd) = rx.recv().await {
                    tokio::task::spawn_local(handle(cfg.clone(), engines.clone(), cmd));
                }
            });
        })?;
    Ok(tx)
}

struct Held {
    users: usize,
    ready: watch::Receiver<Option<Outcome>>,
}

struct Shared {
    cfg: WorkerConfig,
    close_wait: Duration,
    repos: Mutex<HashMap<PathBuf, Held>>,
    /// A repo whose last session released it, until its engine has closed and dropped the lease.
    closing: Mutex<HashMap<PathBuf, (u64, watch::Receiver<bool>)>>,
    worker: Mutex<Option<mpsc::UnboundedSender<Cmd>>>,
    next_close: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn ready_outcome(mut ready: watch::Receiver<Option<Outcome>>) -> Outcome {
    match ready.wait_for(Option::is_some).await {
        Ok(value) => value.clone().unwrap_or(Outcome::Idle),
        Err(_) => Outcome::Idle,
    }
}

impl Shared {
    fn sender(&self) -> Option<mpsc::UnboundedSender<Cmd>> {
        let mut worker = lock(&self.worker);
        if worker.is_none() {
            match spawn_worker(self.cfg.clone()) {
                Ok(tx) => *worker = Some(tx),
                Err(error) => {
                    self.cfg.logger.error(
                        &format!("memory sync: cannot start the sync thread: {error}"),
                        "",
                        "sync-start-failed",
                    );
                    return None;
                }
            }
        }
        worker.clone()
    }

    async fn request<T>(&self, build: impl FnOnce(oneshot::Sender<T>) -> Cmd) -> Option<T> {
        let (reply, answer) = oneshot::channel();
        self.sender()?.send(build(reply)).ok()?;
        answer.await.ok()
    }

    async fn acquire(
        self: Arc<Self>,
        identity: String,
        paths: MemoryIdentityPaths,
        sync_enabled: bool,
    ) {
        if !sync_enabled {
            return;
        }
        // A session rebinding the repo right after the previous one released it would otherwise
        // race the old engine's lease and stay idle; wait for that close first.
        let pending = lock(&self.closing)
            .get(&paths.repo)
            .map(|(_, done)| done.clone());
        if let Some(mut done) = pending {
            let closed = tokio::time::timeout(self.close_wait, done.wait_for(|closed| *closed))
                .await
                .is_ok();
            if !closed {
                self.cfg.logger.error(
                    &format!(
                        "memory sync: the previous engine for this memory did not close within {} ms; starting anyway, and this device may stay idle if it still holds its lease",
                        self.close_wait.as_millis()
                    ),
                    &identity,
                    SYNC_CLOSE_WAIT_EXCEEDED,
                );
            }
        }
        let ready = {
            let mut repos = lock(&self.repos);
            if let Some(held) = repos.get_mut(&paths.repo) {
                held.users += 1;
                return;
            }
            let (publish, ready) = watch::channel(None);
            repos.insert(
                paths.repo.clone(),
                Held {
                    users: 1,
                    ready: ready.clone(),
                },
            );
            let this = self.clone();
            tokio::spawn(async move {
                let outcome = this
                    .request(|reply| Cmd::Start {
                        identity,
                        paths: Box::new(paths),
                        reply,
                    })
                    .await
                    .unwrap_or(Outcome::Idle);
                let _ = publish.send(Some(outcome));
            });
            ready
        };
        ready_outcome(ready).await;
    }

    async fn release(self: Arc<Self>, repo: PathBuf) -> Result<(), String> {
        let (ready, id, publish) = {
            let mut repos = lock(&self.repos);
            let Some(held) = repos.get_mut(&repo) else {
                return Ok(());
            };
            held.users -= 1;
            if held.users > 0 {
                return Ok(());
            }
            let ready = held.ready.clone();
            repos.remove(&repo);
            let (publish, done) = watch::channel(false);
            let id = self.next_close.fetch_add(1, Ordering::Relaxed);
            lock(&self.closing).insert(repo.clone(), (id, done));
            (ready, id, publish)
        };
        let result = match ready_outcome(ready).await {
            Outcome::Running => self
                .request(|reply| Cmd::Close {
                    repo: repo.clone(),
                    reply,
                })
                .await
                .unwrap_or_else(|| Err("memory sync: the sync thread is gone".into())),
            Outcome::Idle | Outcome::Unavailable(_) => Ok(()),
        };
        let _ = publish.send(true);
        let mut closing = lock(&self.closing);
        if closing
            .get(&repo)
            .is_some_and(|(current, _)| *current == id)
        {
            closing.remove(&repo);
        }
        result
    }

    async fn status(self: Arc<Self>, repo: PathBuf) -> Option<SyncRepoStatus> {
        let ready = lock(&self.repos).get(&repo)?.ready.clone();
        match ready_outcome(ready).await {
            Outcome::Idle => None,
            Outcome::Unavailable(reason) => Some(SyncRepoStatus::Unavailable { reason }),
            Outcome::Running => self
                .request(|reply| Cmd::Status { repo, reply })
                .await
                .flatten()
                .map(SyncRepoStatus::Engine),
        }
    }
}

struct Runner(Arc<Shared>);

impl SyncRunner for Runner {
    fn on_change(&self, listener: SyncChange) {
        lock(&self.0.cfg.listeners).push(listener);
    }
    fn acquire<'a>(
        &'a self,
        identity: &'a str,
        paths: &'a MemoryIdentityPaths,
        sync_enabled: bool,
    ) -> SyncFuture<'a, ()> {
        Box::pin(
            self.0
                .clone()
                .acquire(identity.to_owned(), paths.clone(), sync_enabled),
        )
    }

    fn release<'a>(&'a self, repo: &'a Path) -> SyncFuture<'a, Result<(), String>> {
        Box::pin(self.0.clone().release(repo.to_path_buf()))
    }

    fn status<'a>(&'a self, repo: &'a Path) -> SyncFuture<'a, Option<SyncRepoStatus>> {
        Box::pin(self.0.clone().status(repo.to_path_buf()))
    }

    fn device_name(&self, principal: &str) -> Option<String> {
        lock(&self.0.cfg.names)
            .values()
            .find_map(|lookup| lookup(principal))
    }
}

pub fn create_sync_runner(options: SyncRunnerOptions) -> Arc<dyn SyncRunner> {
    let check_runtime = options.open.is_none();
    let open: SyncOpen = options.open.unwrap_or_else(|| Arc::new(open_device_sync));
    Arc::new(Runner(Arc::new(Shared {
        cfg: WorkerConfig {
            logger: options.logger,
            resolve: options.resolve_session.unwrap_or_else(no_account_session),
            open,
            check_runtime,
            names: Arc::default(),
            listeners: Arc::default(),
        },
        close_wait: options
            .close_wait
            .unwrap_or(Duration::from_millis(CLOSE_WAIT_MS)),
        repos: Mutex::default(),
        closing: Mutex::default(),
        worker: Mutex::default(),
        next_close: AtomicU64::new(0),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::sync::engine::error::EngineError;
    use memory_core::sync::engine::ports::{
        AppendRequest, AppendResult, BatchPayload, PortFuture, PullPage, PulledEntry, RefusalCode,
        SealedBatch, Sealer, SnapshotRef, StreamHandlers, Transport, Unsubscribe, Verdict,
        Verifier,
    };

    const BOUND: Duration = Duration::from_secs(20);

    #[derive(Default)]
    struct Log(Mutex<Vec<(String, String, Option<String>)>>);
    impl Log {
        fn lines(&self) -> Vec<(String, String, Option<String>)> {
            lock(&self.0).clone()
        }
    }
    impl SyncRunnerLogger for Log {
        fn info(&self, message: &str, _: &str) {
            lock(&self.0).push(("info".into(), message.into(), None));
        }
        fn warn(&self, message: &str, _: &str) {
            lock(&self.0).push(("warn".into(), message.into(), None));
        }
        fn error(&self, message: &str, _: &str, code: &str) {
            lock(&self.0).push(("error".into(), message.into(), Some(code.into())));
        }
    }

    struct Never;
    impl Transport for Never {
        fn pull(&self, _: u64, _: usize) -> PortFuture<'_, PullPage> {
            Box::pin(async { Err(EngineError::Other("offline".into())) })
        }
        fn download<'a>(&'a self, _: &'a str) -> PortFuture<'a, Vec<u8>> {
            Box::pin(async { Err(EngineError::Other("offline".into())) })
        }
        fn reserve<'a>(&'a self, _: usize, _: &'a str) -> PortFuture<'a, String> {
            Box::pin(async { Err(EngineError::Other("offline".into())) })
        }
        fn upload<'a>(&'a self, _: &'a str, _: &'a [u8]) -> PortFuture<'a, ()> {
            Box::pin(async { Err(EngineError::Other("offline".into())) })
        }
        fn append(&self, _: AppendRequest) -> PortFuture<'_, AppendResult> {
            Box::pin(async { Err(EngineError::Other("offline".into())) })
        }
        fn subscribe(&self, _: StreamHandlers) -> Unsubscribe {
            Box::new(|| {})
        }
    }
    impl Verifier for Never {
        fn verify_entry<'a>(&'a self, _: &'a PulledEntry, _: &'a [u8]) -> PortFuture<'a, Verdict> {
            Box::pin(async { Ok(Verdict::Refused(RefusalCode::Malformed)) })
        }
        fn verify_snapshot<'a>(
            &'a self,
            _: &'a SnapshotRef,
            _: &'a [Vec<u8>],
        ) -> PortFuture<'a, Verdict> {
            Box::pin(async { Ok(Verdict::Refused(RefusalCode::Malformed)) })
        }
    }
    impl Sealer for Never {
        fn seal<'a>(
            &'a self,
            _: &'a str,
            _: u64,
            _: &'a BatchPayload,
        ) -> PortFuture<'a, SealedBatch> {
            Box::pin(async { Err(EngineError::Other("offline".into())) })
        }
    }

    fn offline_session() -> DeviceSyncSession {
        let never = Rc::new(Never);
        DeviceSyncSession {
            session: SyncSession {
                transport: never.clone(),
                verifier: never.clone(),
                sealer: never,
            },
            on_ended: None,
            device_name: Some(Arc::new(|principal| {
                (principal == "p1").then(|| "Laptop".to_owned())
            })),
        }
    }

    fn offline_resolver() -> SyncSessionResolver {
        Arc::new(|_| {
            Box::pin(async {
                let session = offline_session();
                Ok(
                    crate::device_session::resolver::SyncSessionResolution::Open(
                        crate::device_session::resolver::ResolvedDeviceSession {
                            device_name: session.device_name,
                            session: session.session,
                            ended: Rc::new(RefCell::new(Vec::new())),
                        },
                    ),
                )
            })
        })
    }

    fn runner(
        log: &Arc<Log>,
        resolve: Option<SyncSessionResolver>,
        close_wait: Option<Duration>,
    ) -> (Arc<dyn SyncRunner>, Arc<Shared>) {
        let options = SyncRunnerOptions {
            logger: log.clone(),
            resolve_session: resolve,
            open: None,
            close_wait,
        };
        let shared = Arc::new(Shared {
            cfg: WorkerConfig {
                logger: options.logger,
                resolve: options.resolve_session.unwrap_or_else(no_account_session),
                open: Arc::new(open_device_sync),
                check_runtime: true,
                names: Arc::default(),
                listeners: Arc::default(),
            },
            close_wait: options
                .close_wait
                .unwrap_or(Duration::from_millis(CLOSE_WAIT_MS)),
            repos: Mutex::default(),
            closing: Mutex::default(),
            worker: Mutex::default(),
            next_close: AtomicU64::new(0),
        });
        (Arc::new(Runner(shared.clone())), shared)
    }

    fn paths(root: &Path) -> MemoryIdentityPaths {
        let paths = memory_core::identity::layout::build_identity_paths(root, "id");
        std::fs::create_dir_all(paths.repo.join(".git")).unwrap();
        paths
    }

    #[tokio::test]
    async fn no_device_session_stays_idle_and_logs_the_idle_line() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let log = Arc::new(Log::default());
        let (runner, _) = runner(&log, None, None);
        tokio::time::timeout(BOUND, runner.acquire("id", &paths, true))
            .await
            .unwrap();
        assert_eq!(runner.status(&paths.repo).await, None);
        assert!(
            log.lines().iter().any(
                |(level, message, _)| level == "info" && message == SYNC_IDLE_NO_DEVICE_SESSION
            )
        );
        tokio::time::timeout(BOUND, runner.release(&paths.repo))
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn disabled_sync_acquires_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let log = Arc::new(Log::default());
        let (runner, shared) = runner(&log, Some(offline_resolver()), None);
        runner.acquire("id", &paths, false).await;
        assert_eq!(runner.status(&paths.repo).await, None);
        assert!(log.lines().is_empty());
        assert!(lock(&shared.worker).is_none());
    }

    #[tokio::test]
    async fn one_engine_is_shared_per_repo_and_the_last_release_frees_the_lease() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let log = Arc::new(Log::default());
        let (runner, _) = runner(&log, Some(offline_resolver()), None);
        tokio::time::timeout(BOUND, runner.acquire("id", &paths, true))
            .await
            .unwrap();
        tokio::time::timeout(BOUND, runner.acquire("id", &paths, true))
            .await
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(BOUND, runner.status(&paths.repo))
                .await
                .unwrap(),
            Some(SyncRepoStatus::Engine(_))
        ));
        assert_eq!(runner.device_name("p1"), Some("Laptop".to_owned()));
        assert_eq!(runner.device_name("p2"), None);
        tokio::time::timeout(BOUND, runner.release(&paths.repo))
            .await
            .unwrap()
            .unwrap();
        assert!(runner.status(&paths.repo).await.is_some());
        tokio::time::timeout(BOUND, runner.release(&paths.repo))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(runner.status(&paths.repo).await, None);
        // The lease file is gone: the same repo can be opened again.
        let never = Rc::new(Never);
        let reopened = open_device_sync(OpenDeviceSyncOptions {
            on_status_change: None,
            repo_dir: paths.repo.clone(),
            state_dir: paths.runtime.join("sync-device"),
            locks_dir: paths.locks.clone(),
            session: SyncSession {
                transport: never.clone(),
                verifier: never.clone(),
                sealer: never,
            },
            machine_id: Some("fixed".into()),
            log: None,
        })
        .unwrap();
        assert!(reopened.is_some());
    }

    #[tokio::test]
    async fn a_previous_engine_that_never_closes_is_waited_for_only_up_to_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let log = Arc::new(Log::default());
        let (runner, shared) = runner(&log, None, Some(Duration::from_millis(20)));
        let (hold, done) = watch::channel(false);
        lock(&shared.closing).insert(paths.repo.clone(), (0, done));
        tokio::time::timeout(BOUND, runner.acquire("id", &paths, true))
            .await
            .unwrap();
        drop(hold);
        assert!(log.lines().iter().any(|(level, _, code)| level == "error"
            && code.as_deref() == Some(SYNC_CLOSE_WAIT_EXCEEDED)));
    }

    #[test]
    fn an_unwatchable_head_log_warns_and_returns_a_noop_unwatch() {
        let dir = tempfile::tempdir().unwrap();
        let warnings = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = warnings.clone();
        let unwatch = watch_head_log(
            dir.path(),
            Arc::new(|| {}),
            Arc::new(move |message| lock(&seen).push(message.to_owned())),
        );
        unwatch();
        assert_eq!(lock(&warnings).len(), 1);
        assert!(lock(&warnings)[0].starts_with("memory sync: cannot watch "));
    }
}
