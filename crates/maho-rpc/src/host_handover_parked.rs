//! The handoff-parked residency record (senpi `modes/rpc/host-handover-parked.ts`): the set of sessions a
//! predecessor host held resident and parked with `handoff_parked` (or its idle sweep), persisted so the
//! SUCCESSOR can reopen them by path with the same durable session id (senpi#3282).
//!
//! A `retain_on_disconnect` session must survive EVERY generation. Each parked session is ONE file
//! (`<daemonDir>/handover-parked/<sha>.json`) written atomically, so a successor removes exactly the entry it
//! reopened and nothing else: a predecessor that parks another session concurrently cannot be clobbered by that
//! removal, across processes. There is no shared list file to read-merge-write, so there is no lost update to
//! lock against. The predecessor appends an entry (awaited, in-process mutex so two parks in one host keep both);
//! the successor reopens each entry with the same durable id and `retain_on_disconnect` and deletes ONLY that
//! entry's file after success (crash-safe, at most once, never lost). An entry whose session file is gone is
//! dropped, never retried.
use crate::host_handover_reopen::{BoxFuture, HandoverParkedStore};
use crate::rpc_input_validation::{
    browser_engine_error, prompt_surface_error, retry_fallback_error, session_auto_title_error,
    session_context_error, session_kind_error,
};
use crate::session_context_patch::apply_context_patch;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// `HandoverParkedEntry.model`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParkedModel {
    pub provider: String,
    pub model_id: String,
}

/// One parked session. JSON field names are senpi's camelCase.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoverParkedEntry {
    pub session_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_session_id: Option<String>,
    #[serde(default)]
    pub retain_on_disconnect: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ParkedModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    /// The session's launch-profile choices a client-less reopen has no other source for: without them the
    /// successor opens the session under the host defaults, and an `ask` session runs gated tools without
    /// asking (senpi#3364).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_engine: Option<String>,
    /// The rest of the client's `open_session` choices (senpi#3368): its visibility class, the CLIENT part of
    /// its context (host-owned keys are the successor's own and re-merged on open), its auto-titling and its
    /// retry fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_context: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_overlay: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_title: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_fallback: Option<Value>,
    /// The host instance that wrote the park. Its own reopen watch skips it: the park is for the NEXT
    /// generation (senpi#3485). Absent on entries an older generation wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_by: Option<String>,
}

const PERMISSION_PRESET_NAMES: [&str; 6] = [
    "full-access",
    "workspace",
    "accept-edits",
    "auto",
    "read-only",
    "ask",
];
fn record_dir(daemon_dir: &Path) -> PathBuf {
    daemon_dir.join("handover-parked")
}

fn entry_file(daemon_dir: &Path, session_path: &str) -> PathBuf {
    let digest = Sha256::digest(session_path.as_bytes());
    let name: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    record_dir(daemon_dir).join(format!("{name}.json"))
}

/// Mutations of one host run one at a time, so two parks in one host keep both.
static MUTATIONS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn absent_or(
    record: &serde_json::Map<String, Value>,
    key: &str,
    accepts: fn(&Value) -> bool,
) -> bool {
    record.get(key).is_none_or(accepts)
}

