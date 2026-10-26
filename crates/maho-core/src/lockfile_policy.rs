//! Port of senpi packages/coding-agent/src/core/lockfile-policy.ts.

#[cfg(unix)]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const FILE_STORAGE_LOCK_STALE_MS: u64 = 30_000;
pub const FILE_STORAGE_LOCK_UPDATE_MS: u64 = 10_000;
pub const FILE_STORAGE_LOCK_RETRY_BUDGET_MS: u64 = 5_500;
pub const FILE_STORAGE_LOCK_RETRY_MIN_DELAY_MS: u64 = 100;
pub const FILE_STORAGE_LOCK_RETRY_MAX_DELAY_MS: u64 = 1_000;
pub const FILE_STORAGE_SYNC_LOCK_BUDGET_MS: u64 = 1_000;

#[cfg(unix)]
#[derive(Clone)]
pub struct FactsLockBrokerConfig {
    pub endpoint: PathBuf,
    pub auth_path: PathBuf,
    pub settings_path: PathBuf,
}

#[cfg(unix)]
static FACTS_LOCK_BROKER: OnceLock<Mutex<Option<FactsLockBrokerConfig>>> = OnceLock::new();

#[cfg(unix)]
fn broker_config() -> Option<FactsLockBrokerConfig> {
    FACTS_LOCK_BROKER.get()?.lock().ok()?.clone()
}

#[cfg(unix)]
pub fn install_facts_lock_broker(config: FactsLockBrokerConfig) {
    *FACTS_LOCK_BROKER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(config);
}

#[cfg(unix)]
pub struct FactsLockBrokerScope;

#[cfg(unix)]
impl FactsLockBrokerScope {
    pub fn install(config: FactsLockBrokerConfig) -> Self {
        install_facts_lock_broker(config);
        Self
    }
}

#[cfg(unix)]
impl Drop for FactsLockBrokerScope {
    fn drop(&mut self) {
        clear_facts_lock_broker();
    }
}

#[cfg(unix)]
pub fn facts_lock_broker_config_from_environment() -> Result<Option<FactsLockBrokerConfig>, String>
{
    let endpoint = match std::env::var_os("MAHO_FACTS_LOCK_BROKER") {
        Some(value) => PathBuf::from(value),
        None => return Ok(None),
    };
    let auth_path = std::env::var_os("MAHO_FACTS_AUTH_LOCK_TARGET")
        .map(PathBuf::from)
        .ok_or("facts broker auth target is missing")?;
    let settings_path = std::env::var_os("MAHO_FACTS_SETTINGS_LOCK_TARGET")
        .map(PathBuf::from)
        .ok_or("facts broker settings target is missing")?;
    Ok(Some(FactsLockBrokerConfig {
        endpoint,
        auth_path,
        settings_path,
    }))
}

