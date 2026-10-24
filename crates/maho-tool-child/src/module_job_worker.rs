//! The `module` job's worker (senpi `tool-child/module-job-worker.ts`; senpi#3566 todo 4): the launch barrier is
//! installed (by `gated_worker`) before the module runs, so every launch the module makes passes it.
//!
//! mhc has no module graph to `import()`: a "module" is a native export registered with [`register_child_module`] under
//! the file path an extension registered it with (`maho_ext_api::offload::ChildModuleRegistration`), which the `module`
//! job's containment check still validates as a real file inside a granted root. Running it is the same: the export is
//! called with `(args, host)`, its result (or typed failure) goes back to the job as `module_result` / `module_error`,
//! and every host call crosses to the job as `host_call` and comes back as `host_reply`.
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures::FutureExt;
use futures::future::BoxFuture;
use maho_ext_api::AbortSignal;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::entry_error::ToolChildJobError;
use crate::gated_worker::WorkerBarrier;
use crate::module_host::{ModuleHost, ModuleHostSend, create_module_host};
use crate::protocol::HostCallKind;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A loaded module export, called with `(args, host)`.
pub type ModuleExport =
    Arc<dyn Fn(Value, ModuleHost) -> BoxFuture<'static, Result<Value, BoxError>> + Send + Sync>;

/// `ModuleWorkerData`: what the job hands its worker.
#[derive(Debug, Clone)]
pub struct ModuleWorkerData {
    pub module_path: String,
    pub export_name: String,
    pub args: Value,
}

/// A failed host call's reply: its `code` and message.
pub type HostReply = Result<Value, (String, String)>;

/// What a module worker posts to its job (upstream's `postMessage` kinds).
pub enum ModuleWorkerEvent {
    /// `{ kind: "host_call", callId, hostKind, name, args }`; `reply` carries the `host_reply`.
    HostCall {
        call_id: String,
        host_kind: HostCallKind,
        name: Option<String>,
        args: Value,
        reply: oneshot::Sender<HostReply>,
    },
    /// `{ kind: "module_result", value }`.
    ModuleResult { value: Value },
    /// `{ kind: "module_error", code, message }`.
    ModuleError { code: String, message: String },
}

type Registry = HashMap<(String, String), ModuleExport>;

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn registry() -> MutexGuard<'static, Option<Registry>> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

fn real_path(path: &str) -> String {
    std::fs::canonicalize(Path::new(path)).map_or_else(
        |_| path.to_string(),
        |real| real.to_string_lossy().into_owned(),
    )
}

/// Registers the native export a file-backed child module resolves to. `module_path` is the path the registration
/// names (realpath'd here, as the job realpaths what it runs); a later registration of the same pair replaces it.
pub fn register_child_module(module_path: &str, export_name: &str, export: ModuleExport) {
    registry()
        .get_or_insert_with(HashMap::new)
        .insert((real_path(module_path), export_name.to_string()), export);
}

/// The export registered for `(module_path, export_name)`, if any.
pub fn load_module_export(module_path: &str, export_name: &str) -> Option<ModuleExport> {
    registry()
        .as_ref()?
        .get(&(real_path(module_path), export_name.to_string()))
        .cloned()
}

thread_local! {
    static BARRIER: RefCell<Option<WorkerBarrier>> = const { RefCell::new(None) };
}

/// The gated launch APIs of the module worker the calling code runs in (the module's only way to start a command that
/// passes the execution barrier); `None` outside a module worker.
pub fn current_barrier() -> Option<WorkerBarrier> {
    BARRIER.with(|barrier| barrier.borrow().clone())
}

fn module_error(events: &mpsc::UnboundedSender<ModuleWorkerEvent>, code: &str, message: String) {
    let _ = events.send(ModuleWorkerEvent::ModuleError {
        code: code.to_string(),
        message,
    });
}

/// `createWorkerHost`: every call is posted to the job and awaits its `host_reply`.
fn create_worker_host(
    events: mpsc::UnboundedSender<ModuleWorkerEvent>,
    cancellation: AbortSignal,
) -> ModuleHost {
    let next = Arc::new(AtomicU64::new(0));
    let send: ModuleHostSend = Arc::new(move |kind, name, args| {
        let call_id = format!("m{}", next.fetch_add(1, Ordering::SeqCst) + 1);
        let (reply, answered) = oneshot::channel();
        let posted = events.send(ModuleWorkerEvent::HostCall {
            call_id,
            host_kind: kind,
            name,
            args,
            reply,
        });
        async move {
            if posted.is_err() {
                return Err(BoxError::from(ToolChildJobError::new(
                    "host_call_job_ended",
                    "the job ended before this call ran",
                )));
            }
            match answered.await {
                Ok(Ok(value)) => Ok(value),
                Ok(Err((code, message))) => {
                    Err(BoxError::from(ToolChildJobError::new(code, message)))
                }
                Err(_) => Err(BoxError::from(ToolChildJobError::new(
                    "host_call_job_ended",
                    "the job ended",
                ))),
            }
        }
        .boxed()
    });
    create_module_host(cancellation, send)
}