/// `isEntry` plus the typed read: `None` for a well-formed value this version does not understand.
pub fn parse_entry(value: &Value) -> Option<HandoverParkedEntry> {
    let record = value.as_object()?;
    let valid = record.get("sessionPath").is_some_and(Value::is_string)
        && record
            .get("retainOnDisconnect")
            .is_some_and(Value::is_boolean)
        && absent_or(record, "durableSessionId", Value::is_string)
        && absent_or(record, "cwd", Value::is_string)
        && absent_or(record, "parkedBy", Value::is_string)
        && absent_or(record, "thinkingLevel", Value::is_string)
        && absent_or(record, "permissionPreset", |preset| {
            preset
                .as_str()
                .is_some_and(|name| PERMISSION_PRESET_NAMES.contains(&name))
        })
        && prompt_surface_error(record.get("promptSurface")).is_none()
        && browser_engine_error(record.get("browserEngine")).is_none()
        && session_kind_error(record.get("sessionKind")).is_none()
        && session_context_error(record.get("sessionContext")).is_none()
        && record
            .get("contextOverlay")
            .is_none_or(|overlay| apply_context_patch(None, overlay).is_ok())
        && session_auto_title_error(record.get("autoTitle")).is_none()
        && retry_fallback_error(record.get("retryFallback")).is_none()
        && absent_or(record, "model", |model| {
            model.as_object().is_some_and(|model| {
                model.get("provider").is_some_and(Value::is_string)
                    && model.get("modelId").is_some_and(Value::is_string)
            })
        });
    if !valid {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

fn write_entry_file(daemon_dir: &Path, entry: &HandoverParkedEntry) -> io::Result<()> {
    let dir = record_dir(daemon_dir);
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&dir)?;
    }
    let file = entry_file(daemon_dir, &entry.session_path);
    let tmp = file.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut text = serde_json::to_string_pretty(entry).map_err(io::Error::other)?;
    text.push('\n');
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write;
        let mut handle = options.open(&tmp)?;
        if let Err(error) = handle.write_all(text.as_bytes()) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
    }
    std::fs::rename(&tmp, &file).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// `recordHandoverParkedSession`.
pub async fn record_handover_parked_session(
    daemon_dir: &Path,
    entry: &HandoverParkedEntry,
) -> io::Result<()> {
    let _guard = MUTATIONS.lock().await;
    write_entry_file(daemon_dir, entry)
}

/// `readHandoverParkedSessions`. Only a genuinely corrupt payload (a JSON syntax error) is dropped: a
/// well-formed value that fails the shape check may be an entry a different-version generation wrote (a
/// rollback handover) and is left alone, and a read error (a race with a concurrent clear, EMFILE, EACCES) skips
/// the entry for the next read.
pub async fn read_handover_parked_sessions(daemon_dir: &Path) -> Vec<HandoverParkedEntry> {
    let Ok(listing) = std::fs::read_dir(record_dir(daemon_dir)) else {
        return Vec::new();
    };
    let mut names: Vec<PathBuf> = listing
        .filter_map(Result::ok)
        .map(|item| item.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    names.sort();
    let mut entries = Vec::new();
    for file in names {
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        match serde_json::from_str::<Value>(&String::from_utf8_lossy(&bytes)) {
            Ok(parsed) => {
                if let Some(entry) = parse_entry(&parsed) {
                    entries.push(entry);
                }
            }
            Err(_) => {
                let name = file
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned());
                maho_core::output_guard::maho_write_stderr(&format!(
                    "senpi rpc handover-parked entry {} is corrupt; dropping it\n",
                    name.unwrap_or_default()
                ));
                let _ = std::fs::remove_file(&file);
            }
        }
    }
    entries
}

/// `clearHandoverParkedSession`: ENOENT is a normal race with a concurrent clear.
pub async fn clear_handover_parked_session(daemon_dir: &Path, session_path: &str) {
    let _guard = MUTATIONS.lock().await;
    let _ = std::fs::remove_file(entry_file(daemon_dir, session_path));
}

/// `dropHandoverParkedSessionIfFileGone`.
pub async fn drop_handover_parked_session_if_file_gone(daemon_dir: &Path, session_path: &str) {
    if Path::new(session_path).exists() {
        return;
    }
    clear_handover_parked_session(daemon_dir, session_path).await;
}

/// The file-backed [`HandoverParkedStore`] over `<daemonDir>/handover-parked/*.json`.
#[derive(Debug, Clone)]
pub struct FileHandoverParkedStore {
    daemon_dir: PathBuf,
}

impl FileHandoverParkedStore {
    pub fn new(daemon_dir: impl Into<PathBuf>) -> Self {
        Self {
            daemon_dir: daemon_dir.into(),
        }
    }
}

