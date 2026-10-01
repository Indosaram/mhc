use serde_json::{Value, json};
use thiserror::Error;
use std::{collections::BTreeMap, path::Path, sync::Arc};
use tokio::{io::AsyncWriteExt, sync::Mutex};

#[derive(Debug, Error)]
#[error("{0}")]
pub struct ThreadMetadataUpdateError(pub String);

#[derive(Debug, Error)]
pub enum MetadataStateError {
    #[error(transparent)]
    Update(#[from] ThreadMetadataUpdateError),
    #[error("{message}: {path}")]
    State { path: String, message: String },
}
#[derive(Default)]
pub struct ThreadMetadataState {
    mutations: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
}
impl ThreadMetadataState {
    pub async fn read_git_info(&self, session_path: &Path) -> Result<Option<Value>, MetadataStateError> {
        let path = format!("{}.metadata.json", session_path.display());
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(MetadataStateError::State { path, message: format!("Failed to read metadata sidecar ({error})") }),
        };
        let record: Value = serde_json::from_str(&content).map_err(|error| MetadataStateError::State { path: path.clone(), message: format!("Invalid metadata sidecar JSON ({error})") })?;
        let object = record.get("gitInfo").and_then(Value::as_object).ok_or_else(|| MetadataStateError::State { path: path.clone(), message: "Invalid metadata sidecar record".into() })?;
        let mut info = json!({"sha":null,"branch":null,"originUrl":null});
        for key in ["sha", "branch", "originUrl"] {
            match object.get(key) {
                None | Some(Value::Null) => {},
                Some(Value::String(value)) if !value.trim().is_empty() => info[key] = json!(value.trim()),
                _ => return Err(MetadataStateError::State { path, message: format!("Invalid metadata gitInfo.{key}") }),
            }
        }
        Ok(Some(info))
    }
    pub async fn update_git_info(&self, thread_id: &str, session_path: &Path, update: &Value) -> Result<Value, MetadataStateError> {
        let mutation = self.mutations.lock().await.entry(thread_id.into()).or_default().clone();
        let _guard = mutation.lock().await;
        let current = self.read_git_info(session_path).await?;
        let next = merge_git_info(current.as_ref(), update)?;
        let fields = next.as_object().into_iter().flatten().filter(|(_, value)| !value.is_null()).map(|(key, value)| (key.clone(), value.clone())).collect::<serde_json::Map<_, _>>();
        let path = format!("{}.metadata.json", session_path.display());
        write_sidecar_file_atomic(Path::new(&path), &format!("{}\n", json!({"gitInfo":fields}))).await?;
        Ok(next)
    }
}
pub async fn write_sidecar_file_atomic(path: &Path, contents: &str) -> Result<(), MetadataStateError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(".{}-{}.tmp", path.file_name().unwrap_or_default().to_string_lossy(), uuid::Uuid::new_v4()));
    let result = async {
        tokio::fs::create_dir_all(parent).await?;
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).await?;
        file.write_all(contents.as_bytes()).await?;
        drop(file);
        tokio::fs::rename(&temporary, path).await
    }.await;
    if let Err(error) = result {
        let message = match tokio::fs::remove_file(&temporary).await {
            Ok(()) => error.to_string(),
            Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => error.to_string(),
            Err(cleanup) => format!("sidecar write failed ({error}) and its temporary file could not be removed ({cleanup})"),
        };
        return Err(MetadataStateError::State { path: path.display().to_string(), message });
    }
    Ok(())
}

pub fn parse_git_info_update(value: &Value) -> Result<Value, ThreadMetadataUpdateError> {
    let object = value.as_object().ok_or_else(|| ThreadMetadataUpdateError("gitInfo must include at least one field".into()))?;
    let mut update = serde_json::Map::new();
    for key in ["sha", "branch", "originUrl"] {
        if let Some(value) = object.get(key) {
            let parsed = match value {
                Value::Null => Value::Null,
                Value::String(value) if !value.trim().is_empty() => Value::String(value.trim().into()),
                _ => return Err(ThreadMetadataUpdateError(format!("gitInfo.{key} must not be empty"))),
            };
            update.insert(key.into(), parsed);
        }
    }
    if update.is_empty() { return Err(ThreadMetadataUpdateError("gitInfo must include at least one field".into())); }
    Ok(Value::Object(update))
}
pub fn merge_git_info(current: Option<&Value>, update: &Value) -> Result<Value, ThreadMetadataUpdateError> {
    let update = parse_git_info_update(update)?;
    let mut next = json!({"sha":null,"branch":null,"originUrl":null});
    for key in ["sha", "branch", "originUrl"] {
        if let Some(value) = update.get(key).or_else(|| current.and_then(|current| current.get(key))) { next[key] = value.clone(); }
    }
    Ok(next)
}