#[cfg(unix)]
pub fn clear_facts_lock_broker() {
    if let Some(config) = FACTS_LOCK_BROKER.get() {
        *config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

pub fn facts_lock_broker_enabled() -> bool {
    #[cfg(unix)]
    {
        broker_config().is_some()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialStoreBusyError {
    pub path: String,
    pub waited_ms: u64,
}

impl CredentialStoreBusyError {
    pub fn new(path: impl Into<String>, waited_ms: u64) -> Self {
        Self {
            path: path.into(),
            waited_ms,
        }
    }

    pub fn message(&self) -> String {
        format!(
            "Credential store is busy: lock {} was held for {}ms. Another omo process may be refreshing credentials; close unused sessions if contention persists.",
            self.path, self.waited_ms
        )
    }
}

impl std::fmt::Display for CredentialStoreBusyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for CredentialStoreBusyError {}

pub fn is_lock_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
}

fn lock_dir(path: &str) -> PathBuf {
    let mut name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(".lock");
    Path::new(path).with_file_name(name)
}

#[derive(Debug)]
pub struct LockGuard {
    dir: PathBuf,
    held: Option<crate::file_lock::HeldFileLock>,
    #[cfg(unix)]
    broker: Option<UnixStream>,
    #[cfg(unix)]
    broker_key: Option<&'static str>,
}

impl LockGuard {
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.broker.is_some() {
            return;
        }
        if let Some(held) = &self.held {
            held.release();
            if !held.owns_directory() {
                return;
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl LockGuard {
    pub fn held_lock(&self) -> Option<crate::file_lock::HeldFileLock> {
        self.held.clone()
    }
    pub fn assert_held(&self) -> Result<(), String> {
        self.held
            .as_ref()
            .map_or(Ok(()), |held| held.assert_held().map_err(|e| e.to_string()))
    }
    pub fn broker_write(&mut self, content: &str) -> Result<bool, String> {
        self.assert_held()?;
        #[cfg(unix)]
        if let Some(stream) = &mut self.broker {
            let message =
                serde_json::json!({"key": self.broker_key.unwrap_or_default(), "write": content});
            writeln!(stream, "{message}").map_err(|error| error.to_string())?;
            let mut response = Vec::new();
            BufReader::new(stream.try_clone().map_err(|error| error.to_string())?)
                .take(16 * 1024)
                .read_until(b'\n', &mut response)
                .map_err(|error| error.to_string())?;
            if response.last() != Some(&b'\n') {
                return Err("invalid broker response frame".into());
            }
            let result: BrokerResponse =
                serde_json::from_slice(&response).map_err(|error| error.to_string())?;
            return result.result.map(|()| true);
        }
        let _ = content;
        Ok(false)
    }
}

#[cfg(unix)]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerResponse {
    result: Result<(), String>,
}

#[cfg(unix)]
fn broker_target(path: &str, config: &FactsLockBrokerConfig) -> Option<&'static str> {
    match (
        path,
        config.auth_path.to_str(),
        config.settings_path.to_str(),
    ) {
        (target, Some(auth), _) if target == auth => Some("auth"),
        (target, _, Some(settings)) if target == settings => Some("settings"),
        _ => None,
    }
}

#[cfg(unix)]
fn acquire_broker_lease(
    path: &str,
    budget_ms: u64,
) -> Result<Option<LockGuard>, CredentialStoreBusyError> {
    let Some(config) = broker_config() else {
        return Ok(None);
    };
    let key = broker_target(path, &config).ok_or_else(|| {
        CredentialStoreBusyError::new(format!("{path}: unsupported facts lock target"), 0)
    })?;
    let started = Instant::now();
    let mut stream = UnixStream::connect(config.endpoint)
        .map_err(|error| CredentialStoreBusyError::new(format!("{path}: {error}"), 0))?;
    let timeout = Duration::from_millis(budget_ms.max(1));
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| CredentialStoreBusyError::new(format!("{path}: {error}"), 0))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| CredentialStoreBusyError::new(format!("{path}: {error}"), 0))?;
    let request = serde_json::json!({"acquire": key, "budget_ms": budget_ms});
    writeln!(stream, "{request}")
        .map_err(|error| CredentialStoreBusyError::new(format!("{path}: {error}"), 0))?;
    let mut response = Vec::new();
    let frame_size = BufReader::new(
        stream
            .try_clone()
            .map_err(|error| CredentialStoreBusyError::new(format!("{path}: {error}"), 0))?,
    )
    .take(16 * 1024)
    .read_until(b'\n', &mut response)
    .map_err(|error| {
        CredentialStoreBusyError::new(
            format!("{path}: {error}"),
            started.elapsed().as_millis() as u64,
        )
    })?;
    if frame_size == 0 || response.last() != Some(&b'\n') {
        return Err(CredentialStoreBusyError::new(
            format!("{path}: invalid broker response frame"),
            started.elapsed().as_millis() as u64,
        ));
    }
    let response: BrokerResponse = serde_json::from_slice(&response)
        .map_err(|error| CredentialStoreBusyError::new(format!("{path}: {error}"), 0))?;
    response.result.map_err(|error| {
        CredentialStoreBusyError::new(
            format!("{path}: {error}"),
            started.elapsed().as_millis() as u64,
        )
    })?;
    Ok(Some(LockGuard {
        dir: lock_dir(path),
        held: None,
        broker: Some(stream),
        broker_key: Some(key),
    }))
}

fn try_acquire(dir: &Path) -> std::io::Result<()> {
    match std::fs::create_dir(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if let Ok(metadata) = std::fs::metadata(dir)
                && let Ok(modified) = metadata.modified()
                && modified
                    .elapsed()
                    .map(|age| age.as_millis() as u64 > FILE_STORAGE_LOCK_STALE_MS)
                    .unwrap_or(false)
            {
                let _ = std::fs::remove_dir_all(dir);
                return std::fs::create_dir(dir);
            }
            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "ELOCKED",
            ))
        }
        Err(error) => Err(error),
    }
}

pub fn acquire_lock_sync(
    path: &str,
    budget_ms: u64,
) -> Result<LockGuard, CredentialStoreBusyError> {
    #[cfg(unix)]
    if let Some(guard) = acquire_broker_lease(path, budget_ms)? {
        return Ok(guard);
    }
    acquire_host_lock_sync(path, budget_ms)
}

