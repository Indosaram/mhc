use std::path::Path;

use crate::backend::{BackendKind, IsolationError, Result, BACKEND_FILE};
use crate::util::now_iso8601;

pub fn mark_started(base: &Path, backend: BackendKind, extra: &[(&str, String)]) -> Result<()> {
    let path = base.join(BACKEND_FILE);
    let mut object: serde_json::Map<String, serde_json::Value> = match std::fs::read_to_string(&path)
    {
        Ok(text) => serde_json::from_str(&text).map_err(|error| IsolationError::other(error.to_string()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err(error.into()),
    };
    object.insert(
        "backend".to_string(),
        serde_json::Value::String(backend.as_str().to_string()),
    );
    object.insert(
        "started_at".to_string(),
        serde_json::Value::String(now_iso8601()),
    );
    for (key, value) in extra {
        object.insert((*key).to_string(), serde_json::Value::String(value.clone()));
    }
    let text = serde_json::to_string(&serde_json::Value::Object(object))
        .map_err(|error| IsolationError::other(error.to_string()))?;
    std::fs::write(&path, text)?;
    Ok(())
}
