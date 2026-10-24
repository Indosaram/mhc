//! What a session's tool child can reach on the server (senpi#3566 todo 7, DESIGN PART 1): a call from a child-run tool
//! body. A `tool` call runs as a nested call of the tool call that owns the job - the same path as in-process
//! `ctx.executeTool`, hooks once - and `list_tools` names the tools such a call may reach. `authorize` asks the session's
//! filesystem policy (never cached: every call is decided again). `lock_file` takes the file's slot in the same mutation
//! queue the session's own file tools use, granted only to a live job; the slot is released only by `unlock_file` or
//! by the child's observed exit - never by a cancel, so a write still in flight finishes before anyone else's begins.
use crate::tool_dispatch::offload::BoxError;
use futures::{FutureExt, future::BoxFuture};
use maho_agent::types::AgentToolCallOutcome;
use maho_ext_api::AbortSignal;
use maho_tool_child::{
    host_calls_server::{HostCallHandler, HostCallRequest},
    protocol::HostCallKind,
    session_child::ToolChildHostContext,
};
use maho_tools::{
    FilesystemPolicyChecker, FilesystemPolicyDecision, FilesystemPolicyRequest,
    file_mutation_queue::{CancelSignal, FileMutationLock, acquire_file_mutation_lock},
    filesystem_policy::{FilesystemOperation, canonicalize_filesystem_path},
    path_utils::resolve_to_cwd,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

/// Runs `name` as a nested call of `parent_tool_call_id` through the session's tool pipeline.
pub type ExecuteNested = Arc<
    dyn Fn(
            String,
            String,
            Value,
            AbortSignal,
        ) -> BoxFuture<'static, Result<AgentToolCallOutcome, BoxError>>
        + Send
        + Sync,
>;

/// Serves one registered kind on the server; it runs with whatever server-side context its registrar holds.
pub type HostCallTarget =
    Arc<dyn Fn(HostCallTargetRequest) -> BoxFuture<'static, Result<Value, BoxError>> + Send + Sync>;

pub struct HostCallTargetRequest {
    pub name: Option<String>,
    pub args: Value,
    pub signal: AbortSignal,
}

#[derive(Clone)]
pub struct SessionHostCallDeps {
    /// The session cwd: a child's relative path names the same file the session's own file tools would.
    pub cwd: PathBuf,
    pub execute_nested: ExecuteNested,
    pub callable_tool_names: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// The `completion` / `reserved` targets registered on the session (codemode's runtime registers them).
    pub target: Arc<dyn Fn(RegisteredHostCallKind) -> Option<HostCallTarget> + Send + Sync>,
    /// The session's composed filesystem policy; `None` when no extension registered one.
    pub filesystem_policy: Arc<dyn Fn() -> Option<FilesystemPolicyChecker> + Send + Sync>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisteredHostCallKind {
    Completion,
    Reserved,
}

impl RegisteredHostCallKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completion => "completion",
            Self::Reserved => "reserved",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct HostCallError {
    pub code: String,
    /// The full message, `<code>: <detail>`.
    pub message: String,
}

impl HostCallError {
    pub fn new(code: impl Into<String>, message: impl AsRef<str>) -> Self {
        let code = code.into();
        Self {
            message: format!("{code}: {}", message.as_ref()),
            code,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn kind_name(kind: HostCallKind) -> &'static str {
    match kind {
        HostCallKind::Tool => "tool",
        HostCallKind::Completion => "completion",
        HostCallKind::Reserved => "reserved",
        HostCallKind::ListTools => "list_tools",
        HostCallKind::Authorize => "authorize",
        HostCallKind::LockFile => "lock_file",
        HostCallKind::UnlockFile => "unlock_file",
    }
}

type Locks = Arc<Mutex<HashMap<String, FileMutationLock>>>;

fn release_lock(locks: &Locks, lock_id: &str) -> bool {
    // Dropping the lock releases its slot.
    lock(locks).remove(lock_id).is_some()
}

pub fn create_session_host_call_handler(deps: SessionHostCallDeps) -> HostCallHandler {
    let locks: Locks = Arc::default();
    let next_lock = Arc::new(AtomicU64::new(0));
    Arc::new(move |request: HostCallRequest| {
        let (deps, locks, next_lock) = (deps.clone(), locks.clone(), next_lock.clone());
        async move {
            serve(deps, locks, next_lock, request)
                .await
                .map_err(|error| error.to_string())
        }
        .boxed()
    })
}

async fn serve(
    deps: SessionHostCallDeps,
    locks: Locks,
    next_lock: Arc<AtomicU64>,
    mut request: HostCallRequest,
) -> Result<Value, BoxError> {
    let args = request.args.take().unwrap_or(Value::Null);
    {
        {
            // A call of an ended job never runs (it could still spend tokens); only releasing a lock it holds is allowed.
            if request.kind != HostCallKind::UnlockFile && !(request.generation.job_live)() {
                return Err(HostCallError::new(
                    "host_call_job_ended",
                    format!("{}: the job has ended", kind_name(request.kind)),
                )
                .into());
            }
            match request.kind {
                HostCallKind::Tool => {
                    let Some(parent) = calling_tool_call_id(&request) else {
                        return Err(HostCallError::new(
                            "host_call_no_caller",
                            "the job runs for no tool call",
                        )
                        .into());
                    };
                    let Some(name) = request.name else {
                        return Err(HostCallError::new(
                            "host_call_no_name",
                            "a tool call names no tool",
                        )
                        .into());
                    };
                    let outcome = (deps.execute_nested)(parent, name, args, request.signal).await?;
                    Ok(json!({
                        "result": serde_json::to_value(&outcome.result)?,
                        "isError": outcome.is_error,
                    }))
                }
                HostCallKind::ListTools => Ok(json!((deps.callable_tool_names)())),
                HostCallKind::Completion | HostCallKind::Reserved => {
                    let registered = if request.kind == HostCallKind::Completion {
                        RegisteredHostCallKind::Completion
                    } else {
                        RegisteredHostCallKind::Reserved
                    };
                    let Some(target) = (deps.target)(registered) else {
                        return Err(HostCallError::new(
                            "host_call_unsupported_kind",
                            format!(
                                "no {} target is registered on this session",
                                registered.as_str()
                            ),
                        )
                        .into());
                    };
                    target(HostCallTargetRequest {
                        name: request.name,
                        args,
                        signal: request.signal,
                    })
                    .await
                }
                HostCallKind::Authorize => {
                    let policy_args = policy_request(&args, &deps.cwd).await?;
                    let decision = match (deps.filesystem_policy)() {
                        None => FilesystemPolicyDecision::Allow,
                        Some(policy) => policy(policy_args).await?,
                    };
                    Ok(match decision {
                        FilesystemPolicyDecision::Allow => json!({ "allow": true }),
                        FilesystemPolicyDecision::Deny { reason } => {
                            json!({ "allow": false, "reason": reason })
                        }
                    })
                }
                HostCallKind::LockFile => {
                    lock_file(&deps, &locks, &next_lock, &args, request).await
                }
                HostCallKind::UnlockFile => {
                    let lock_id = string_field(&args, "lockId")?;
                    if !release_lock(&locks, &lock_id) {
                        return Err(HostCallError::new(
                            "host_call_unknown_lock",
                            format!("no held lock {lock_id}"),
                        )
                        .into());
                    }
                    Ok(Value::Null)
                }
            }
        }
    }
}

async fn lock_file(
    deps: &SessionHostCallDeps,
    locks: &Locks,
    next_lock: &AtomicU64,
    args: &Value,
    request: HostCallRequest,
) -> Result<Value, BoxError> {
    let path = string_field(args, "path")?;
    // The wait is cancelled by the request's signal; the reason is empty, which the queue words as its own message.
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(None);
    let cancel: CancelSignal = cancel_rx;
    let watcher = {
        let signal = request.signal.clone();
        tokio::spawn(async move {
            signal.cancelled().await;
            cancel_tx.send_replace(Some(String::new()));
        })
    };
    let acquired =
        acquire_file_mutation_lock(&resolve_to_cwd(&path, &deps.cwd), Some(cancel)).await;
    watcher.abort();
    let release = acquired?;
    // Granted only to a live job: one that ended while it waited never holds the slot.
    if !(request.generation.job_live)() || request.signal.is_aborted() {
        release.release();
        return Err(HostCallError::new(
            "host_call_job_ended",
            format!("lock on {path}: the job ended while it waited"),
        )
        .into());
    }
    let lock_id = format!("L{}", next_lock.fetch_add(1, Ordering::SeqCst) + 1);
    lock(locks).insert(lock_id.clone(), release);
    {
        let (locks, lock_id, exited) = (
            locks.clone(),
            lock_id.clone(),
            request.generation.exited.clone(),
        );
        tokio::spawn(async move {
            exited.await;
            release_lock(&locks, &lock_id);
        });
    }
    if let Some(stuck) = request.generation.stuck.clone() {
        let (locks, lock_id) = (locks.clone(), lock_id.clone());
        tokio::spawn(async move {
            let holder = stuck.await;
            if let Some(held) = lock(&locks).get(&lock_id) {
                held.mark_stuck(&holder);
            }
        });
    }
    Ok(json!({ "lockId": lock_id }))
}

fn string_field(args: &Value, field: &str) -> Result<String, HostCallError> {
    match args.get(field).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_owned()),
        _ => Err(HostCallError::new(
            "host_call_bad_args",
            format!("{field} must be a non-empty string"),
        )),
    }
}

/// The server canonicalizes the child's path the way the built-in file tools do; a child-sent canonical path is never trusted.
async fn policy_request(args: &Value, cwd: &Path) -> Result<FilesystemPolicyRequest, BoxError> {
    let operation = string_field(args, "operation")?;
    let operation = match operation.as_str() {
        "read" => FilesystemOperation::Read,
        "enumerate" => FilesystemOperation::Enumerate,
        "write" => FilesystemOperation::Write,
        _ => {
            return Err(HostCallError::new(
                "host_call_bad_args",
                "operation must be read, enumerate or write",
            )
            .into());
        }
    };
    Ok(FilesystemPolicyRequest {
        operation,
        canonical_path: canonicalize_filesystem_path(&resolve_to_cwd(
            &string_field(args, "path")?,
            cwd,
        ))
        .await?,
        tool_name: string_field(args, "toolName")?,
    })
}

fn calling_tool_call_id(request: &HostCallRequest) -> Option<String> {
    let context = request
        .context
        .as_ref()?
        .downcast_ref::<ToolChildHostContext>()?;
    context.tool_call_id.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_tool_child::host_calls_server::HostCallGeneration;
    use std::time::Duration;

    const BOUND: Duration = Duration::from_secs(5);

    fn deps(cwd: &Path) -> SessionHostCallDeps {
        SessionHostCallDeps {
            cwd: cwd.to_path_buf(),
            execute_nested: Arc::new(|_, _, _, _| async { Err("no nested calls".into()) }.boxed()),
            callable_tool_names: Arc::new(|| vec!["read".to_owned(), "grep".to_owned()]),
            target: Arc::new(|_| None),
            filesystem_policy: Arc::new(|| None),
        }
    }

    fn request(kind: HostCallKind, args: Value, live: bool) -> HostCallRequest {
        HostCallRequest {
            call_id: "c1".into(),
            job_id: "j1".into(),
            kind,
            name: None,
            args: (!args.is_null()).then_some(args),
            signal: AbortSignal::default(),
            context: None,
            generation: HostCallGeneration {
                exited: std::future::pending::<()>().boxed().shared(),
                stuck: None,
                job_live: Arc::new(move || live),
            },
        }
    }

    async fn call(handler: &HostCallHandler, request: HostCallRequest) -> Result<Value, String> {
        tokio::time::timeout(BOUND, handler(request))
            .await
            .expect("bounded")
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn list_tools_names_the_callable_tools_and_a_dead_job_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handler = create_session_host_call_handler(deps(dir.path()));
        assert_eq!(
            call(
                &handler,
                request(HostCallKind::ListTools, Value::Null, true)
            )
            .await,
            Ok(json!(["read", "grep"]))
        );
        assert_eq!(
            call(
                &handler,
                request(HostCallKind::ListTools, Value::Null, false)
            )
            .await,
            Err("host_call_job_ended: list_tools: the job has ended".to_owned())
        );
    }

    #[tokio::test]
    async fn tool_calls_need_a_caller_and_a_name_and_unregistered_targets_are_unsupported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handler = create_session_host_call_handler(deps(dir.path()));
        assert_eq!(
            call(&handler, request(HostCallKind::Tool, Value::Null, true)).await,
            Err("host_call_no_caller: the job runs for no tool call".to_owned())
        );
        let mut nameless = request(HostCallKind::Tool, Value::Null, true);
        nameless.context = Some(Arc::new(ToolChildHostContext {
            tool_call_id: Some("t1".into()),
        }));
        assert_eq!(
            call(&handler, nameless).await,
            Err("host_call_no_name: a tool call names no tool".to_owned())
        );
        assert_eq!(
            call(
                &handler,
                request(HostCallKind::Completion, Value::Null, true)
            )
            .await,
            Err(
                "host_call_unsupported_kind: no completion target is registered on this session"
                    .to_owned()
            )
        );
    }

    #[tokio::test]
    async fn authorize_allows_without_a_policy_and_rejects_a_bad_operation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handler = create_session_host_call_handler(deps(dir.path()));
        let ok = json!({ "operation": "read", "path": "a.txt", "toolName": "t" });
        assert_eq!(
            call(&handler, request(HostCallKind::Authorize, ok, true)).await,
            Ok(json!({ "allow": true }))
        );
        let bad = json!({ "operation": "delete", "path": "a.txt", "toolName": "t" });
        assert_eq!(
            call(&handler, request(HostCallKind::Authorize, bad, true)).await,
            Err("host_call_bad_args: operation must be read, enumerate or write".to_owned())
        );
    }

    #[tokio::test]
    async fn a_lock_is_granted_once_and_released_only_by_unlock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handler = create_session_host_call_handler(deps(dir.path()));
        let granted = call(
            &handler,
            request(HostCallKind::LockFile, json!({ "path": "a.txt" }), true),
        )
        .await
        .expect("granted");
        let lock_id = granted["lockId"].as_str().expect("lock id").to_owned();
        assert_eq!(
            call(
                &handler,
                request(HostCallKind::UnlockFile, json!({ "lockId": lock_id }), true)
            )
            .await,
            Ok(Value::Null)
        );
        assert_eq!(
            call(
                &handler,
                request(HostCallKind::UnlockFile, json!({ "lockId": lock_id }), true)
            )
            .await,
            Err(format!("host_call_unknown_lock: no held lock {lock_id}"))
        );
    }
}
