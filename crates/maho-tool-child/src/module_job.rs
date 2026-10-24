//! The `module` job kind (senpi `tool-child/module-job.ts`; senpi#3566, plan todo 4; the contract todo 8's
//! `ctx.offload` reuses unchanged).
//!
//! Wire shape: `{ modulePath, export, args }`. The child runs the native export registered for `modulePath` in a gated
//! job worker (see `module_job_worker`) and calls it with `args`; the result must be JSON. `modulePath` must resolve -
//! after realpath - inside one of the installed extension directories the server granted this child at spawn
//! (`SENPI_TOOL_CHILD_MODULE_ROOTS`), so a job can never name an arbitrary file.
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::FutureExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use maho_ext_api::AbortSignal;

use crate::entry::{
    HandlerError, HostCallFn, HostCallOptions, ToolChildJobContext, ToolChildJobHandler,
};
pub use crate::entry_error::ToolChildJobError;
use crate::gated_worker::start_gated_worker;
use crate::host_calls_child::HostCallTarget;
use crate::launch_env_names::ProcessEnv;
use crate::module_job_worker::{HostReply, ModuleWorkerData, ModuleWorkerEvent, run_module_worker};
use crate::protocol::HostCallKind;

pub const MODULE_JOB_KIND: &str = "module";
pub const TOOL_CHILD_MODULE_ROOTS_ENV: &str = "SENPI_TOOL_CHILD_MODULE_ROOTS";

/// How long a cancelled module body may take to settle on its own before its worker is ended.
pub const MODULE_CANCEL_GRACE_MS: u64 = 2_000;

/// `ModuleJobArgs`.
#[derive(Debug, Clone, PartialEq)]
pub struct ModuleJobArgs {
    pub module_path: String,
    pub export: String,
    pub args: Value,
}

pub fn parse_module_job_args(value: &Value) -> Result<ModuleJobArgs, ToolChildJobError> {
    let Some(object) = value.as_object() else {
        return Err(ToolChildJobError::new(
            "module_job_invalid",
            "args must be an object",
        ));
    };
    let module_path = match object.get("modulePath").and_then(Value::as_str) {
        Some(path) if Path::new(path).is_absolute() => path.to_string(),
        _ => {
            return Err(ToolChildJobError::new(
                "module_job_invalid",
                "modulePath must be an absolute path",
            ));
        }
    };
    let export = match object.get("export").and_then(Value::as_str) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => {
            return Err(ToolChildJobError::new(
                "module_job_invalid",
                "export must be a non-empty string",
            ));
        }
    };
    Ok(ModuleJobArgs {
        module_path,
        export,
        args: object.get("args").cloned().unwrap_or(Value::Null),
    })
}

pub fn parse_module_roots(env: &ProcessEnv) -> Result<Vec<String>, ToolChildJobError> {
    let raw = match env.get(TOOL_CHILD_MODULE_ROOTS_ENV) {
        None => return Ok(Vec::new()),
        Some(raw) if raw.is_empty() => return Ok(Vec::new()),
        Some(raw) => raw,
    };
    // A malformed document is a plain failure upstream (`JSON.parse` throws), not a typed job error.
    let parsed: Value = serde_json::from_str(raw)
        .map_err(|error| ToolChildJobError::new("tool_child_job_failed", error.to_string()))?;
    let invalid = || {
        ToolChildJobError::new(
            "module_job_invalid",
            format!("{TOOL_CHILD_MODULE_ROOTS_ENV} must be a JSON array of absolute paths"),
        )
    };
    let Some(items) = parsed.as_array() else {
        return Err(invalid());
    };
    items
        .iter()
        .map(|item| match item.as_str() {
            Some(root) if Path::new(root).is_absolute() => Ok(root.to_string()),
            _ => Err(invalid()),
        })
        .collect()
}

fn realpath_or_none(path: &str) -> Option<std::path::PathBuf> {
    std::fs::canonicalize(path).ok()
}

