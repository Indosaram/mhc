//! Port of senpi packages/coding-agent/src/core/session-sidecar-store.ts.
//!
//! Generic atomic, versioned, per-session sidecar store: a per-file lock serializes read-modify-write
//! cycles, writes go through a temp file with 0600 permissions and a rename, and parsing fails
//! closed on version or session mismatches.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarError {
    Invalid(String),
    UnsupportedVersion(String),
    Io(String),
}

impl std::fmt::Display for SidecarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::UnsupportedVersion(message) => f.write_str(message),
            Self::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SidecarError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarStoreRef {
    pub base_dir: String,
    pub session_id: String,
}

pub type SidecarPayloadParser = Arc<dyn Fn(&Value, &SidecarStoreRef) -> Result<Value, SidecarError> + Send + Sync>;

#[derive(Clone)]
pub struct CreateSidecarStoreOptions {
    pub base_dir: String,
    pub session_id: String,
    pub version: i64,
    pub temp_prefix: String,
    pub parse: SidecarPayloadParser,
}

pub fn encoded_session_id(session_id: &str) -> String {
    let mut encoded = String::new();
    for byte in session_id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

pub fn sidecar_file_path(reference: &SidecarStoreRef) -> String {
    Path::new(&reference.base_dir).join(format!("{}.json", encoded_session_id(&reference.session_id))).to_string_lossy().into_owned()
}

fn snapshots() -> &'static Mutex<HashMap<String, Value>> {
    static SNAPSHOTS: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();
    SNAPSHOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn tails() -> &'static Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    static TAILS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    TAILS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn tail_for(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut tails = tails().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(tails.entry(key.to_owned()).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))))
}

/// Serializes read-modify-write cycles per key across every sidecar consumer.
pub async fn serialize_by_key<T>(key: &str, operation: impl std::future::Future<Output = T>) -> T {
    let tail = tail_for(key);
    let _guard = tail.lock().await;
    operation.await
}

#[derive(Clone)]
pub struct SidecarStore {
    file_path: String,
    options: CreateSidecarStoreOptions,
    reference: SidecarStoreRef,
}

impl SidecarStore {
    pub fn file_path(&self) -> &str {
        &self.file_path
    }

    pub async fn read(&self) -> Result<Option<Value>, SidecarError> {
        let raw = match tokio::fs::read_to_string(&self.file_path).await {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SidecarError::Io(error.to_string())),
        };
        let state = parse_payload(&raw, &self.options, &self.reference)?;
        snapshots().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(self.file_path.clone(), state.clone());
        Ok(Some(state))
    }

    pub async fn write(&self, state: &Value) -> Result<(), SidecarError> {
        if let Some(parent) = Path::new(&self.file_path).parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| SidecarError::Io(error.to_string()))?;
        }
        let contents = format!("{}\n", serde_json::to_string_pretty(state).unwrap_or_default());
        write_atomic(&self.file_path, &contents, &self.options.temp_prefix).await?;
        snapshots().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(self.file_path.clone(), state.clone());
        Ok(())
    }

    pub async fn mutate(
        &self,
        mutate: impl FnOnce(Option<&Value>) -> Result<Value, SidecarError>,
    ) -> Result<Value, SidecarError> {
        let store = self.clone();
        serialize_by_key(&self.file_path.clone(), async move {
            let current = store.read().await?;
            let next = mutate(current.as_ref())?;
            store.write(&next).await?;
            Ok(next)
        })
        .await
    }

    pub fn snapshot(&self) -> Option<Value> {
        snapshots().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&self.file_path).cloned()
    }

    pub fn clear(&self) {
        snapshots().lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&self.file_path);
    }
}

/// Creates a per-session store bound to baseDir/<encoded session id>.json.
pub fn create_sidecar_store(options: CreateSidecarStoreOptions) -> SidecarStore {
    let reference = SidecarStoreRef { base_dir: options.base_dir.clone(), session_id: options.session_id.clone() };
    let file_path = sidecar_file_path(&reference);
    SidecarStore { file_path, options, reference }
}

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

/// Writes contents to filePath atomically: a hidden temp file (0600) next to the target, then a
/// rename over it.
pub async fn write_atomic(file_path: &str, contents: &str, temp_prefix: &str) -> Result<(), SidecarError> {
    let directory = Path::new(file_path).parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let temp_path = directory.join(format!(".{temp_prefix}-{}.tmp", uuid::Uuid::now_v7()));
    let write_result = async {
        tokio::fs::write(&temp_path, contents).await.map_err(|error| SidecarError::Io(error.to_string()))?;
        set_mode(&temp_path, 0o600);
        tokio::fs::rename(&temp_path, file_path).await.map_err(|error| SidecarError::Io(error.to_string()))
    }
    .await;
    if let Err(error) = write_result {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error);
    }
    Ok(())
}

