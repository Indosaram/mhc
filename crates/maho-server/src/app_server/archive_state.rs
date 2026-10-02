use super::metadata_state::{MetadataStateError, write_sidecar_file_atomic};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::Arc};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum ArchiveStateError {
    #[error("{message}: {path}")]
    State { path: String, message: String },
    #[error(transparent)]
    Metadata(#[from] MetadataStateError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub struct ThreadArchiveState {
    session_dir: Option<PathBuf>,
    mutations: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
}
fn sidecar(thread: &Value) -> Result<PathBuf, ArchiveStateError> {
    thread["sessionPath"].as_str().filter(|path| !path.is_empty()).map(|path| PathBuf::from(format!("{path}.archived"))).ok_or_else(|| ArchiveStateError::State { path:thread["id"].as_str().unwrap_or_default().into(), message:format!("Thread {} has no session path for archive state",thread["id"].as_str().unwrap_or_default()) })
}
fn read_error(path: &Path, error: impl std::fmt::Display) -> ArchiveStateError {
    ArchiveStateError::State { path:path.to_string_lossy().into_owned(), message:format!("Failed to read archived thread state ({error})") }
}
fn parse_thread(path: &Path, contents: &str) -> Result<Value, ArchiveStateError> {
    let record: Value = serde_json::from_str(contents).map_err(|error| ArchiveStateError::State { path:path.to_string_lossy().into_owned(), message:format!("Invalid archived thread sidecar JSON ({error})") })?;
    let thread = &record["thread"];
    if !record.is_object() || !thread.is_object() { return Err(ArchiveStateError::State { path:path.to_string_lossy().into_owned(), message:"Invalid archived thread sidecar record".into() }); }
    let mut wire = json!({});
    for field in ["id","sessionId","cwd","createdAt","updatedAt","sessionPath"] {
        let Some(value) = thread[field].as_str().filter(|value| !value.is_empty()) else { return Err(ArchiveStateError::State { path:path.to_string_lossy().into_owned(), message:"Invalid archived thread record".into() }); };
        wire[field] = json!(value);
    }
    let Some(status) = thread["status"]["type"].as_str().filter(|status| matches!(*status,"idle"|"active"|"notLoaded")) else { return Err(ArchiveStateError::State { path:path.to_string_lossy().into_owned(), message:"Invalid archived thread record".into() }); };
    wire["status"] = json!({"type":status});
    for field in ["preview","name"] { wire[field] = thread[field].as_str().map_or(Value::Null, |value| json!(value)); }
    Ok(wire)
}
impl ThreadArchiveState {
    pub fn new(session_dir: Option<PathBuf>) -> Self { Self { session_dir, mutations:Default::default() } }
    pub async fn mark_archived(&self, thread: &Value, archived_at: &str) -> Result<(), ArchiveStateError> {
        let mutation = self.mutations.lock().await.entry(thread["id"].as_str().unwrap_or_default().into()).or_default().clone();
        let _guard = mutation.lock().await;
        write_sidecar_file_atomic(&sidecar(thread)?, &format!("{}\n",json!({"archivedAt":archived_at,"thread":thread}))).await?;
        Ok(())
    }
    pub async fn is_archived(&self, thread: &Value) -> Result<bool, ArchiveStateError> {
        let path = sidecar(thread)?;
        match tokio::fs::read_to_string(&path).await {
            Ok(_) => Ok(true), Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false), Err(error) => Err(read_error(&path,error)),
        }
    }
    pub async fn list_archived_threads(&self) -> Result<Vec<Value>, ArchiveStateError> {
        let Some(directory) = &self.session_dir else { return Ok(Vec::new()); };
        let mut directory = match tokio::fs::read_dir(directory).await {
            Ok(directory) => directory, Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()), Err(error) => return Err(read_error(directory,error)),
        };
        let mut threads = Vec::new();
        while let Some(entry) = directory.next_entry().await? {
            if !entry.file_name().to_string_lossy().ends_with(".archived") { continue; }
            let path = entry.path();
            match tokio::fs::read_to_string(&path).await {
                Ok(contents) => threads.push(parse_thread(&path,&contents)?),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(read_error(&path,error)),
            }
        }
        Ok(threads)
    }
    pub async fn clear_archived(&self, id: &str) -> Result<(), ArchiveStateError> {
        let mutation = self.mutations.lock().await.entry(id.into()).or_default().clone();
        let _guard = mutation.lock().await;
        for thread in self.list_archived_threads().await?.iter().filter(|thread| thread["id"] == id) {
            match tokio::fs::remove_file(sidecar(thread)?).await { Ok(()) => {}, Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}, Err(error) => return Err(error.into()) }
        }
        Ok(())
    }
    pub async fn unarchive(&self, id: &str, now_ms: i64) -> Result<Option<Value>, ArchiveStateError> {
        let mutation = self.mutations.lock().await.entry(id.into()).or_default().clone();
        let _guard = mutation.lock().await;
        let Some(mut thread) = self.list_archived_threads().await?.into_iter().find(|thread| thread["id"] == id) else { return Ok(None); };
        let path = thread["sessionPath"].as_str().unwrap_or_default().to_owned();
        let metadata = tokio::fs::metadata(&path).await?;
        let modified = metadata.modified()?.duration_since(std::time::UNIX_EPOCH).map_err(std::io::Error::other)?.as_millis();
        let modified = i64::try_from(modified).map_err(std::io::Error::other)?;
        let previous = chrono::DateTime::parse_from_rfc3339(thread["updatedAt"].as_str().unwrap_or_default()).map(|time| time.timestamp_millis()).unwrap_or(-1);
        let next = now_ms.max(modified + 1).max(previous + 1);
        let time = chrono::DateTime::from_timestamp_millis(next).ok_or_else(|| std::io::Error::other("Invalid unarchive timestamp"))?;
        let accessed = metadata.accessed()?;
        tokio::task::spawn_blocking(move || std::fs::File::options().write(true).open(path)?.set_times(std::fs::FileTimes::new().set_accessed(accessed).set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_millis(u64::try_from(next).map_err(std::io::Error::other)?)))).await.map_err(std::io::Error::other)??;
        match tokio::fs::remove_file(sidecar(&thread)?).await { Ok(()) => {}, Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}, Err(error) => return Err(error.into()) }
        thread["updatedAt"] = json!(time.to_rfc3339_opts(chrono::SecondsFormat::Millis,true));
        Ok(Some(thread))
    }
}