/// The module's realpath when it is a granted file or lies inside a granted directory (both realpath'd); otherwise refused.
pub fn contained_module_path(
    module_path: &str,
    roots: &[String],
) -> Result<String, ToolChildJobError> {
    let Some(resolved) = realpath_or_none(module_path) else {
        return Err(ToolChildJobError::new(
            "offload_module_unavailable",
            format!("{module_path} does not exist"),
        ));
    };
    for root in roots {
        let Some(real_root) = realpath_or_none(root) else {
            continue;
        };
        // A granted file is granted exactly; a granted directory grants what is strictly inside it.
        if resolved == real_root && resolved.is_file() {
            return Ok(resolved.to_string_lossy().into_owned());
        }
        if let Ok(inside) = resolved.strip_prefix(&real_root)
            && inside.as_os_str() != ""
        {
            return Ok(resolved.to_string_lossy().into_owned());
        }
    }
    Err(ToolChildJobError::new(
        "module_job_outside_roots",
        format!("{module_path} is not inside an installed extension directory"),
    ))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct LockState {
    held: HashSet<String>,
    closed: bool,
}

/// The file locks one module job holds on the server. Lock and unlock calls outlive the job's cancel (the server cancels
/// a still-waiting lock when the job ends); a lock granted after the job closed is released at once, so none is held
/// by a job that can no longer release it.
pub struct JobFileLocks {
    host_call: HostCallFn,
    state: Mutex<LockState>,
}

impl JobFileLocks {
    pub fn new(host_call: HostCallFn) -> Self {
        Self {
            host_call,
            state: Mutex::new(LockState::default()),
        }
    }

    /// Records a granted lock; false when the job has closed and the lock was released instead. Call inside a runtime.
    pub fn granted(&self, lock_id: &str) -> bool {
        {
            let mut state = lock(&self.state);
            if !state.closed {
                state.held.insert(lock_id.to_string());
                return true;
            }
        }
        let unlocking = self.unlock(lock_id);
        tokio::spawn(async move {
            let _ = unlocking.await;
        });
        false
    }

    pub fn released(&self, lock_id: &str) {
        lock(&self.state).held.remove(lock_id);
    }

    /// After the worker has stopped: releases every lock it still holds.
    pub async fn release_all(&self) {
        let held: Vec<String> = {
            let mut state = lock(&self.state);
            state.closed = true;
            state.held.drain().collect()
        };
        // A release the server cannot take (the channel closed) is covered by its release on the child's exit.
        futures::future::join_all(held.iter().map(|lock_id| self.unlock(lock_id))).await;
    }

    fn unlock(
        &self,
        lock_id: &str,
    ) -> futures::future::BoxFuture<'static, Result<Value, ToolChildJobError>> {
        (self.host_call)(
            HostCallTarget {
                kind: HostCallKind::UnlockFile,
                name: None,
                args: Some(json!({ "lockId": lock_id })),
            },
            HostCallOptions {
                survives_cancel: true,
            },
        )
    }
}

/// A host call from the module's worker, forwarded over the reverse bridge; the reply goes back to that worker.
fn forward_host_call(
    host_call: &HostCallFn,
    locks: &Arc<JobFileLocks>,
    kind: HostCallKind,
    name: Option<String>,
    args: Value,
    reply: tokio::sync::oneshot::Sender<HostReply>,
) {
    let lock_call = matches!(kind, HostCallKind::LockFile | HostCallKind::UnlockFile);
    let call = host_call(
        HostCallTarget {
            kind,
            name,
            args: if args.is_null() {
                None
            } else {
                Some(args.clone())
            },
        },
        HostCallOptions {
            survives_cancel: lock_call,
        },
    );
    let locks = Arc::clone(locks);
    tokio::spawn(async move {
        match call.await {
            Ok(value) => {
                if kind == HostCallKind::LockFile
                    && let Some(lock_id) = value.get("lockId").and_then(Value::as_str)
                    && !locks.granted(lock_id)
                {
                    return;
                }
                if kind == HostCallKind::UnlockFile
                    && let Some(released) = args.get("lockId").and_then(Value::as_str)
                {
                    locks.released(released);
                }
                let _ = reply.send(Ok(value));
            }
            Err(error) => {
                let _ = reply.send(Err((error.code, error.message)));
            }
        }
    });
}

/// `createModuleJobHandler(env)`.
pub fn create_module_job_handler(env: ProcessEnv) -> ToolChildJobHandler {
    Arc::new(move |value: Value, context: ToolChildJobContext| {
        let env = env.clone();
        async move {
            run_module_job(&env, &value, context)
                .await
                .map_err(HandlerError::from)
        }
        .boxed()
    })
}