/// The worker body: install nothing here (`start_gated_worker` already installed the barrier before this runs), find
/// the export, call it with `(args, host)`, and post exactly one `module_result` / `module_error`.
pub fn run_module_worker(
    data: ModuleWorkerData,
    barrier: WorkerBarrier,
    events: mpsc::UnboundedSender<ModuleWorkerEvent>,
    cancellation: AbortSignal,
) -> impl Future<Output = ()> {
    BARRIER.with(|slot| *slot.borrow_mut() = Some(barrier));
    async move {
        let Some(target) = load_module_export(&data.module_path, &data.export_name) else {
            module_error(
                &events,
                "offload_module_unavailable",
                format!(
                    "{} has no function export \"{}\"",
                    data.module_path, data.export_name
                ),
            );
            return;
        };
        let host = create_worker_host(events.clone(), cancellation);
        let call = AssertUnwindSafe(async move { target(data.args, host).await })
            .catch_unwind()
            .await;
        match call {
            Ok(Ok(value)) => {
                let _ = events.send(ModuleWorkerEvent::ModuleResult { value });
            }
            Ok(Err(error)) => module_error(&events, "module_job_failed", error.to_string()),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "the module panicked".to_string());
                module_error(&events, "module_job_failed", message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::barrier_clock::BarrierClock;
    use crate::gated_worker::{GatedWorkerData, install_worker_barrier};
    use crate::launch_env_names::ProcessEnv;
    use serde_json::json;

    fn barrier() -> WorkerBarrier {
        let data = GatedWorkerData {
            barrier_clock: BarrierClock::create(2).expect("clock"),
            job_id: "j".into(),
            reentry: None,
        };
        install_worker_barrier(&data, Box::new(|_| {}), &ProcessEnv::new())
    }

    fn data(path: &str, export: &str) -> ModuleWorkerData {
        ModuleWorkerData {
            module_path: path.to_string(),
            export_name: export.to_string(),
            args: json!({ "x": 1 }),
        }
    }

    async fn run(data: ModuleWorkerData) -> ModuleWorkerEvent {
        let (tx, mut rx) = mpsc::unbounded_channel();
        run_module_worker(data, barrier(), tx, AbortSignal::default()).await;
        rx.recv().await.expect("an event")
    }

    #[tokio::test]
    async fn an_unregistered_export_is_offload_module_unavailable() {
        match run(data("/nowhere/mod.rs", "missing")).await {
            ModuleWorkerEvent::ModuleError { code, message } => {
                assert_eq!(code, "offload_module_unavailable");
                assert_eq!(
                    message,
                    "/nowhere/mod.rs has no function export \"missing\""
                );
            }
            _ => panic!("expected module_error"),
        }
    }

    #[tokio::test]
    async fn a_registered_export_is_called_with_its_args_and_returns_its_value() {
        let export: ModuleExport =
            Arc::new(|args, _host| async move { Ok(json!({ "got": args })) }.boxed());
        register_child_module("/virtual/worker-ok.rs", "echo", export);
        match run(data("/virtual/worker-ok.rs", "echo")).await {
            ModuleWorkerEvent::ModuleResult { value } => {
                assert_eq!(value, json!({ "got": { "x": 1 } }))
            }
            _ => panic!("expected module_result"),
        }
    }

    #[tokio::test]
    async fn a_failing_or_panicking_export_is_module_job_failed() {
        let failing: ModuleExport = Arc::new(|_, _| async { Err(BoxError::from("boom")) }.boxed());
        register_child_module("/virtual/worker-bad.rs", "fail", failing);
        let panicking: ModuleExport = Arc::new(|_, _| async { panic!("kaboom") }.boxed());
        register_child_module("/virtual/worker-bad.rs", "panic", panicking);
        for (export, text) in [("fail", "boom"), ("panic", "kaboom")] {
            match run(data("/virtual/worker-bad.rs", export)).await {
                ModuleWorkerEvent::ModuleError { code, message } => {
                    assert_eq!(
                        (code.as_str(), message.as_str()),
                        ("module_job_failed", text)
                    );
                }
                _ => panic!("expected module_error"),
            }
        }
    }

    #[tokio::test]
    async fn a_host_call_is_posted_and_its_reply_reaches_the_module() {
        let export: ModuleExport =
            Arc::new(|_, host| async move { host.call("count", json!({ "i": 1 })).await }.boxed());
        register_child_module("/virtual/worker-host.rs", "ask", export);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn(run_module_worker(
            data("/virtual/worker-host.rs", "ask"),
            barrier(),
            tx,
            AbortSignal::default(),
        ));
        let Some(ModuleWorkerEvent::HostCall {
            call_id,
            host_kind,
            name,
            args,
            reply,
        }) = rx.recv().await
        else {
            panic!("expected host_call");
        };
        assert_eq!(
            (call_id.as_str(), host_kind, name.as_deref()),
            ("m1", HostCallKind::Tool, Some("count"))
        );
        assert_eq!(args, json!({ "i": 1 }));
        let _ = reply.send(Ok(json!(7)));
        match rx.recv().await {
            Some(ModuleWorkerEvent::ModuleResult { value }) => assert_eq!(value, json!(7)),
            _ => panic!("expected module_result"),
        }
        worker.await.expect("worker task");
    }
}
