//! The host calls of an in-process module (senpi#3622), answered by the session's host-call handler under the rules the
//! child's `ServerHostCalls` applies, so a module behaves the same on both runtimes.
use super::offload::{BoxError, OffloadError, OffloadErrorCode};
use super::tool_dispatcher::ToolChildModuleJob;
use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use maho_ext_api::AbortSignal;
use maho_tool_child::{
    admission::{HostCallAdmission, HostCallDecision},
    host_calls_server::{HostCallGeneration, HostCallHandler, HostCallRequest},
    module_host::{ModuleHost, ModuleHostSend, create_module_host},
    protocol::{HostCallKind, ServerToChild, ToolChildMessage},
    session_child::ToolChildHostContext,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    future::Future,
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::{oneshot, watch};

/// A call a module makes in-process, admitted like the child's (`HostCallAdmission`): running, queued FIFO, or refused.
struct InProcessCall {
    abort: AbortSignal,
    start: Option<Box<dyn FnOnce() + Send>>,
}

/// A refused or unrun in-process call: the same codes a child's caller reads from `.code`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct InProcessHostCallError {
    pub code: String,
    /// The full message, `<code>: <detail>`.
    pub message: String,
}

impl InProcessHostCallError {
    pub fn new(code: impl Into<String>, message: impl AsRef<str>) -> Self {
        let code = code.into();
        Self {
            message: format!("{code}: {}", message.as_ref()),
            code,
        }
    }
}

struct InProcessShared {
    admitting: AtomicBool,
    closed: AtomicBool,
    ended: AtomicBool,
    exited: watch::Sender<bool>,
    stuck: watch::Sender<Option<String>>,
    exited_future: Shared<BoxFuture<'static, ()>>,
    stuck_future: Shared<BoxFuture<'static, String>>,
    admission: Mutex<HostCallAdmission>,
    calls: Mutex<HashMap<String, InProcessCall>>,
    /// `lockId -> path`, in the order the locks were granted.
    held: Mutex<Vec<(String, String)>>,
    next: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct InProcessHostCalls {
    pub host: ModuleHost,
    shared: Arc<InProcessShared>,
}

impl InProcessHostCalls {
    pub fn stop_admitting(&self) {
        self.shared.admitting.store(false, Ordering::SeqCst);
    }

    /// The session closed: every further call is refused `host_call_session_closed`, and calls running are signalled.
    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.admitting.store(false, Ordering::SeqCst);
        self.abort_calls();
    }

    /// Ends the job: running calls are signalled and every lock it holds is released.
    pub fn end(&self) {
        if self.shared.ended.swap(true, Ordering::SeqCst) {
            return;
        }
        lock(&self.shared.held).clear();
        self.abort_calls();
        self.shared.exited.send_replace(true);
    }

    pub fn ended(&self) -> bool {
        self.shared.ended.load(Ordering::SeqCst)
    }

    pub fn held_files(&self) -> Vec<String> {
        let mut files: Vec<String> = Vec::new();
        for (_, path) in lock(&self.shared.held).iter() {
            if !files.contains(path) {
                files.push(path.clone());
            }
        }
        files
    }

    pub fn mark_stuck(&self, holder: &str) {
        self.shared.stuck.send_replace(Some(holder.to_owned()));
    }