async fn run_module_job(
    env: &ProcessEnv,
    value: &Value,
    context: ToolChildJobContext,
) -> Result<Option<Value>, ToolChildJobError> {
    let job = parse_module_job_args(value)?;
    let module_path = contained_module_path(&job.module_path, &parse_module_roots(env)?)?;
    let (events_tx, mut events) = mpsc::unbounded_channel::<ModuleWorkerEvent>();
    let cancellation = AbortSignal::default();
    let data = ModuleWorkerData {
        module_path,
        export_name: job.export,
        args: job.args,
    };
    let body_cancellation = cancellation.clone();
    let worker = start_gated_worker(
        &context.gate,
        &context.job_id,
        env.clone(),
        move |barrier| run_module_worker(data, barrier, events_tx, body_cancellation),
    )
    .map_err(|error| ToolChildJobError::new("module_job_failed", error.to_string()))?;
    let worker = Arc::new(worker);
    // A cancel reaches the body as an aborted signal; a body that has not settled within the grace is ended with its
    // worker. Either way the job fails tool_child_cancelled and the owned groups it still runs are ended.
    let cancelled = Arc::new(AtomicBool::new(false));
    let locks = Arc::new(JobFileLocks::new(Arc::clone(&context.host_call)));
    let canceller = {
        let (signal, cancelled, worker) = (
            context.signal.clone(),
            Arc::clone(&cancelled),
            Arc::clone(&worker),
        );
        tokio::spawn(async move {
            signal.cancelled().await;
            cancelled.store(true, Ordering::SeqCst);
            cancellation.abort();
            tokio::time::sleep(Duration::from_millis(MODULE_CANCEL_GRACE_MS)).await;
            worker.terminate();
        })
    };
    let outcome: Result<Value, ToolChildJobError> = async {
        while let Some(event) = events.recv().await {
            match event {
                ModuleWorkerEvent::HostCall {
                    host_kind,
                    name,
                    args,
                    reply,
                    ..
                } => {
                    forward_host_call(&context.host_call, &locks, host_kind, name, args, reply);
                }
                ModuleWorkerEvent::ModuleResult { value } => return Ok(value),
                ModuleWorkerEvent::ModuleError { code, message } => {
                    return Err(ToolChildJobError::new(code, message));
                }
            }
        }
        let code = if worker.was_terminated() { 1 } else { 0 };
        Err(ToolChildJobError::new(
            "module_job_failed",
            format!("worker exited ({code})"),
        ))
    }
    .await;
    let outcome = match outcome {
        Err(_) if cancelled.load(Ordering::SeqCst) => Err(ToolChildJobError::new(
            "tool_child_cancelled",
            "the job was cancelled",
        )),
        other => other,
    };
    canceller.abort();
    // The worker is stopped before its locks are released: no write it started can still be running (drain first).
    worker.terminate();
    worker.finished().await;
    if cancelled.load(Ordering::SeqCst) {
        context.gate.end_job_groups(&context.job_id);
    }
    locks.release_all().await;
    outcome.map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn code_of<T: std::fmt::Debug>(result: Result<T, ToolChildJobError>) -> String {
        result.expect_err("an error").code
    }

    #[test]
    fn parses_job_args_and_refuses_malformed_ones() {
        let parsed =
            parse_module_job_args(&json!({ "modulePath": "/a/b.rs", "export": "x", "args": [1] }))
                .expect("valid");
        assert_eq!(
            parsed,
            ModuleJobArgs {
                module_path: "/a/b.rs".into(),
                export: "x".into(),
                args: json!([1])
            }
        );
        assert_eq!(
            code_of(parse_module_job_args(&json!(null))),
            "module_job_invalid"
        );
        assert_eq!(
            code_of(parse_module_job_args(
                &json!({ "modulePath": "rel", "export": "x" })
            )),
            "module_job_invalid"
        );
        assert_eq!(
            code_of(parse_module_job_args(
                &json!({ "modulePath": "/a", "export": "" })
            )),
            "module_job_invalid"
        );
    }

    #[test]
    fn parses_roots_from_the_environment() {
        let env = |raw: &str| -> ProcessEnv {
            HashMap::from([(TOOL_CHILD_MODULE_ROOTS_ENV.to_string(), raw.to_string())])
        };
        assert_eq!(
            parse_module_roots(&ProcessEnv::new()).expect("unset"),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_module_roots(&env("")).expect("empty"),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_module_roots(&env("[\"/a\",\"/b\"]")).expect("roots"),
            vec!["/a", "/b"]
        );
        assert_eq!(
            code_of(parse_module_roots(&env("[\"rel\"]"))),
            "module_job_invalid"
        );
        assert_eq!(
            code_of(parse_module_roots(&env("{}"))),
            "module_job_invalid"
        );
        assert_eq!(
            code_of(parse_module_roots(&env("nope"))),
            "tool_child_job_failed"
        );
    }

    #[test]
    fn accepts_a_file_inside_a_granted_root_and_refuses_one_outside() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let module = root.path().join("offload.rs");
        std::fs::write(&module, "x").expect("write");
        let stray = outside.path().join("stray.rs");
        std::fs::write(&stray, "x").expect("write");
        let roots = vec![root.path().to_string_lossy().into_owned()];
        let real = std::fs::canonicalize(&module)
            .expect("real")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            contained_module_path(&module.to_string_lossy(), &roots).expect("inside"),
            real
        );
        assert_eq!(
            code_of(contained_module_path(&stray.to_string_lossy(), &roots)),
            "module_job_outside_roots"
        );
        assert_eq!(
            code_of(contained_module_path(
                &root.path().to_string_lossy(),
                &roots
            )),
            "module_job_outside_roots"
        );
        assert_eq!(
            code_of(contained_module_path(
                &root.path().join("missing.rs").to_string_lossy(),
                &roots
            )),
            "offload_module_unavailable"
        );
        // A granted file is granted exactly.
        let file_root = vec![module.to_string_lossy().into_owned()];
        assert_eq!(
            contained_module_path(&module.to_string_lossy(), &file_root).expect("exact"),
            real
        );
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlinks_to_their_realpath() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let module = root.path().join("offload.rs");
        std::fs::write(&module, "x").expect("write");
        let stray = outside.path().join("stray.rs");
        std::fs::write(&stray, "x").expect("write");
        let roots = vec![root.path().to_string_lossy().into_owned()];
        // A link outside that points inside is contained (by its realpath).
        let link = outside.path().join("link.rs");
        std::os::unix::fs::symlink(&module, &link).expect("symlink");
        let real = std::fs::canonicalize(&module)
            .expect("real")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            contained_module_path(&link.to_string_lossy(), &roots).expect("linked"),
            real
        );
        // A linked directory pointing outside the root does not lend its files the root.
        let linked_root = outside.path().join("linked-root");
        std::os::unix::fs::symlink(outside.path(), &linked_root).expect("symlink");
        assert_eq!(
            code_of(contained_module_path(
                &linked_root.join("stray.rs").to_string_lossy(),
                &roots
            )),
            "module_job_outside_roots"
        );
    }

    type RecordedHostCalls = Arc<Mutex<Vec<(HostCallKind, Value, bool)>>>;

    fn recording_host_call() -> (HostCallFn, RecordedHostCalls) {
        let calls: RecordedHostCalls = Arc::default();
        let log = Arc::clone(&calls);
        let host_call: HostCallFn = Arc::new(move |target, options| {
            lock(&log).push((
                target.kind,
                target.args.clone().unwrap_or(Value::Null),
                options.survives_cancel,
            ));
            async { Ok(Value::Null) }.boxed()
        });
        (host_call, calls)
    }

    #[tokio::test]
    async fn release_all_unlocks_every_held_lock_surviving_cancel_and_later_grants_unlock_at_once()
    {
        let (host_call, calls) = recording_host_call();
        let locks = JobFileLocks::new(host_call);
        assert!(locks.granted("L1"));
        assert!(locks.granted("L2"));
        locks.released("L2");
        locks.release_all().await;
        assert_eq!(
            lock(&calls).clone(),
            vec![(HostCallKind::UnlockFile, json!({ "lockId": "L1" }), true)]
        );
        // Closed now: a lock granted after the job closed is released instead of held.
        assert!(!locks.granted("L3"));
        tokio::time::timeout(Duration::from_secs(30), async {
            while lock(&calls).len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("late unlock in time");
        assert_eq!(
            lock(&calls)[1],
            (HostCallKind::UnlockFile, json!({ "lockId": "L3" }), true)
        );
    }
}