pub fn acquire_host_lock_sync(
    path: &str,
    budget_ms: u64,
) -> Result<LockGuard, CredentialStoreBusyError> {
    let dir = lock_dir(path);
    if let Some(parent) = dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let started = Instant::now();
    let mut delay_ms = FILE_STORAGE_LOCK_RETRY_MIN_DELAY_MS;
    loop {
        match try_acquire(&dir) {
            Ok(()) => {
                let held = crate::file_lock::HeldFileLock::new(Path::new(path), &dir, None)
                    .map_err(|e| CredentialStoreBusyError::new(format!("{path}: {e}"), 0))?;
                held.start_heartbeat();
                return Ok(LockGuard {
                    dir,
                    held: Some(held),
                    #[cfg(unix)]
                    broker: None,
                    #[cfg(unix)]
                    broker_key: None,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let waited = started.elapsed().as_millis() as u64;
                if waited >= budget_ms {
                    return Err(CredentialStoreBusyError::new(path, waited));
                }
                std::thread::sleep(Duration::from_millis(
                    delay_ms.min(budget_ms.saturating_sub(waited)).max(1),
                ));
                delay_ms = (delay_ms * 2).min(FILE_STORAGE_LOCK_RETRY_MAX_DELAY_MS);
            }
            Err(error) => {
                return Err(CredentialStoreBusyError::new(
                    format!("{path}: {error}"),
                    started.elapsed().as_millis() as u64,
                ));
            }
        }
    }
}

pub async fn acquire_lock_async(
    path: &str,
    budget_ms: u64,
) -> Result<LockGuard, CredentialStoreBusyError> {
    #[cfg(unix)]
    if broker_config().is_some() {
        let path = path.to_owned();
        return tokio::task::spawn_blocking(move || acquire_lock_sync(&path, budget_ms))
            .await
            .map_err(|error| CredentialStoreBusyError::new(error.to_string(), 0))?;
    }
    let dir = lock_dir(path);
    if let Some(parent) = dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let started = Instant::now();
    let mut delay_ms = FILE_STORAGE_LOCK_RETRY_MIN_DELAY_MS;
    loop {
        match try_acquire(&dir) {
            Ok(()) => {
                let held = crate::file_lock::HeldFileLock::new(Path::new(path), &dir, None)
                    .map_err(|e| CredentialStoreBusyError::new(format!("{path}: {e}"), 0))?;
                held.start_heartbeat();
                return Ok(LockGuard {
                    dir,
                    held: Some(held),
                    #[cfg(unix)]
                    broker: None,
                    #[cfg(unix)]
                    broker_key: None,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let waited = started.elapsed().as_millis() as u64;
                if waited >= budget_ms {
                    return Err(CredentialStoreBusyError::new(path, waited));
                }
                tokio::time::sleep(Duration::from_millis(
                    delay_ms.min(budget_ms.saturating_sub(waited)).max(1),
                ))
                .await;
                delay_ms = (delay_ms * 2).min(FILE_STORAGE_LOCK_RETRY_MAX_DELAY_MS);
            }
            Err(error) => {
                return Err(CredentialStoreBusyError::new(
                    format!("{path}: {error}"),
                    started.elapsed().as_millis() as u64,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquires_and_releases_a_lock() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("auth.json");
        std::fs::write(&target, "{}").expect("write");
        let path = target.to_string_lossy().into_owned();
        let guard = acquire_lock_sync(&path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS).expect("lock");
        assert!(guard.path().exists());
        let dir = lock_dir(&path);
        drop(guard);
        assert!(!dir.exists());
    }

    #[test]
    fn reports_a_busy_store_when_the_budget_elapses() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("auth.json");
        std::fs::write(&target, "{}").expect("write");
        let path = target.to_string_lossy().into_owned();
        let _held = acquire_lock_sync(&path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS).expect("lock");
        let error = acquire_lock_sync(&path, 50).expect_err("busy");
        assert!(
            error
                .message()
                .starts_with("Credential store is busy: lock ")
        );
        assert!(error.message().contains("was held for"));
    }
}

#[cfg(test)]
mod loss_tests {
    use super::*;
    #[test]
    fn replacement_owner_is_not_deleted_or_allowed_to_write() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory
            .path()
            .join("auth.json")
            .to_string_lossy()
            .into_owned();
        let guard = acquire_host_lock_sync(&path, 0).expect("lock");
        assert!(guard.assert_held().is_ok());
        let lock_path = guard.path().to_path_buf();
        std::fs::write(lock_path.join("owner-token"), "replacement").expect("replace");
        assert!(guard.assert_held().is_err());
        drop(guard);
        assert!(lock_path.exists());
    }
}