impl HandoverParkedStore for FileHandoverParkedStore {
    fn read(&self) -> BoxFuture<'_, Vec<HandoverParkedEntry>> {
        Box::pin(read_handover_parked_sessions(&self.daemon_dir))
    }
    fn clear(&self, session_path: &str) -> BoxFuture<'_, ()> {
        let session_path = session_path.to_owned();
        Box::pin(
            async move { clear_handover_parked_session(&self.daemon_dir, &session_path).await },
        )
    }
    fn drop_if_file_gone(&self, session_path: &str) -> BoxFuture<'_, ()> {
        let session_path = session_path.to_owned();
        Box::pin(async move {
            drop_handover_parked_session_if_file_gone(&self.daemon_dir, &session_path).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn file_of(dir: &Path, session_path: &str) -> PathBuf {
        entry_file(dir, session_path)
    }

    fn entry(path: &str) -> HandoverParkedEntry {
        HandoverParkedEntry {
            session_path: path.into(),
            retain_on_disconnect: true,
            ..Default::default()
        }
    }

    fn write_raw(dir: &Path, session_path: &str, body: &Value) -> PathBuf {
        let file = file_of(dir, session_path);
        std::fs::create_dir_all(record_dir(dir)).unwrap();
        std::fs::write(&file, format!("{body}\n")).unwrap();
        file
    }

    #[test]
    fn entry_file_name_is_the_first_16_hex_chars_of_the_path_sha256() {
        let file = entry_file(Path::new("/d"), "abc");
        assert_eq!(file, Path::new("/d/handover-parked/ba7816bf8f01cfea.json"));
    }

    #[tokio::test]
    async fn drops_a_corrupt_unparseable_entry_file_and_keeps_going() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir
            .path()
            .join("corrupt.jsonl")
            .to_string_lossy()
            .into_owned();
        let file = file_of(dir.path(), &session_path);
        std::fs::create_dir_all(record_dir(dir.path())).unwrap();
        std::fs::write(&file, "{ not json").unwrap();
        assert!(read_handover_parked_sessions(dir.path()).await.is_empty());
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn keeps_a_well_formed_entry_written_by_another_version() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_raw(
            dir.path(),
            "/other.jsonl",
            &json!({"future":true,"sessionPath":42}),
        );
        assert!(read_handover_parked_sessions(dir.path()).await.is_empty());
        assert!(file.exists());
    }

    #[tokio::test]
    async fn round_trips_a_recorded_entry() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir
            .path()
            .join("session.jsonl")
            .to_string_lossy()
            .into_owned();
        let mut parked = entry(&session_path);
        parked.durable_session_id = Some("29510000-0000-4000-8000-000000000010".into());
        parked.cwd = Some(dir.path().to_string_lossy().into_owned());
        record_handover_parked_session(dir.path(), &parked)
            .await
            .unwrap();
        let entries = read_handover_parked_sessions(dir.path()).await;
        assert_eq!(entries, vec![parked]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_record_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        record_handover_parked_session(dir.path(), &entry("/s.jsonl"))
            .await
            .unwrap();
        let file_mode = std::fs::metadata(file_of(dir.path(), "/s.jsonl"))
            .unwrap()
            .permissions()
            .mode();
        let dir_mode = std::fs::metadata(record_dir(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(file_mode & 0o777, 0o600);
        assert_eq!(dir_mode & 0o777, 0o700);
    }

    #[tokio::test]
    async fn round_trips_the_preset_prompt_surface_and_browser_engine() {
        let dir = tempfile::tempdir().unwrap();
        let mut parked = entry("/preset.jsonl");
        parked.permission_preset = Some("ask".into());
        parked.prompt_surface = Some("app".into());
        parked.browser_engine = Some("builtin".into());
        record_handover_parked_session(dir.path(), &parked)
            .await
            .unwrap();
        assert_eq!(
            read_handover_parked_sessions(dir.path()).await,
            vec![parked]
        );
    }

    #[tokio::test]
    async fn keeps_but_does_not_return_an_entry_with_an_unknown_prompt_surface() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_raw(
            dir.path(),
            "/future.jsonl",
            &json!({"sessionPath":"/future.jsonl","retainOnDisconnect":true,"promptSurface":"hologram"}),
        );
        assert!(read_handover_parked_sessions(dir.path()).await.is_empty());
        assert!(file.exists());
    }

    #[tokio::test]
    async fn keeps_but_does_not_return_an_entry_with_an_unknown_permission_preset() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_raw(
            dir.path(),
            "/future-preset.jsonl",
            &json!({"sessionPath":"/future-preset.jsonl","retainOnDisconnect":true,"permissionPreset":"future-preset"}),
        );
        assert!(read_handover_parked_sessions(dir.path()).await.is_empty());
        assert!(file.exists());
    }

    #[tokio::test]
    async fn clearing_one_entry_leaves_the_others_and_a_missing_one_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        record_handover_parked_session(dir.path(), &entry("/a.jsonl"))
            .await
            .unwrap();
        record_handover_parked_session(dir.path(), &entry("/b.jsonl"))
            .await
            .unwrap();
        clear_handover_parked_session(dir.path(), "/a.jsonl").await;
        clear_handover_parked_session(dir.path(), "/never.jsonl").await;
        let left: Vec<String> = read_handover_parked_sessions(dir.path())
            .await
            .into_iter()
            .map(|parked| parked.session_path)
            .collect();
        assert_eq!(left, vec!["/b.jsonl".to_owned()]);
    }

    #[tokio::test]
    async fn drops_an_entry_only_when_its_session_file_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.jsonl");
        std::fs::write(&live, "").unwrap();
        let live = live.to_string_lossy().into_owned();
        let gone = dir.path().join("gone.jsonl").to_string_lossy().into_owned();
        let store = FileHandoverParkedStore::new(dir.path());
        record_handover_parked_session(dir.path(), &entry(&live))
            .await
            .unwrap();
        record_handover_parked_session(dir.path(), &entry(&gone))
            .await
            .unwrap();
        store.drop_if_file_gone(&live).await;
        store.drop_if_file_gone(&gone).await;
        let left: Vec<String> = store
            .read()
            .await
            .into_iter()
            .map(|parked| parked.session_path)
            .collect();
        assert_eq!(left, vec![live]);
    }

    #[test]
    fn retry_fallback_shape_is_validated() {
        let ok = json!({"modelFallback":true,"fallbackChains":{"a":["b"]}});
        assert_eq!(retry_fallback_error(Some(&ok)), None);
        assert_eq!(retry_fallback_error(None), None);
        for bad in [
            json!(null),
            json!([]),
            json!({"modelFallback":"yes","fallbackChains":{}}),
            json!({"modelFallback":true,"fallbackChains":[]}),
            json!({"modelFallback":true,"fallbackChains":{"a":[" "]}}),
            json!({"modelFallback":true,"fallbackChains":{" ":["a"]}}),
        ] {
            assert!(retry_fallback_error(Some(&bad)).is_some(), "{bad}");
        }
        let too_many: serde_json::Map<String, Value> = (0..33)
            .map(|index| (format!("k{index}"), json!(["a"])))
            .collect();
        assert_eq!(
            retry_fallback_error(Some(
                &json!({"modelFallback":true,"fallbackChains":too_many})
            ))
            .as_deref(),
            Some("retryFallback allows at most 32 chains.")
        );
        let long_chain = vec!["a"; 33];
        assert_eq!(
            retry_fallback_error(Some(
                &json!({"modelFallback":true,"fallbackChains":{"k":long_chain}})
            ))
            .as_deref(),
            Some("retryFallback allows at most 32 entries per chain.")
        );
    }
}