fn parse_payload(raw: &str, options: &CreateSidecarStoreOptions, reference: &SidecarStoreRef) -> Result<Value, SidecarError> {
    let parsed: Value = serde_json::from_str(raw)
        .map_err(|_| SidecarError::Invalid("sidecar store contains unparseable JSON".to_owned()))?;
    let Some(object) = parsed.as_object() else {
        return Err(SidecarError::Invalid("sidecar store must be a JSON object".to_owned()));
    };
    if object.get("version").and_then(Value::as_i64) != Some(options.version) {
        let found = object.get("version").cloned().unwrap_or(Value::Null);
        return Err(SidecarError::UnsupportedVersion(format!(
            "unsupported sidecar store version: {} (expected {})",
            found, options.version
        )));
    }
    let session_id = object.get("sessionId").and_then(Value::as_str).filter(|value| !value.is_empty());
    let Some(session_id) = session_id else {
        return Err(SidecarError::Invalid("sidecar store is missing a sessionId".to_owned()));
    };
    if session_id != reference.session_id {
        return Err(SidecarError::Invalid(format!(
            "sidecar store sessionId {session_id} does not match the session it was loaded for"
        )));
    }
    (options.parse)(&parsed, reference)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn options(dir: &str) -> CreateSidecarStoreOptions {
        CreateSidecarStoreOptions {
            base_dir: dir.to_owned(),
            session_id: "session/one".to_owned(),
            version: 2,
            temp_prefix: "sidecar".to_owned(),
            parse: Arc::new(|raw, _reference| Ok(raw.clone())),
        }
    }

    #[test]
    fn the_encoded_session_id_escapes_reserved_characters() {
        assert_eq!(encoded_session_id("session/one"), "session%2Fone");
        assert_eq!(encoded_session_id("plain-1.2_ok"), "plain-1.2_ok");
    }

    #[test]
    fn the_sidecar_path_uses_the_encoded_id() {
        let reference = SidecarStoreRef { base_dir: "/tmp/x".to_owned(), session_id: "a/b".to_owned() };
        assert_eq!(sidecar_file_path(&reference), "/tmp/x/a%2Fb.json");
    }

    #[tokio::test]
    async fn writing_then_reading_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = create_sidecar_store(options(&dir.path().to_string_lossy()));
        store.write(&json!({ "version": 2, "sessionId": "session/one", "value": 7 })).await.expect("write");
        let read = store.read().await.expect("read").expect("present");
        assert_eq!(read["value"], json!(7));
        assert_eq!(store.snapshot().expect("snapshot")["value"], json!(7));
    }

    #[tokio::test]
    async fn a_missing_file_reads_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = create_sidecar_store(options(&dir.path().to_string_lossy()));
        assert!(store.read().await.expect("read").is_none());
    }

    #[tokio::test]
    async fn mutate_serializes_and_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = create_sidecar_store(options(&dir.path().to_string_lossy()));
        let next = store
            .mutate(|_current| Ok(json!({ "version": 2, "sessionId": "session/one", "count": 1 })))
            .await
            .expect("mutate");
        assert_eq!(next["count"], json!(1));
        assert!(store.read().await.expect("read").is_some());
    }

    #[tokio::test]
    async fn a_version_mismatch_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = create_sidecar_store(options(&dir.path().to_string_lossy()));
        store.write(&json!({ "version": 1, "sessionId": "session/one" })).await.expect("write");
        assert!(matches!(store.read().await, Err(SidecarError::UnsupportedVersion(_))));
    }

    #[tokio::test]
    async fn a_session_mismatch_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = create_sidecar_store(options(&dir.path().to_string_lossy()));
        store.write(&json!({ "version": 2, "sessionId": "other" })).await.expect("write");
        assert!(matches!(store.read().await, Err(SidecarError::Invalid(_))));
    }

    #[tokio::test]
    async fn clear_drops_the_snapshot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = create_sidecar_store(options(&dir.path().to_string_lossy()));
        store.write(&json!({ "version": 2, "sessionId": "session/one" })).await.expect("write");
        store.clear();
        assert!(store.snapshot().is_none());
    }
}
