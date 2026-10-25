//! Loss-aware experimental server startup (senpi `experimental/server.ts`).
//!
//! Transport and coordinator implementations supply `ServerStartup`; this module owns the
//! launcher/activation serialization and the ownership fences around their asynchronous work.
use std::{future::Future, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::{Duration, SystemTime}};
use tokio::task::JoinHandle;

const LOCK_STALE_MS: u64 = 30_000;
const ACTIVATION_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_SERVER_ID_FILE: &str = "default-server-id";

#[derive(Debug, Clone, thiserror::Error)]
#[error("File lock on {path:?} was lost before the operation committed: {cause}")]
pub struct FileLockLostError {
    pub path: PathBuf,
    pub cause: String,
}
impl FileLockLostError { pub const CODE: &'static str = "ELOCKLOST"; }

#[derive(Debug, thiserror::Error)]
pub enum ServerStartupError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    LockLost(#[from] FileLockLostError),
    #[error("{0}")]
    Startup(String),
}

struct LockState { released: bool, lost: Option<FileLockLostError> }
pub type LostCallback = Arc<dyn Fn(FileLockLostError) + Send + Sync>;

/// Directory lock with a heartbeat and a per-generation marker. Releasing a lost lock must never
/// remove a replacement owner's directory. The same generation check runs immediately at fences.
pub struct HeldFileLock {
    path: PathBuf,
    marker: PathBuf,
    state: Arc<Mutex<LockState>>,
    heartbeat: JoinHandle<()>,
    on_lost: Option<LostCallback>,
}

fn check_generation(path: &Path, marker: &Path, state: &Mutex<LockState>, on_lost: Option<&LostCallback>) -> Result<(), FileLockLostError> {
    let error = {
        let mut state = state.lock().expect("experimental lock state");
        if let Some(error) = &state.lost { return Err(error.clone()); }
        if state.released { return Ok(()); }
        match std::fs::metadata(marker) {
            Ok(_) => return Ok(()),
            Err(cause) => {
                let error = FileLockLostError { path: path.to_owned(), cause: cause.to_string() };
                state.lost = Some(error.clone());
                error
            }
        }
    };
    if let Some(on_lost) = on_lost { on_lost(error.clone()); }
    Err(error)
}

impl HeldFileLock {
    pub async fn acquire(path: &Path, stale: Duration, update: Duration, wait: Duration, on_lost: Option<LostCallback>) -> Result<Self, ServerStartupError> {
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            match std::fs::create_dir(&lock_path) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    match std::fs::metadata(&lock_path).and_then(|metadata| metadata.modified()) {
                        Ok(modified) if modified.elapsed().is_ok_and(|age| age > stale) => {
                            match std::fs::remove_dir_all(&lock_path) {
                                Ok(()) => continue,
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                                Err(error) => return Err(error.into()),
                            }
                        }
                        Ok(_) => {},
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.into()),
                    }
                    if tokio::time::Instant::now() >= deadline { return Err(ServerStartupError::Startup("Timed out acquiring experimental server lock".into())); }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let marker = lock_path.join(uuid::Uuid::new_v4().to_string());
        if let Err(error) = std::fs::write(&marker, []) {
            if let Err(cleanup) = std::fs::remove_dir(&lock_path) {
                crate::output_guard::maho_write_stderr(&format!("Experimental lock acquisition cleanup failed: {cleanup}\n"));
            }
            return Err(error.into());
        }
        let state = Arc::new(Mutex::new(LockState { released: false, lost: None }));
        let heartbeat = {
            let path = path.to_owned(); let marker = marker.clone(); let lock_path = lock_path.clone();
            let state = state.clone(); let on_lost = on_lost.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(update);
                loop {
                    interval.tick().await;
                    if state.lock().expect("experimental lock state").released { break; }
                    if check_generation(&path, &marker, &state, on_lost.as_ref()).is_err() { break; }
                    let refresh = std::fs::File::open(&lock_path).and_then(|file| file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now())));
                    if let Err(cause) = refresh {
                        let error = {
                            let mut state = state.lock().expect("experimental lock state");
                            if state.released || state.lost.is_some() { break; }
                            let error = FileLockLostError { path: path.clone(), cause: cause.to_string() };
                            state.lost = Some(error.clone()); error
                        };
                        if let Some(on_lost) = &on_lost { on_lost(error); }
                        break;
                    }
                }
            })
        };
        Ok(Self { path: path.to_owned(), marker, state, heartbeat, on_lost })
    }
    pub fn assert_held(&self) -> Result<(), FileLockLostError> {
        check_generation(&self.path, &self.marker, &self.state, self.on_lost.as_ref())
    }
    pub fn lost(&self) -> bool { self.state.lock().expect("experimental lock state").lost.is_some() }
    pub async fn release(&self) -> Result<(), std::io::Error> {
        self.release_now()
    }
    fn release_now(&self) -> Result<(), std::io::Error> {
        let mut state = self.state.lock().expect("experimental lock state");
        if state.released { return Ok(()); }
        state.released = true;
        self.heartbeat.abort();
        if state.lost.is_some() { return Ok(()); }
        // Removing only our marker, then the empty directory, preserves a reclaimed generation.
        match std::fs::remove_file(&self.marker) {
            Ok(()) => std::fs::remove_dir(self.marker.parent().expect("lock marker parent")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}
impl Drop for HeldFileLock {
    fn drop(&mut self) {
        if let Err(error) = self.release_now() { crate::output_guard::maho_write_stderr(&format!("Experimental lock release failed: {error}\n")); }
    }
}

pub struct ServerProfile { pub server_id: String, lock: HeldFileLock }
impl ServerProfile {
    pub fn assert_held(&self) -> Result<(), FileLockLostError> { self.lock.assert_held() }
    pub async fn release(&self) -> Result<(), std::io::Error> { self.lock.release().await }
}
fn validate_server_id(value: &str) -> Result<String, ServerStartupError> {
    let id = uuid::Uuid::parse_str(value).map_err(|_| ServerStartupError::Startup(format!("Invalid experimental server ID: {value}")))?;
    if id.to_string() != value { return Err(ServerStartupError::Startup(format!("Invalid experimental server ID: {value}"))); }
    Ok(value.to_owned())
}

pub async fn acquire_server_profile(directory: &Path, requested_server_id: Option<&str>) -> Result<ServerProfile, ServerStartupError> {
    use std::io::Write;
    std::fs::create_dir_all(directory)?;
    let server_id = if let Some(id) = requested_server_id { validate_server_id(id)? } else {
        let path = directory.join(DEFAULT_SERVER_ID_FILE);
        match std::fs::read_to_string(&path) {
            Ok(value) => validate_server_id(value.trim())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let candidate = uuid::Uuid::new_v4().to_string();
                let mut options = std::fs::OpenOptions::new(); options.write(true).create_new(true);
                #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
                match options.open(&path) {
                    Ok(mut file) => { file.write_all(candidate.as_bytes())?; candidate },
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => validate_server_id(std::fs::read_to_string(path)?.trim())?,
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
    };
    let lock = HeldFileLock::acquire(&directory.join(format!("launcher-{server_id}")), Duration::from_millis(LOCK_STALE_MS), Duration::from_millis(LOCK_STALE_MS / 3), Duration::from_millis(LOCK_STALE_MS), None).await?;
    Ok(ServerProfile { server_id, lock })
}

/// Runtime-specific Unix transport and process operations. Startup failures must clean their own
/// partially initialized resources; once `start_backend` succeeds this module guarantees close.
#[async_trait::async_trait]
pub trait ServerStartup: Send {
    type Client: Send;
    type Child: Send;
    type Server: Send;
    async fn connect(&mut self, server_id: &str) -> Result<Option<Self::Client>, ServerStartupError>;
    async fn spawn(&mut self, server_id: &str) -> Result<Self::Child, ServerStartupError>;
    fn child_exited(&self, child: &Self::Child) -> bool;
    async fn terminate(&mut self, child: Self::Child) -> Result<(), ServerStartupError>;
    async fn start_backend(&mut self, server_id: &str) -> Result<Self::Server, ServerStartupError>;
    async fn connect_coordinator(&mut self, server: &mut Self::Server) -> Result<(), ServerStartupError>;
    async fn discover_and_refresh(&mut self, server: &mut Self::Server) -> Result<(), ServerStartupError>;
    async fn start_relay(&mut self, server: &mut Self::Server) -> Result<(), ServerStartupError>;
    async fn close(&mut self, server: Self::Server) -> Result<(), ServerStartupError>;
}

pub async fn activate_server<R: ServerStartup>(directory: &Path, requested_server_id: Option<&str>, model_selected: bool, runtime: &mut R) -> Result<(String, R::Client), ServerStartupError> {
    let profile = acquire_server_profile(directory, requested_server_id).await?;
    let server_id = profile.server_id.clone(); profile.release().await?;
    let activation = acquire_server_activation(directory, &server_id).await?;
    let result = async {
        if let Some(client) = runtime.connect(&server_id).await? {
            if model_selected { return Err(ServerStartupError::Startup("Model selection is only valid when automatically activating a new server".into())); }
            return Ok((server_id.clone(), client));
        }
        activation.assert_held()?;
        let child = runtime.spawn(&server_id).await?;
        let startup = async {
            let deadline = tokio::time::Instant::now() + Duration::from_millis(ACTIVATION_TIMEOUT_MS);
            loop {
                activation.assert_held()?;
                if let Some(client) = runtime.connect(&server_id).await? { return Ok((server_id.clone(), client)); }
                if runtime.child_exited(&child) { return Err(ServerStartupError::Startup("Automatically activated server exited during startup".into())); }
                if tokio::time::Instant::now() >= deadline { return Err(ServerStartupError::Startup("Timed out waiting for automatically activated server".into())); }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }.await;
        if startup.is_err() { runtime.terminate(child).await?; }
        startup
    }.await;
    activation.release().await?;
    result
}

pub async fn acquire_server_activation(directory: &Path, server_id: &str) -> Result<HeldFileLock, ServerStartupError> {
    HeldFileLock::acquire(&directory.join(format!("activation-{server_id}")), Duration::from_millis(ACTIVATION_TIMEOUT_MS * 2), Duration::from_millis(ACTIVATION_TIMEOUT_MS), Duration::from_millis(ACTIVATION_TIMEOUT_MS), None).await
}

pub async fn start_server<R: ServerStartup>(directory: &Path, server_id: &str, runtime: &mut R) -> Result<R::Server, ServerStartupError> {
    let profile = acquire_server_profile(directory, Some(server_id)).await?;
    let mut server = runtime.start_backend(&profile.server_id).await?;
    let started = async {
        runtime.connect_coordinator(&mut server).await?;
        profile.assert_held()?;
        runtime.discover_and_refresh(&mut server).await?;
        profile.assert_held()?;
        runtime.start_relay(&mut server).await?;
        profile.assert_held()?;
        Ok::<(), ServerStartupError>(())
    }.await;
    if let Err(error) = started {
        runtime.close(server).await?;
        profile.release().await?;
        return Err(error);
    }
    profile.release().await?;
    Ok(server)
}

pub async fn start_foreground_server<R: ServerStartup>(directory: &Path, requested_server_id: Option<&str>, runtime: &mut R) -> Result<R::Server, ServerStartupError> {
    let profile = acquire_server_profile(directory, requested_server_id).await?;
    let server_id = profile.server_id.clone(); profile.release().await?;
    let activation = acquire_server_activation(directory, &server_id).await?;
    let result = async {
        let server = start_server(directory, &server_id, runtime).await?;
        if let Err(error) = activation.assert_held() {
            if let Err(close_error) = runtime.close(server).await { crate::output_guard::maho_write_stderr(&format!("Server close after activation loss failed: {close_error}\n")); }
            return Err(error.into());
        }
        Ok(server)
    }.await;
    activation.release().await?;
    result
}

/// Await a bounded operation without introducing a polling loop.
pub(super) async fn bounded_report<F: Future<Output = Result<(), String>>>(report: F, timeout: Duration) {
    match tokio::time::timeout(timeout, report).await {
        Ok(Ok(())) => {},
        Ok(Err(error)) => { crate::output_guard::maho_write_stderr(&format!("Worker ownership-loss report failed: {error}\n")); },
        Err(_) => { crate::output_guard::maho_write_stderr("Worker ownership-loss report timed out\n"); },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "12345678-1234-4234-8234-123456789abc";

    fn reclaim(directory: &Path, kind: &str) {
        let path = directory.join(format!("{kind}-{ID}.lock"));
        std::fs::remove_dir_all(&path).expect("remove old generation");
        std::fs::create_dir(&path).expect("replacement generation");
        std::fs::write(path.join("replacement"), []).expect("replacement marker");
    }

    #[tokio::test]
    async fn profile_loss_is_typed_and_release_preserves_the_new_owner() {
        let directory = tempfile::tempdir().expect("directory");
        let profile = acquire_server_profile(directory.path(), Some(ID)).await.expect("profile");
        reclaim(directory.path(), "launcher");
        assert_eq!(profile.assert_held().expect_err("lost ownership").path, directory.path().join(format!("launcher-{ID}")));
        profile.release().await.expect("release lost generation");
        assert!(directory.path().join(format!("launcher-{ID}.lock/replacement")).exists());
    }

    struct Runtime { directory: PathBuf, lose_activation: bool, lose_launcher: bool, closed: bool, relay: bool, spawned: bool }
    #[async_trait::async_trait]
    impl ServerStartup for Runtime {
        type Client = (); type Child = (); type Server = ();
        async fn connect(&mut self, _: &str) -> Result<Option<()>, ServerStartupError> {
            if !self.spawned { return Ok(None); }
            reclaim(&self.directory, "activation");
            Ok(None)
        }
        async fn spawn(&mut self, _: &str) -> Result<(), ServerStartupError> { self.spawned = true; Ok(()) }
        fn child_exited(&self, _: &()) -> bool { false }
        async fn terminate(&mut self, _: ()) -> Result<(), ServerStartupError> { self.closed = true; Ok(()) }
        async fn start_backend(&mut self, _: &str) -> Result<(), ServerStartupError> { Ok(()) }
        async fn connect_coordinator(&mut self, _: &mut ()) -> Result<(), ServerStartupError> {
            if self.lose_launcher { reclaim(&self.directory, "launcher"); }
            Ok(())
        }
        async fn discover_and_refresh(&mut self, _: &mut ()) -> Result<(), ServerStartupError> {
            if self.lose_activation { reclaim(&self.directory, "activation"); }
            Ok(())
        }
        async fn start_relay(&mut self, _: &mut ()) -> Result<(), ServerStartupError> { self.relay = true; Ok(()) }
        async fn close(&mut self, _: ()) -> Result<(), ServerStartupError> { self.closed = true; Ok(()) }
    }
    fn runtime(directory: &Path) -> Runtime { Runtime { directory: directory.into(), lose_activation: false, lose_launcher: false, closed: false, relay: false, spawned: false } }

    #[tokio::test]
    async fn coordinator_fence_prevents_relay_start_and_closes_backend() {
        let directory = tempfile::tempdir().expect("directory"); let mut runtime = runtime(directory.path()); runtime.lose_launcher = true;
        assert!(matches!(start_server(directory.path(), ID, &mut runtime).await, Err(ServerStartupError::LockLost(_))));
        assert!(runtime.closed); assert!(!runtime.relay);
    }

    #[tokio::test]
    async fn foreground_activation_loss_closes_just_started_server() {
        let directory = tempfile::tempdir().expect("directory"); let mut runtime = runtime(directory.path()); runtime.lose_activation = true;
        assert!(matches!(start_foreground_server(directory.path(), Some(ID), &mut runtime).await, Err(ServerStartupError::LockLost(_))));
        assert!(runtime.closed); assert!(runtime.relay);
    }

    #[tokio::test]
    async fn activation_poll_checks_ownership_and_terminates_the_child() {
        let directory = tempfile::tempdir().expect("directory"); let mut runtime = runtime(directory.path());
        let result = tokio::time::timeout(Duration::from_secs(2), activate_server(directory.path(), Some(ID), false, &mut runtime)).await.expect("bounded activation");
        assert!(matches!(result, Err(ServerStartupError::LockLost(_)))); assert!(runtime.closed);
    }
}
