//! Test fixtures and harness helpers matching upstream `test/fixtures.ts`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};

use maho_ai::model::Model;
use maho_ai::types::ThinkingLevel;
use maho_tools::definition::ToolSessionManager;
use maho_tools::ToolContext;
use senpi_desktop_service::child::{ChildFactory, EngineChild};
use senpi_desktop_service::rpc_client::DesktopServiceError;
use senpi_desktop_service::service::{DesktopService, DesktopServiceOptions};
use serde_json::Value;

use senpi_desktop_tool::activation::ComputerHandle;
use senpi_desktop_tool::session::{ComputerHostContext, ComputerModel};
use senpi_desktop_tool::settings::{resolve_computer_settings, ComputerSettingsInput};

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
    pub children_count: Arc<AtomicUsize>,
    notify: Arc<Notify>,
}

impl SpawnLog {
    pub fn new(env_overrides: HashMap<String, String>) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let kill_count = Arc::new(AtomicUsize::new(0));
        let children_count = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(Notify::new());
        let child_counter = Arc::new(AtomicUsize::new(0));

        let kills_clone = Arc::clone(&kill_count);
        let children_counter = Arc::clone(&children_count);

        let factory: ChildFactory = Arc::new(move || {
            let kills = Arc::clone(&kills_clone);
            let envs = env_overrides.clone();
            let child_idx = child_counter.fetch_add(1, Ordering::SeqCst);
            children_counter.fetch_add(1, Ordering::SeqCst);

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
            children_count,
            notify,
        }
    }

    pub fn notification_tracer(&self) -> senpi_desktop_service::service::NotificationObserver {
        let reqs = Arc::clone(&self.requests);
        let notify = Arc::clone(&self.notify);

        Arc::new(move |method: &str, params: &Value| {
            if method == "engine.log"
                && let Some(msg) = params.get("message").and_then(Value::as_str)
                && let Some(rest) = msg.strip_prefix("recv ")
                && let Ok(val) = serde_json::from_str::<Value>(rest)
                && let Some(m) = val.get("method").and_then(Value::as_str) {
                let req = WireRequest { id: val.get("id").and_then(Value::as_u64), method: m.to_string(), params: val.get("params").cloned() };
                let reqs = reqs.clone();
                let notify = notify.clone();
                tokio::spawn(async move { reqs.lock().await.push(req); notify.notify_waiters(); });
            }
        })
    }
}

pub struct TestToolSessionManager {
    pub session_id: String,
    pub session_file: Option<PathBuf>,
}

impl ToolSessionManager for TestToolSessionManager {
    fn session_id(&self) -> &str {
        &self.session_id
    }
    fn session_file(&self) -> Option<&Path> {
        self.session_file.as_deref()
    }
}

pub struct TestToolContext {
    pub cwd: PathBuf,
    pub session_manager: TestToolSessionManager,
    pub model: Option<Model>,
}

impl TestToolContext {
    pub fn new(
        cwd: impl Into<PathBuf>,
        model: Option<Model>,
        session_id: impl Into<String>,
        session_dir: impl Into<PathBuf>,
    ) -> Self {
        let dir = session_dir.into();
        Self {
            cwd: cwd.into(),
            session_manager: TestToolSessionManager {
                session_id: session_id.into(),
                session_file: Some(dir.join("session.json")),
            },
            model,
        }
    }
}

impl ToolContext for TestToolContext {
    fn cwd(&self) -> &Path {
        &self.cwd
    }
    fn model(&self) -> Option<&Model> {
        self.model.as_ref()
    }
    fn thinking_level(&self) -> Option<ThinkingLevel> {
        None
    }
    fn session_manager(&self) -> &dyn ToolSessionManager {
        &self.session_manager
    }
    fn goal_store_file(&self) -> Option<&Path> {
        None
    }
}

pub fn host_context(model: Option<ComputerModel>) -> ComputerHostContext {
    ComputerHostContext::new("/work", model, "session-1", "/sessions/project")
}

pub struct DesktopFixture {
    pub handle: Arc<ComputerHandle>,
    pub service: Arc<DesktopService>,
    pub log: SpawnLog,
}

pub fn desktop_fixture(
    settings: Option<ComputerSettingsInput>,
    env: HashMap<String, String>,
) -> DesktopFixture {
    let mut env_map = env;
    env_map.insert("FAKE_ENGINE_DESKTOP".into(), "1".into());

    let log = SpawnLog::new(env_map);
    let tracer = log.notification_tracer();

    let service = Arc::new(DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(tracer),
    }));

    let raw_val = settings.map(|s| serde_json::to_value(s).unwrap_or(Value::Null));
    let resolved = resolve_computer_settings(raw_val.as_ref(), "linux").expect("settings");

    let svc_clone = service.clone();
    let handle = Arc::new(ComputerHandle::new(svc_clone, move || resolved.clone()));

    DesktopFixture {
        handle,
        service,
        log,
    }
}

pub async fn methods_of(log: &SpawnLog) -> Vec<String> {
    log.requests.lock().await.iter().map(|r| r.method.clone()).collect()
}
