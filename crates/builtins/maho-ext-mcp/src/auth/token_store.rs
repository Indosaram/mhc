use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStoredAuth {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_info: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_verifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
    /// Authorization server that issued the grant (senpi#4194); refresh tokens are presented only to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
#[derive(Debug, thiserror::Error)]
pub enum TokenStoreError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("Could not acquire MCP auth lock at {path}: {cause}")]
    Lock {
        path: PathBuf,
        cause: std::io::Error,
    },
}
pub fn hash_server_url(url: &str) -> String {
    format!("{:x}", Sha256::digest(url))
}
/// Upstream `hashServerKey`: credentials are keyed by server name and URL, so two servers sharing a URL
/// (a work and a personal account) never share or sign out each other's grant.
pub fn hash_server_key(server_name: &str, server_url: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{server_name}\0{server_url}"))
    )
}
const LEGACY_LOCK_STALE_MS: u64 = 30_000;
#[derive(Clone)]
pub struct McpTokenStore {
    pub server_name: String,
    pub server_url: String,
    agent_dir: PathBuf,
    hash: String,
    legacy_hash: String,
    pub lock_retries: usize,
    pub disable_lock: bool,
}
impl McpTokenStore {
    pub fn new(agent_dir: &Path, server_name: &str, server_url: &str) -> Self {
        Self {
            server_name: server_name.into(),
            server_url: server_url.into(),
            agent_dir: agent_dir.into(),
            hash: hash_server_key(server_name, server_url),
            legacy_hash: hash_server_url(server_url),
            lock_retries: 50,
            disable_lock: false,
        }
    }
    pub fn root_dir(&self) -> PathBuf {
        self.agent_dir.join("mcp-auth")
    }
    pub fn dir(&self) -> PathBuf {
        self.root_dir().join(&self.hash)
    }
    pub fn tokens_path(&self) -> PathBuf {
        self.dir().join("tokens.json")
    }
    pub fn lock_path(&self) -> PathBuf {
        self.dir().join("tokens.json.lock")
    }
    /// The directory an older build stored this URL's record in (keyed by URL alone).
    pub fn legacy_dir(&self) -> PathBuf {
        self.root_dir().join(&self.legacy_hash)
    }
    fn legacy_lock_file(&self) -> PathBuf {
        self.root_dir()
            .join(format!("{}.migrate.lock", self.legacy_hash))
    }
    fn migration(&self) -> super::legacy_migration::LegacyMigration<McpStoredAuth> {
        let store = self.clone();
        let lock_path = self.legacy_lock_file();
        super::legacy_migration::LegacyMigration {
            root_dir: self.root_dir(),
            lock_path: lock_path.clone(),
            legacy_tokens: self.legacy_dir().join("tokens.json"),
            tokens_path: self.tokens_path(),
            stale_ms: LEGACY_LOCK_STALE_MS,
            disable_lock: self.disable_lock,
            read: Box::new(|path: &Path| read_json(path).ok().flatten()),
            write: Box::new(move |record: &McpStoredAuth| {
                store
                    .write_unlocked(Some(record))
                    .map_err(std::io::Error::other)
            }),
            lock_error: Box::new(move |cause: std::io::Error| {
                std::io::Error::other(format!(
                    "Could not acquire MCP auth lock at {}: {cause}",
                    lock_path.display()
                ))
            }),
        }
    }
    /// A record stored by URL alone moves to the first server that reads it.
    pub fn read(&self) -> Result<Option<McpStoredAuth>, TokenStoreError> {
        let record = match read_json(&self.tokens_path())? {
            Some(record) => Some(record),
            None => super::legacy_migration::adopt_legacy_record(&self.migration())?,
        };
        Ok(attribute_legacy_issuer(record))
    }
    /// Upstream `readAsync`: the legacy claim waits on the migration lock without blocking the runtime.
    pub async fn read_async(&self) -> Result<Option<McpStoredAuth>, TokenStoreError> {
        Ok(attribute_legacy_issuer(
            super::legacy_migration::read_record_async(&self.migration()).await?,
        ))
    }
    fn ensure_dir(&self) -> Result<(), TokenStoreError> {
        fs::create_dir_all(self.dir())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(self.dir(), fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    pub fn with_lock<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, TokenStoreError>,
    ) -> Result<T, TokenStoreError> {
        self.ensure_dir()?;
        if self.disable_lock {
            return operation(self);
        }
        let mut opts = fs::OpenOptions::new();
        opts.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let lock = opts.open(self.lock_path())?;
        let mut delay = 20u64;
        for attempt in 0..=self.lock_retries {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if attempt < self.lock_retries => {
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                    delay = (delay * 6 / 5).min(200);
                }
                Err(error) => {
                    return Err(TokenStoreError::Lock {
                        path: self.lock_path(),
                        cause: match error {
                            std::fs::TryLockError::WouldBlock => std::io::Error::new(
                                std::io::ErrorKind::WouldBlock,
                                "Lock file is already being held",
                            ),
                            std::fs::TryLockError::Error(error) => error,
                        },
                    });
                }
            }
        }
        let result = operation(self);
        lock.unlock()?;
        result
    }
    pub fn update(
        &self,
        mutate: impl FnOnce(Option<McpStoredAuth>) -> Option<McpStoredAuth>,
    ) -> Result<Option<McpStoredAuth>, TokenStoreError> {
        self.with_lock(|store| {
            let next = mutate(store.read()?);
            store.write_unlocked(next.as_ref())?;
            Ok(next)
        })
    }
    pub fn write(&self, record: McpStoredAuth) -> Result<(), TokenStoreError> {
        self.update(|_| Some(record))?;
        Ok(())
    }
    pub fn write_unlocked(&self, record: Option<&McpStoredAuth>) -> Result<(), TokenStoreError> {
        if let Some(record) = record {
            self.ensure_dir()?;
            atomic_json(&self.tokens_path(), record)?;
            self.write_index(false)?;
        } else {
            remove_file(&self.tokens_path())?;
            self.write_index(true)?;
        }
        Ok(())
    }
    pub fn clear(&self) -> Result<(), TokenStoreError> {
        self.with_lock(|store| {
            match fs::remove_dir_all(store.dir()) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            };
            // Signing out also removes the URL-keyed record this server would otherwise take over.
            remove_file(&store.legacy_dir().join("tokens.json"))?;
            Ok(())
        })?;
        self.write_index(true)
    }
    fn write_index(&self, remove: bool) -> Result<(), TokenStoreError> {
        let path = self.root_dir().join("index.json");
        if remove && !path.exists() {
            return Ok(());
        }
        let mut index: BTreeMap<String, String> = read_json(&path)?.unwrap_or_default();
        if remove {
            if index
                .get(&self.server_name)
                .is_some_and(|hash| hash != &self.hash)
            {
                return Ok(());
            }
            index.remove(&self.server_name);
        } else {
            if index.get(&self.server_name) == Some(&self.hash) {
                return Ok(());
            }
            index.insert(self.server_name.clone(), self.hash.clone());
        }
        atomic_json(&path, &index)
    }
}
/// Discovery state is no longer persisted; drop the key from legacy records. A record saved before the
/// issuer stamp existed is first attributed to the authorization server its sign-in discovered (senpi#2940
/// `storedGrantIssuer`), so the legacy key is read before it is stripped.
fn attribute_legacy_issuer(mut record: Option<McpStoredAuth>) -> Option<McpStoredAuth> {
    if let Some(record) = record.as_mut()
        && let Some(state) = record.extra.remove("discoveryState")
        && record.issuer.is_none()
    {
        record.issuer = state
            .get("authorizationServerUrl")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .map(str::to_owned);
    }
    record
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, TokenStoreError> {
    let text = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&text)?))
}
fn atomic_json(path: &Path, data: &impl Serialize) -> Result<(), TokenStoreError> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "auth path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::Builder::new()
        .prefix("mcp-auth-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    writeln!(tmp, "{}", serde_json::to_string_pretty(data)?)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
fn remove_file(path: &Path) -> Result<(), std::io::Error> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
