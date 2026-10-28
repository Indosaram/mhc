//! Extra worker tools (spec §6): a worker process calls a tool of the parent's live runtime
//! through request/response files under `<state dir>/workpools/calls/`.
//!
//! The worker writes `<call id>.req.json` (`{pool_id, task_id, tool, params}`); the parent's
//! [`Bridge`] executes the call when the tool is granted to that pool and writes
//! `<call id>.res.json` (`{ok, text, is_error}`). Both sides use temp-file + rename, so a reader
//! never sees a partial file. The parent checks the directory on a short interval only while it
//! owns pools that grant extra tools.
use crate::model::{PoolStatus, codes};
use crate::store::PoolStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often the parent looks for calls, and a worker for its answer.
pub const CALL_CHECK: Duration = Duration::from_millis(100);
/// How long a worker waits for the parent to answer a call.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallRequest {
    pub pool_id: String,
    pub task_id: String,
    pub tool: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallResponse {
    pub text: String,
    pub is_error: bool,
}

pub fn calls_dir(store: &PoolStore) -> PathBuf {
    store.dir().join("calls")
}

fn write_atomic(dir: &Path, name: &str, value: &impl Serialize) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    let written = std::fs::File::create(&tmp).and_then(|mut file| {
        file.write_all(&bytes)?;
        file.sync_all()
    });
    written.and_then(|()| std::fs::rename(&tmp, dir.join(name))).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Worker side: sends one call and waits for the answer.
pub async fn call(store: &PoolStore, request: &CallRequest) -> CallResponse {
    let dir = calls_dir(store);
    let id = uuid::Uuid::new_v4().simple().to_string();
    if let Err(error) = write_atomic(&dir, &format!("{id}.req.json"), request) {
        return CallResponse { text: format!("{}: cannot reach the parent session: {error}", codes::TOOLS_UNAVAILABLE), is_error: true };
    }
    let response_path = dir.join(format!("{id}.res.json"));
    let deadline = tokio::time::Instant::now() + CALL_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let Ok(bytes) = std::fs::read(&response_path) {
            let _ = std::fs::remove_file(&response_path);
            return serde_json::from_slice(&bytes).unwrap_or_else(|error| CallResponse {
                text: format!("{}: unreadable answer from the parent session: {error}", codes::TOOLS_UNAVAILABLE),
                is_error: true,
            });
        }
        tokio::time::sleep(CALL_CHECK).await;
    }
    let _ = std::fs::remove_file(dir.join(format!("{id}.req.json")));
    CallResponse { text: format!("{}: the parent session did not answer", codes::TOOLS_UNAVAILABLE), is_error: true }
}

/// Executes a granted tool in the parent runtime.
pub type Executor = std::sync::Arc<dyn Fn(String, Value) -> crate::host::BoxFuture<CallResponse> + Send + Sync>;

/// Parent side: answers the pending calls once.
pub async fn serve_once(store: &PoolStore, execute: &Executor) {
    let dir = calls_dir(store);
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let pending: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".req.json") && !n.starts_with('.')))
        .collect();
    for path in pending {
        let Some(id) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".req.json")).map(str::to_owned) else { continue };
        let request: Option<CallRequest> = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let _ = std::fs::remove_file(&path);
        let response = match request {
            None => CallResponse { text: format!("{}: malformed tool call", codes::INVALID_INPUT), is_error: true },
            Some(request) => match store.load(&request.pool_id) {
                Ok(Some(record))
                    if record.status != PoolStatus::Cancelled
                        && record.tools.contains(&request.tool)
                        && record.worker(&request.task_id).is_some() =>
                {
                    execute(request.tool, request.params).await
                }
                Ok(_) => CallResponse { text: format!("{}: this tool is not granted to this worker", codes::POLICY_DENIED), is_error: true },
                Err(error) => CallResponse { text: error.to_string(), is_error: true },
            },
        };
        if let Err(error) = write_atomic(&dir, &format!("{id}.res.json"), &response) {
            maho_ext_api::diagnostic(&format!("workpool: cannot answer a worker tool call: {error}"));
        }
    }
}

/// Whether any pool in `store` grants extra tools and is not cancelled.
pub fn has_granting_pools(store: &PoolStore) -> bool {
    store
        .load_all()
        .into_iter()
        .any(|(_, loaded)| loaded.is_ok_and(|record| record.status != PoolStatus::Cancelled && !record.tools.is_empty()))
}