    fn abort_calls(&self) {
        let aborts: Vec<AbortSignal> = lock(&self.shared.calls)
            .values()
            .map(|call| call.abort.clone())
            .collect();
        for abort in aborts {
            abort.abort();
        }
    }
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

/// The host an in-process module gets, its calls answered by the session's host-call handler under the same rules the
/// child's `ServerHostCalls` applies: one admission per job, the job's cancel signalled into every call except the lock
/// calls (a lock is released after the writes stop, not when the cancel arrives), and every call still running signalled
/// when the job ends. `end` resolves the generation's `exited`, which releases the locks the job still holds;
/// `mark_stuck` resolves its `stuck`, which keeps them held and fails every waiter on them.
pub fn in_process_host_calls(
    job: &ToolChildModuleJob,
    cancelled: AbortSignal,
    host_calls: Option<HostCallHandler>,
) -> InProcessHostCalls {
    let exited = watch::channel(false).0;
    let stuck = watch::channel(None).0;
    let exited_future = {
        let mut receiver = exited.subscribe();
        async move {
            let _ = receiver.wait_for(|done| *done).await;
        }
        .boxed()
        .shared()
    };
    let stuck_future = {
        let mut receiver = stuck.subscribe();
        async move {
            let holder = match receiver.wait_for(|holder| holder.is_some()).await {
                Ok(holder) => Some((*holder).clone().unwrap_or_default()),
                Err(_) => None,
            };
            match holder {
                Some(holder) => holder,
                None => std::future::pending().await,
            }
        }
        .boxed()
        .shared()
    };
    let shared = Arc::new(InProcessShared {
        admitting: AtomicBool::new(true),
        closed: AtomicBool::new(false),
        ended: AtomicBool::new(false),
        exited,
        stuck,
        exited_future,
        stuck_future,
        admission: Mutex::new(HostCallAdmission::new()),
        calls: Mutex::default(),
        held: Mutex::default(),
        next: AtomicU64::new(0),
    });
    let job_id = format!("in-process:{}#{}", job.module_path, job.export);
    let tool_call_id = calling_tool_call_id(&job.args);
    let send: ModuleHostSend = {
        let shared = shared.clone();
        let cancelled = cancelled.clone();
        Arc::new(move |kind, name, args| {
            let (shared, cancelled) = (shared.clone(), cancelled.clone());
            let (host_calls, job_id, tool_call_id) =
                (host_calls.clone(), job_id.clone(), tool_call_id.clone());
            async move {
                let value = answer(
                    &shared,
                    &cancelled,
                    host_calls,
                    &job_id,
                    tool_call_id,
                    kind,
                    name,
                    args.clone(),
                )
                .await?;
                // Only an answered unlock lets go of the file: a refused one leaves it held, and named in a stuck notice.
                if kind == HostCallKind::UnlockFile
                    && let Some(lock_id) = string_of(&args, "lockId")
                {
                    lock(&shared.held).retain(|(held, _)| *held != lock_id);
                }
                if kind == HostCallKind::LockFile
                    && let (Some(lock_id), Some(path)) =
                        (string_of(&value, "lockId"), string_of(&args, "path"))
                {
                    let mut held = lock(&shared.held);
                    held.retain(|(entry, _)| *entry != lock_id);
                    held.push((lock_id, path));
                }
                Ok(value)
            }
            .boxed()
        })
    };
    let host = create_module_host(cancelled, send);
    InProcessHostCalls { host, shared }
}

#[allow(clippy::too_many_arguments)]
async fn answer(
    shared: &Arc<InProcessShared>,
    cancelled: &AbortSignal,
    host_calls: Option<HostCallHandler>,
    job_id: &str,
    tool_call_id: Option<String>,
    kind: HostCallKind,
    name: Option<String>,
    args: Value,
) -> Result<Value, BoxError> {
    let label = name.clone().unwrap_or_else(|| kind_name(kind).to_owned());
    let Some(handler) = host_calls else {
        return Err(OffloadError::new(
            OffloadErrorCode::ToolRuntimeUnavailable,
            format!("{}: this session answers no host calls", kind_name(kind)),
        )
        .into());
    };
    if shared.closed.load(Ordering::SeqCst) {
        return Err(InProcessHostCallError::new(
            "host_call_session_closed",
            format!("{label}: the session was closed"),
        )
        .into());
    }
    let call_id = format!("ip{}", shared.next.fetch_add(1, Ordering::SeqCst) + 1);
    let abort = AbortSignal::default();
    let survives_cancel = matches!(kind, HostCallKind::LockFile | HostCallKind::UnlockFile);
    let forwarder: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> = Arc::default();
    let (reply, answered) = oneshot::channel::<Result<Value, BoxError>>();
    let start: Box<dyn FnOnce() + Send> = {
        let (shared, abort, call_id, job_id, label, forwarder_done) = (
            shared.clone(),
            abort.clone(),
            call_id.clone(),
            job_id.to_owned(),
            label.clone(),
            forwarder.clone(),
        );
        Box::new(move || {
            let outcome: BoxFuture<'static, Result<Value, BoxError>> = if abort.is_aborted() {
                async move {
                    Err(InProcessHostCallError::new(
                        "host_call_job_ended",
                        format!("{label}: the job ended before this call ran"),
                    )
                    .into())
                }
                .boxed()
            } else {
                let live = shared.clone();
                let request = HostCallRequest {
                    call_id: call_id.clone(),
                    job_id,
                    kind,
                    name,
                    args: (!args.is_null()).then_some(args),
                    signal: abort.clone(),
                    context: Some(Arc::new(ToolChildHostContext { tool_call_id })),
                    generation: HostCallGeneration {
                        exited: shared.exited_future.clone(),
                        stuck: Some(shared.stuck_future.clone()),
                        job_live: Arc::new(move || {
                            live.admitting.load(Ordering::SeqCst)
                                && !live.ended.load(Ordering::SeqCst)
                        }),
                    },
                };
                settle_now(move || async move { handler(request).await.map_err(BoxError::from) })
            };
            tokio::spawn(async move {
                let result = outcome.await;
                let _ = reply.send(result);
                lock(&shared.calls).remove(&call_id);
                if let Some(forwarder) = lock(&forwarder_done).take() {
                    forwarder.abort();
                }
                let promoted = lock(&shared.admission).settle(&call_id);
                if let Some(promoted) = promoted {
                    let start = lock(&shared.calls)
                        .get_mut(&promoted)
                        .and_then(|call| call.start.take());
                    if let Some(start) = start {
                        start();
                    }
                }
            });
        })
    };
    // Submit and register under one lock: a promotion can only find the call once it is registered.
    let abort_call = abort.clone();
    let begin = {
        let mut calls = lock(&shared.calls);
        let decision = lock(&shared.admission).submit(&call_id);
        match decision {
            HostCallDecision::Refused { frame } => Err(frame),
            HostCallDecision::Active => {
                calls.insert(call_id.clone(), InProcessCall { abort, start: None });
                Ok(Some(start))
            }
            HostCallDecision::Waiting => {
                calls.insert(
                    call_id.clone(),
                    InProcessCall {
                        abort,
                        start: Some(start),
                    },
                );
                Ok(None)
            }
        }
    };
    match begin {
        Err(frame) => {
            abort_call.abort();
            let code = match frame.as_ref() {
                ToolChildMessage::Server(ServerToChild::HostCallRefused { code, .. }) => {
                    code.clone()
                }
                _ => "host_call_capacity".to_owned(),
            };
            return Err(InProcessHostCallError::new(code, format!("{label}: refused")).into());
        }
        Ok(started) => {
            if !survives_cancel {
                if cancelled.is_aborted() {
                    abort_call.abort();
                } else {
                    let (cancelled, abort_call) = (cancelled.clone(), abort_call.clone());
                    *lock(&forwarder) = Some(tokio::spawn(async move {
                        cancelled.cancelled().await;
                        abort_call.abort();
                    }));
                }
            }
            if let Some(start) = started {
                start();
            }
        }
    }
    match answered.await {
        Ok(result) => result,
        Err(_) => Err(InProcessHostCallError::new(
            "host_call_job_ended",
            format!("{label}: the job ended before this call ran"),
        )
        .into()),
    }
}

fn string_of(value: &Value, field: &str) -> Option<String> {
    value.get(field)?.as_str().map(str::to_owned)
}

/// Calls `run` now; a synchronous panic becomes a rejection like an async one.
pub fn settle_now<T, F>(run: impl FnOnce() -> F) -> BoxFuture<'static, Result<T, BoxError>>
where
    T: Send + 'static,
    F: Future<Output = Result<T, BoxError>> + Send + 'static,
{
    match std::panic::catch_unwind(AssertUnwindSafe(run)) {
        Ok(future) => future.boxed(),
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panicked".to_owned());
            async move { Err(message.into()) }.boxed()
        }
    }
}

fn calling_tool_call_id(args: &Value) -> Option<String> {
    string_of(args, "toolCallId")
}
