use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};
use tokio::time::Duration;

use senpi_desktop_service::child::{ChildFactory, EngineChild};
use senpi_desktop_service::rpc_client::DesktopServiceError;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct WireRequest {
    pub id: Option<u64>,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Clone)]
pub struct SpawnLog {
    pub factory: ChildFactory,
    pub requests: Arc<Mutex<Vec<WireRequest>>>,
    notify: Arc<Notify>,
}

impl SpawnLog {
    pub fn new(env_overrides: HashMap<String, String>) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let notify = Arc::new(Notify::new());
        let child_counter = Arc::new(AtomicUsize::new(0));

        let factory: ChildFactory = Arc::new(move || {
            let envs = env_overrides.clone();
            let child_idx = child_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            Box::pin(async move {
                let script_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/fake_engine.py");
                let python_cmd = if cfg!(windows) { "python" } else { "python3" };
                let mut cmd = Command::new(python_cmd);
                cmd.arg(&script_path).arg("--stdio");
                for (k, v) in &envs {
                    cmd.env(k, v);
                }
                cmd.stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());

                let child = cmd.spawn().map_err(|e| {
                    DesktopServiceError::closed(format!("failed to spawn fake child: {e}"))
                })?;

                let engine_child = EngineChild::new(
                    format!("fake-engine-{child_idx}"),
                    child,
                );
                Ok(engine_child)
            })
        });

        Self {
            factory,
            requests,
            notify,
        }
    }

    pub fn notification_tracer(
        &self,
        _instance_idx: usize,
    ) -> senpi_desktop_service::service::NotificationObserver {
        let requests = Arc::clone(&self.requests);
        let notify = Arc::clone(&self.notify);
        Arc::new(move |method: &str, params: &Value| {
            if method == "engine.log"
                && let Some(text) = params.get("message").and_then(Value::as_str)
                && let Some(json_str) = text.strip_prefix("recv ")
                && let Ok(parsed) = serde_json::from_str::<Value>(json_str)
                && let Some(req_method) = parsed.get("method").and_then(Value::as_str) {
                let wire = WireRequest {
                    id: parsed.get("id").and_then(Value::as_u64),
                    method: req_method.to_string(),
                    params: parsed.get("params").cloned(),
                };
                let requests = requests.clone();
                let notify = notify.clone();
                tokio::spawn(async move {
                    requests.lock().await.push(wire);
                    notify.notify_waiters();
                });
            }
        })
    }

    pub async fn nth_request(&self, method: &str, n: usize) -> WireRequest {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let reqs = self.requests.lock().await;
                let matching: Vec<_> = reqs.iter().filter(|r| r.method == method).cloned().collect();
                if matching.len() >= n {
                    return matching[n - 1].clone();
                }
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("timed out waiting for {n}th request to method {method}");
            }
            tokio::time::timeout_at(deadline, notified).await.expect("request notification deadline");
        }
    }
}
