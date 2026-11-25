//! Port of senpi `packages/coding-agent/src/modes/rpc/session-worker-start.ts`.
//!
//! Spawns a session worker isolate with issued per-worker data (`session_worker_data`), host
//! process start identity, and optional webview broker metadata, disposing issued grants if spawn
//! fails.

use crate::process_start_identity::read_process_start_ticks;
use crate::session_worker_data::{IssuedSessionWorkerData, session_worker_data_registry};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Configuration for spawning a session worker process.
#[derive(Debug, Clone)]
pub struct SessionWorkerSpawnSpec {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub worker_data: Value,
    pub env: HashMap<String, String>,
}

pub struct StartedSessionWorker<W> {
    pub worker: W,
    pub issued: Arc<IssuedSessionWorkerData>,
}

/// Builds the workerData payload matching upstream `startSessionWorker`.
pub fn build_worker_data_payload(
    issued: &IssuedSessionWorkerData,
    webview_broker_port: Option<Value>,
) -> Value {
    let host_started_at = read_process_start_ticks(std::process::id());
    let mut obj = json!({
        "hostProcessStartedAtMs": host_started_at,
        "extensionData": issued.extension_data,
    });
    if let Some(port) = webview_broker_port {
        obj["webviewBroker"] = port;
    }
    obj
}

/// Issues per-worker data from [`session_worker_data_registry`] and invokes `spawn` with the built
/// worker-data payload; disposes the issued grants immediately if `spawn` fails.
pub fn start_session_worker_with<W, F>(
    webview_broker_port: Option<Value>,
    spawn: F,
) -> Result<StartedSessionWorker<W>, String>
where
    F: FnOnce(Value, &IssuedSessionWorkerData) -> Result<W, String>,
{
    let (worker, issued) = session_worker_data_registry().spawn(|issued| {
        let payload = build_worker_data_payload(issued, webview_broker_port);
        spawn(payload, issued)
    })?;
    Ok(StartedSessionWorker { worker, issued })
}

/// Default process-spawning helper for starting a native session worker subprocess.
pub fn start_session_worker(
    spec_builder: impl FnOnce(Value) -> Result<SessionWorkerSpawnSpec, String>,
    webview_broker_port: Option<Value>,
) -> Result<StartedSessionWorker<tokio::process::Child>, String> {
    start_session_worker_with(webview_broker_port, |payload, _issued| {
        let spec = spec_builder(payload)?;
        let mut cmd = tokio::process::Command::new(&spec.executable);
        cmd.args(&spec.args)
            .envs(&spec.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit());
        cmd.spawn().map_err(|err| err.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_worker_data::{SessionWorkerDataGrant, SessionWorkerDataRegistry};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn start_session_worker_disposes_issued_grants_on_failure() {
        let reg = SessionWorkerDataRegistry::default();
        let disposed = Arc::new(AtomicUsize::new(0));
        let d1 = disposed.clone();
        reg.provide(
            "ext",
            Arc::new(move || {
                let d1 = d1.clone();
                Ok(SessionWorkerDataGrant {
                    value: json!("v"),
                    dispose: Some(Box::new(move || {
                        d1.fetch_add(1, Ordering::SeqCst);
                    })),
                })
            }),
        )
        .expect("provide");

        let res = reg.spawn::<(), _>(|issued| {
            let payload = build_worker_data_payload(issued, Some(json!(99)));
            assert_eq!(payload["extensionData"]["ext"], "v");
            assert_eq!(payload["webviewBroker"], 99);
            Err("exec_failed".into())
        });
        assert!(res.is_err());
        assert_eq!(disposed.load(Ordering::SeqCst), 1);
    }
}
