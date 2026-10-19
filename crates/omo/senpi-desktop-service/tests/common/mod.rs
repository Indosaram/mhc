//! Deterministic test fixtures and fake engine spawner with event barriers.
//! Matches `test/harness.ts` and `test/fake-engine.mjs`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};
use tokio::time::{timeout, Duration};

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
    pub kill_count: Arc<AtomicUsize>,
    notify: Arc<Notify>,
}

impl SpawnLog {
    pub fn new(env_overrides: HashMap<String, String>) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let kill_count = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(Notify::new());
        let child_counter = Arc::new(AtomicUsize::new(0));

        let kills_clone = Arc::clone(&kill_count);

        let factory: ChildFactory = Arc::new(move || {
            let kills = Arc::clone(&kills_clone);
            let envs = env_overrides.clone();
            let child_idx = child_counter.fetch_add(1, Ordering::SeqCst);

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

                let on_kill = Arc::new(move || {
                    kills.fetch_add(1, Ordering::SeqCst);
                });

                let engine_child = EngineChild::new_with_hook(
                    format!("fake-engine-{child_idx}"),
                    child,
                    Some(on_kill),
                );
                Ok(engine_child)
            })
        });

        Self {
            factory,
            requests,
            kill_count,
            notify,
        }
    }

    pub fn notification_tracer(&self, _child_idx: usize) -> senpi_desktop_service::service::NotificationObserver {
        let reqs = Arc::clone(&self.requests);
        let notify = Arc::clone(&self.notify);

        Arc::new(move |method: &str, params: &Value| {
            if method == "engine.log"
                && let Some(msg) = params.get("message").and_then(Value::as_str)
                && let Some(rest) = msg.strip_prefix("recv ")
                && let Ok(req_val) = serde_json::from_str::<Value>(rest)
                && let Some(req_method) = req_val.get("method").and_then(Value::as_str) {
                                let wire = WireRequest {
                                    id: req_val.get("id").and_then(Value::as_u64),
                                    method: req_method.to_string(),
                                    params: req_val.get("params").cloned(),
                                };
                                let reqs_inner = Arc::clone(&reqs);
                                let notif_inner = Arc::clone(&notify);
                                tokio::spawn(async move {
                                    reqs_inner.lock().await.push(wire);
                                    notif_inner.notify_waiters();
                                });
            }
        })
    }

    pub fn kills(&self) -> usize {
        self.kill_count.load(Ordering::SeqCst)
    }

    pub async fn nth_request(&self, method: &str, n: usize) -> WireRequest {
        let guard_dur = Duration::from_secs(15);
        let start = tokio::time::Instant::now();

        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            {
                let reqs = self.requests.lock().await;
                let count = reqs.iter().filter(|r| r.method == method).count();
                if count >= n {
                    let matching: Vec<_> = reqs
                        .iter()
                        .filter(|r| r.method == method)
                        .cloned()
                        .collect();
                    return matching[n - 1].clone();
                }
            }

            let elapsed = start.elapsed();
            if elapsed >= guard_dur {
                panic!("timed out waiting for {}-th request of method '{}'", n, method);
            }
            let remaining = guard_dur - elapsed;

            if timeout(remaining, notified).await.is_err() {
                panic!("timed out waiting for {}-th request of method '{}'", n, method);
            }
        }
    }
}
